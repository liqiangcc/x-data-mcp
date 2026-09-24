//! NM-101 oracle: emit observed outputs of x_data_mcp::official_market's
//! S11b decision layer — TdxRangeProvider (probe/probe_facts/acquire/
//! acquire_typed), TdxBackfillProvider/TdxRepublishProvider
//! (acquire_merged_segment), TdxGapVerifier::verify, TdxDailyRunner
//! (run_item/run_items over a scripted repository) — for differential
//! verification against src_node/official/runner.mjs.
//!
//! usage: oracle_runner <manifest-path> <snapshot-root> <now-rfc3339> <scenario-json>
//! Scenario ops (probe/probe_facts/probe_bound return a private
//! `Continuity` — observable only through the acquire surfaces'
//! evidence.continuity, which is what these ops compare):
//!   {"op":"acquire","base":<raw>,"range":{"start","end"},"candidate_id":..,"published_at":..}
//!   {"op":"acquire_typed","facts":..,"base_window":[bar..],"range":..,"published_at":..}
//!   {"op":"acquire_merged","provider":"backfill"|"republish","base":<raw>,"range":..,"candidate_id":..,"published_at":..}
//!   {"op":"gap_verify","target":{"symbol","start_date","end_date"},"subject":{"version_id","integrity_sha256"},"records":{version_id:<raw>},"observed_at":..}
//!   {"op":"run_item","task_id":..,"item_id":..,"published_at":..,"repo":<script>}
//!   {"op":"run_items","task_id":..,"item_ids":[..],"generation_id":..,"published_at":..,"repo":<script>}
//! Each line: {"id","observed"}; observed = {"status":"ok","value":..} or
//! {"status":"error","kind":..,"error":"{Display}"}. A trailing
//! {"id":"trace","ops":[..]} line records the scripted repository call
//! order for decision-order comparison.
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::env;
use std::path::Path;
use x_data_mcp::market_data::{Bar, SourceError};
use x_data_mcp::market_storage::compat::{check as compat_check, CheckedRecord};
use x_data_mcp::market_storage::{
    Error as MError, MarketRepository, PublishPlan, ReadPage, Range, Receipt,
    Scope, FixedFacts,
};
use x_data_mcp::market_update::{DateRange, UpdateBaseFacts};
use x_data_mcp::official_market::{
    CollectionCandidate, TdxBackfillProvider,
    TdxDailyRunner, TdxGapVerifier, TdxMarketSource, TdxRangeProvider,
    TdxRepublishProvider,
};
use x_data_mcp::market_update::runner::{
    Acquisition, ItemExecution, RangeProvider, TypedAcquisition, TypedDecision,
};
use x_data_mcp::task_control::{
    Change, ItemBinding, ItemCounts, ItemOutcome, TaskHead, TaskItem, TaskSnapshot,
};
use chrono::{DateTime, NaiveDate, Utc};

fn emit(id: &str, observed: Value) {
    println!("{}", serde_json::to_string(&json!({"id": id, "observed": observed})).unwrap());
}
fn ok(v: Value) -> Value {
    json!({"status": "ok", "value": v})
}
fn src_kind(e: &SourceError) -> &'static str {
    match e {
        SourceError::NotVerified(_) => "not_verified",
        SourceError::Unsupported(_) => "unsupported",
        SourceError::Invocation(_) => "invocation",
        SourceError::InvalidResponse(_) => "invalid_response",
        SourceError::Timeout { .. } => "timeout",
    }
}
fn m_kind(e: &MError) -> String {
    match e {
        MError::Storage(_) => "storage".into(),
        MError::Integrity(_) => "integrity".into(),
        MError::Base(_) => "base".into(),
        MError::Conflict(_) => "conflict".into(),
        MError::NotFound => "not_found".into(),
        MError::Invalid(_) => "invalid".into(),
        MError::OutcomeUnknown(_) => "outcome_unknown".into(),
        MError::BasisNotVerified(_) => "basis_not_verified".into(),
        MError::Task(t) => format!("task:{}", format!("{t:?}").split('(').next().unwrap_or("?")),
    }
}
fn serr(e: SourceError) -> Value {
    json!({"status": "error", "kind": src_kind(&e), "error": format!("{e}")})
}
fn merr(e: MError) -> Value {
    json!({"status": "error", "kind": m_kind(&e), "error": format!("{e}")})
}

fn nd(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}
fn ts(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}
fn range_of(v: &Value) -> DateRange {
    DateRange { start: nd(v["start"].as_str().unwrap()), end: nd(v["end"].as_str().unwrap()) }
}
fn facts_of(v: &Value) -> UpdateBaseFacts {
    UpdateBaseFacts {
        version_id: v["version_id"].as_str().unwrap().into(),
        symbol: v["symbol"].as_str().unwrap().into(),
        record_count: v["record_count"].as_u64().unwrap() as u32,
        first_day: nd(v["first_day"].as_str().unwrap()),
        last_day: nd(v["last_day"].as_str().unwrap()),
        integrity_sha256: v["integrity_sha256"].as_str().unwrap().into(),
        coverage: serde_json::from_value(v["coverage"].clone()).unwrap(),
        unit_currency: serde_json::from_value(v["unit_currency"].clone()).unwrap(),
        source: v["source"].as_str().unwrap().into(),
        generation_id: v["generation_id"].as_str().unwrap().into(),
        generation_manifest_sha256: v["generation_manifest_sha256"].as_str().unwrap().into(),
        day_member_sha256: v["day_member_sha256"].as_str().unwrap().into(),
        security_event_sha256: v["security_event_sha256"].as_str().unwrap().into(),
    }
}
fn base_of(v: &Value) -> Result<CheckedRecord, MError> {
    compat_check(v)
}


fn acquisition_json(a: &Acquisition) -> Value {
    json!({
        "batch": serde_json::to_value(&a.batch).unwrap(),
        "evidence": serde_json::to_value(&a.evidence).unwrap(),
        "combined": a.combined.as_ref().map(|c| json!({
            "record": c.record,
            "evidence": serde_json::to_value(&c.evidence).unwrap(),
        })),
    })
}
fn typed_json(t: &Option<TypedAcquisition>) -> Value {
    match t {
        None => Value::Null,
        Some(t) => json!({
            "decision": match &t.decision {
                TypedDecision::Unchanged => json!("Unchanged"),
                TypedDecision::Append { upserts } => json!({"Append": {"upserts": upserts}}),
            },
            "evidence": serde_json::to_value(&t.evidence).unwrap(),
        }),
    }
}

// ------------------------------------------------------------------
// Scripted repository: every method serves canned JSON + records the
// call in an op trace. Transition effects are scenario-declared partial
// updates applied to the caller-held head/item (the runner's decision
// order is the unit under test; decide_head itself is task_control).
#[derive(Default)]
struct RepoState {
    ops: RefCell<Vec<String>>,
    bindings: RefCell<HashMap<String, VecDeque<Value>>>,
    receipts: RefCell<HashMap<String, VecDeque<Value>>>,
    transitions: RefCell<VecDeque<Value>>,
    heads: HashMap<String, Value>,
    tasks: HashMap<String, Value>,
    scopes: HashMap<String, Value>,
    records: HashMap<String, Value>,
    staged: RefCell<HashMap<String, Value>>,
    mark_validated_err: Option<String>,
    stage_err: Option<String>,
    commit_result: Value,
    collection_batch: Value,
}
// Publisher::prepare clones the repository — clones share the script
// state (queues, trace, staged records).
#[derive(Clone, Default)]
struct ScriptedRepo {
    inner: std::rc::Rc<RepoState>,
}
impl std::ops::Deref for ScriptedRepo {
    type Target = RepoState;
    fn deref(&self) -> &RepoState { &self.inner }
}
impl ScriptedRepo {
    fn pop(q: &RefCell<HashMap<String, VecDeque<Value>>>, key: &str) -> Option<Value> {
        let mut m = q.borrow_mut();
        let dq = m.get_mut(key)?;
        if dq.len() > 1 { dq.pop_front() } else { dq.front().cloned() }
    }
    fn parse_head(v: &Value) -> TaskHead {
        TaskHead {
            task_id: v["task_id"].as_str().unwrap().into(),
            key: serde_json::from_value(v["key"].clone()).unwrap(),
            status: serde_json::from_value(v["status"].clone()).unwrap(),
            state_revision: v["state_revision"].as_u64().unwrap(),
            accepted_run_id: v["accepted_run_id"].as_str().unwrap().into(),
            owner_run_id: v["owner_run_id"].as_str().map(str::to_string),
            last_progress_at: ts(v["last_progress_at"].as_str().unwrap()),
            result: if v["result"].is_null() { None } else { Some(v["result"].clone()) },
            counts: ItemCounts {
                total: v["counts"]["total"].as_u64().unwrap(),
                pending: v["counts"]["pending"].as_u64().unwrap(),
                running: v["counts"]["running"].as_u64().unwrap(),
                finished: v["counts"]["finished"].as_u64().unwrap(),
                succeeded: v["counts"]["succeeded"].as_u64().unwrap(),
            },
            evidence_digest: v["evidence_digest"].as_str().map(str::to_string),
        }
    }
    fn patch_head(h: &mut TaskHead, p: &Value) {
        if let Some(v) = p.get("status") { h.status = serde_json::from_value(v.clone()).unwrap(); }
        if let Some(v) = p.get("state_revision") { h.state_revision = v.as_u64().unwrap(); }
        if let Some(v) = p.get("accepted_run_id") { h.accepted_run_id = v.as_str().unwrap().into(); }
        if p.get("owner_run_id").is_some() {
            h.owner_run_id = p["owner_run_id"].as_str().map(str::to_string);
        }
        if let Some(v) = p.get("last_progress_at") { h.last_progress_at = ts(v.as_str().unwrap()); }
        if p.get("result").is_some() {
            h.result = if p["result"].is_null() { None } else { Some(p["result"].clone()) };
        }
        if let Some(c) = p.get("counts") {
            for f in ["total", "pending", "running", "finished", "succeeded"] {
                if let Some(x) = c.get(f) {
                    let n = x.as_u64().unwrap();
                    match f {
                        "total" => h.counts.total = n,
                        "pending" => h.counts.pending = n,
                        "running" => h.counts.running = n,
                        "finished" => h.counts.finished = n,
                        _ => h.counts.succeeded = n,
                    }
                }
            }
        }
    }
    fn patch_item(i: &mut TaskItem, p: &Value) {
        if let Some(v) = p.get("stage") { i.stage = serde_json::from_value(v.clone()).unwrap(); }
        if p.get("outcome").is_some() {
            i.outcome = serde_json::from_value::<Option<ItemOutcome>>(p["outcome"].clone()).unwrap();
        }
    }
    fn script_err(v: &Value) -> MError {
        let msg = v["err"].as_str().unwrap().to_string();
        match v["err_kind"].as_str().unwrap_or("storage") {
            "invalid" => MError::Invalid(msg),
            "integrity" => MError::Integrity(msg),
            "conflict" => MError::Conflict(msg),
            "outcome_unknown" => MError::OutcomeUnknown(msg),
            "not_found" => MError::NotFound,
            "task_needs_verification" =>
                MError::Task(x_data_mcp::task_control::Error::NeedsVerification(msg)),
            "task_unavailable" =>
                MError::Task(x_data_mcp::task_control::Error::StorageUnavailable(msg)),
            "task_outcome_unknown" =>
                MError::Task(x_data_mcp::task_control::Error::OutcomeUnknown(msg)),
            _ => MError::Storage(msg),
        }
    }
    fn change_tag(c: &Change) -> String {
        match c {
            Change::Claim => "claim".into(),
            Change::RequestCancel => "requestCancel".into(),
            Change::StartItem(id) => format!("startItem:{id}"),
            Change::FinishItem(id, o) =>
                format!("finishItem:{id}:{}:{}", o.success, o.error.clone().unwrap_or_default()),
            Change::FinishExecution => "finishExecution".into(),
            Change::FinishCancellation => "finishCancellation".into(),
            Change::Reconcile => "reconcile".into(),
            Change::RequireVerification { item_id, reason } =>
                format!("requireVerification:{item_id}:{reason}"),
        }
    }
}
impl MarketRepository for ScriptedRepo {
    fn stage(&self, raw: &Value) -> Result<(), MError> {
        self.ops.borrow_mut().push(format!(
            "stage:{}", raw["integrity_sha256"].as_str().unwrap_or("")));
        if let Some(m) = &self.stage_err {
            return Err(MError::Storage(m.clone()));
        }
        self.staged.borrow_mut()
            .insert(raw["version_id"].as_str().unwrap().to_string(), raw.clone());
        Ok(())
    }
    fn candidate(&self, id: &str) -> Result<CheckedRecord, MError> {
        self.ops.borrow_mut().push(format!("candidate:{id}"));
        let staged = self.staged.borrow().get(id).cloned();
        match staged.or_else(|| self.records.get(id).cloned()) {
            Some(v) => compat_check(&v),
            None => Err(MError::Integrity("record must be object".into())),
        }
    }
    fn mark_validated(&self, record: &CheckedRecord) -> Result<(), MError> {
        self.ops.borrow_mut()
            .push(format!("mark_validated:{}", record.version().version_id));
        match &self.mark_validated_err {
            Some(m) => Err(MError::Storage(m.clone())),
            None => Ok(()),
        }
    }
    fn fixed(&self, id: &str) -> Result<CheckedRecord, MError> {
        self.ops.borrow_mut().push(format!("fixed:{id}"));
        match self.records.get(id) {
            Some(v) => compat_check(v),
            None => Err(MError::NotFound),
        }
    }
    fn fixed_many(&self, _ids: &[String]) -> Result<Vec<CheckedRecord>, MError> {
        self.ops.borrow_mut().push("fixed_many".into());
        Err(MError::Storage("seam".into()))
    }
    fn fixed_facts(&self, id: &str) -> Result<FixedFacts, MError> {
        self.ops.borrow_mut().push(format!("fixed_facts:{id}"));
        Err(MError::Storage("seam".into()))
    }
    fn fixed_window(&self, id: &str, _f: NaiveDate, _l: NaiveDate) -> Result<CheckedRecord, MError> {
        self.ops.borrow_mut().push(format!("fixed_window:{id}"));
        Err(MError::Storage("seam".into()))
    }
    fn fixed_window_years(&self, _id: &str, _f: NaiveDate, _l: NaiveDate)
        -> Result<std::collections::BTreeSet<i32>, MError> {
        self.ops.borrow_mut().push("fixed_window_years".into());
        Err(MError::Storage("seam".into()))
    }
    fn task(&self, id: &str) -> Result<TaskSnapshot, MError> {
        self.ops.borrow_mut().push(format!("task:{id}"));
        match self.tasks.get(id) {
            Some(v) => Ok(serde_json::from_value(v.clone()).unwrap()),
            None => Err(MError::Task(x_data_mcp::task_control::Error::TaskNotFound)),
        }
    }
    fn task_head(&self, id: &str) -> Result<TaskHead, MError> {
        self.ops.borrow_mut().push(format!("task_head:{id}"));
        match self.heads.get(id) {
            Some(v) => Ok(Self::parse_head(v)),
            None => Err(MError::Task(x_data_mcp::task_control::Error::TaskNotFound)),
        }
    }
    fn task_item_binding(&self, task: &str, item: &str) -> Result<Option<ItemBinding>, MError> {
        self.ops.borrow_mut().push(format!("task_item_binding:{task}:{item}"));
        match Self::pop(&self.bindings, item) {
            None => Ok(None),
            Some(v) => {
                if v.get("err").is_some() { return Err(Self::script_err(&v)); }
                Ok(Some(ItemBinding {
                    head: Self::parse_head(&v["head"]),
                    item: serde_json::from_value::<TaskItem>(v["item"].clone()).unwrap(),
                    scopes_bound: v["scopes_bound"].as_bool().unwrap_or(true),
                }))
            }
        }
    }
    fn transition_task_head(
        &self,
        head: &mut TaskHead,
        item: Option<&mut TaskItem>,
        run: &str,
        change: Change,
    ) -> Result<(), MError> {
        self.ops.borrow_mut()
            .push(format!("transition_task_head:{run}:{}", Self::change_tag(&change)));
        let mut q = self.transitions.borrow_mut();
        let effect = if q.len() > 1 { q.pop_front() } else { q.front().cloned() };
        drop(q);
        match effect {
            None => Ok(()),
            Some(v) => {
                if v.get("err").is_some() { return Err(Self::script_err(&v)); }
                if let Some(h) = v.get("head") { Self::patch_head(head, h); }
                if let (Some(it), Some(p)) = (item, v.get("item")) { Self::patch_item(it, p); }
                Ok(())
            }
        }
    }
    fn commit(&self, plan: &PublishPlan) -> Result<Receipt, MError> {
        self.ops.borrow_mut()
            .push(format!("commit:{}", plan.publication().publication_id));
        if self.commit_result.get("err").is_some() {
            Err(Self::script_err(&self.commit_result))
        } else {
            Ok(serde_json::from_value::<Receipt>(self.commit_result["ok"].clone()).unwrap())
        }
    }
    fn receipt(&self, _id: &str) -> Result<Option<Receipt>, MError> {
        self.ops.borrow_mut().push("receipt".into());
        Err(MError::Storage("seam".into()))
    }
    fn range(&self, _s: &Scope, _v: Option<&str>, _r: &Range) -> Result<ReadPage, MError> {
        self.ops.borrow_mut().push("range".into());
        Err(MError::Storage("seam".into()))
    }
    fn versions(&self, _s: &Scope) -> Result<Vec<x_data_mcp::publish::EffectiveVersion>, MError> {
        self.ops.borrow_mut().push("versions".into());
        Err(MError::Storage("seam".into()))
    }
}
impl x_data_mcp::official_market::DailyCollectionRepository for ScriptedRepo {
    fn collection_receipt(&self, task: &str, item: &str) -> Result<Option<Receipt>, MError> {
        self.ops.borrow_mut().push(format!("collection_receipt:{task}:{item}"));
        match Self::pop(&self.receipts, item) {
            None => Ok(None),
            Some(v) => {
                if v.get("err").is_some() { return Err(Self::script_err(&v)); }
                if v.is_null() { return Ok(None); }
                Ok(Some(serde_json::from_value::<Receipt>(v).unwrap()))
            }
        }
    }
    fn effective_head(&self, scope: &Scope) -> Result<Option<String>, MError> {
        let k = scope.occupancy_key();
        self.ops.borrow_mut().push(format!("effective_head:{k}"));
        match self.scopes.get(&k) {
            None => Ok(None),
            Some(v) => {
                if v.get("err").is_some() { return Err(Self::script_err(v)); }
                Ok(v.as_str().map(str::to_string))
            }
        }
    }
    fn transition_task(
        &self, task: &str, revision: u64, run: &str, change: Change,
    ) -> Result<TaskSnapshot, MError> {
        self.ops.borrow_mut()
            .push(format!("transition_task:{task}:{revision}:{run}:{}", Self::change_tag(&change)));
        Err(MError::Storage("seam".into()))
    }
    fn commit_collection_batch(
        &self, task_id: &str, items: &[CollectionCandidate],
    ) -> Result<Option<Vec<Result<Receipt, MError>>>, MError> {
        self.ops.borrow_mut()
            .push(format!("commit_collection_batch:{task_id}:{}", items.len()));
        // Bind candidate content into the trace (serde Value form,
        // canonical to_string) — the decision payload under test.
        for it in items {
            let v = json!({
                "task_id": it.task_id, "item_id": it.item_id,
                "publication_id": it.publication_id, "version_id": it.version_id,
                "run_id": it.run_id, "symbol": it.symbol, "source": it.source,
                "bars": it.bars, "coverage": it.coverage,
                "unit_currency": it.unit_currency,
                "adjustment_evidence": it.adjustment_evidence,
                "published_at": it.published_at,
                "generation_id": it.generation_id,
                "generation_manifest_sha256": it.generation_manifest_sha256,
                "day_member_sha256": it.day_member_sha256,
                "security_event_sha256": it.security_event_sha256,
                "generation_evidence": it.generation_evidence,
            });
            self.ops.borrow_mut().push(format!(
                "cand:{}", x_data_mcp::hash::sha256(v.to_string().as_bytes())));
        }
        if self.collection_batch.get("err").is_some() {
            return Err(Self::script_err(&self.collection_batch));
        }
        if self.collection_batch.get("none").is_some() { return Ok(None); }
        let mut out = Vec::new();
        for r in self.collection_batch["ok"].as_array().unwrap() {
            if r.get("err").is_some() { out.push(Err(Self::script_err(r))); }
            else { out.push(Ok(serde_json::from_value::<Receipt>(r["ok"].clone()).unwrap())); }
        }
        Ok(Some(out))
    }
}

fn repo_of(v: &Value) -> ScriptedRepo {
    let mut r = RepoState::default();
    for (k, arr) in v["bindings"].as_object().map(|o| o.clone()).unwrap_or_default() {
        r.bindings.borrow_mut()
            .insert(k, arr.as_array().unwrap().iter().cloned().collect());
    }
    for (k, arr) in v["receipts"].as_object().map(|o| o.clone()).unwrap_or_default() {
        r.receipts.borrow_mut()
            .insert(k, arr.as_array().unwrap().iter().cloned().collect());
    }
    for t in v["transitions"].as_array().map(|a| a.clone()).unwrap_or_default() {
        r.transitions.borrow_mut().push_back(t);
    }
    if let Some(o) = v["heads"].as_object() {
        r.heads = o.iter().map(|(k, x)| (k.clone(), x.clone())).collect();
    }
    if let Some(o) = v["tasks"].as_object() {
        r.tasks = o.iter().map(|(k, x)| (k.clone(), x.clone())).collect();
    }
    if let Some(o) = v["scopes"].as_object() {
        r.scopes = o.iter().map(|(k, x)| (k.clone(), x.clone())).collect();
    }
    if let Some(o) = v["records"].as_object() {
        r.records = o.iter().map(|(k, x)| (k.clone(), x.clone())).collect();
    }
    r.mark_validated_err = v["mark_validated_err"].as_str().map(str::to_string);
    r.stage_err = v["stage_err"].as_str().map(str::to_string);
    r.commit_result = v["commit"].clone();
    r.collection_batch = v["commit_collection_batch"].clone();
    ScriptedRepo { inner: std::rc::Rc::new(r) }
}

fn execution_json(e: &ItemExecution) -> Value {
    match e {
        ItemExecution::Published(r) => json!({"published": serde_json::to_value(r).unwrap()}),
        ItemExecution::NotPublished(t) =>
            json!({"not_published": serde_json::to_value(t).unwrap()}),
    }
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let manifest = std::fs::read(&args[0]).unwrap();
    let root = args[1].clone();
    let now = ts(&args[2]);
    let scenario: Value =
        serde_json::from_str(&std::fs::read_to_string(&args[3]).unwrap()).unwrap();
    let published_at = ts(scenario["published_at"].as_str().unwrap_or(&args[2]));
    let source = match TdxMarketSource::inspect(Path::new(&root), &manifest, now) {
        Ok(s) => s,
        Err(e) => {
            emit("inspect", serr(e));
            return;
        }
    };
    for (i, op) in scenario["ops"].as_array().unwrap().iter().enumerate() {
        let id = format!("{}:{}", i, op["op"].as_str().unwrap());
        let session = source.materialize_session();
        match op["op"].as_str().unwrap() {
            "acquire" => {
                let range = range_of(&op["range"]);
                let p = TdxRangeProvider { session: &session, published_at };
                match base_of(&op["base"]) {
                    Err(e) => emit(&id, merr(e)),
                    Ok(base) => match p.acquire(&base, &range,
                            op["candidate_id"].as_str().unwrap()) {
                        Ok(a) => emit(&id, ok(acquisition_json(&a))),
                        Err(e) => emit(&id, serr(e)),
                    },
                }
            }
            "acquire_typed" => {
                let facts = facts_of(&op["facts"]);
                let window: Vec<Bar> =
                    serde_json::from_value(op["base_window"].clone()).unwrap();
                let range = range_of(&op["range"]);
                let p = TdxRangeProvider { session: &session, published_at };
                match p.acquire_typed(&facts, &window, &range) {
                    Ok(t) => emit(&id, ok(typed_json(&t))),
                    Err(e) => emit(&id, serr(e)),
                }
            }
            "acquire_merged" => {
                let range = range_of(&op["range"]);
                let cid = op["candidate_id"].as_str().unwrap();
                match base_of(&op["base"]) {
                    Err(e) => emit(&id, merr(e)),
                    Ok(base) => {
                        let r = match op["provider"].as_str().unwrap() {
                            "backfill" =>
                                TdxBackfillProvider { session: &session, published_at }
                                    .acquire(&base, &range, cid),
                            _ =>
                                TdxRepublishProvider { session: &session, published_at }
                                    .acquire(&base, &range, cid),
                        };
                        match r {
                            Ok(a) => emit(&id, ok(acquisition_json(&a))),
                            Err(e) => emit(&id, serr(e)),
                        }
                    }
                }
            }
            "gap_verify" => {
                let repo = ScriptedRepo {
                    inner: std::rc::Rc::new(RepoState {
                        records: op["records"].as_object()
                            .map(|o| o.iter().map(|(k, x)| (k.clone(), x.clone())).collect())
                            .unwrap_or_default(),
                        ..Default::default()
                    }),
                };
                let target = x_data_mcp::gap_verification::GapTarget {
                    symbol: op["target"]["symbol"].as_str().unwrap().into(),
                    start_date: nd(op["target"]["start_date"].as_str().unwrap()),
                    end_date: nd(op["target"]["end_date"].as_str().unwrap()),
                };
                let subject = x_data_mcp::gap_verification::SubjectBinding {
                    version_id: op["subject"]["version_id"].as_str().unwrap().into(),
                    integrity_sha256: op["subject"]["integrity_sha256"].as_str().unwrap().into(),
                };
                let observed = ts(op["observed_at"].as_str().unwrap());
                let v = TdxGapVerifier {
                    source: &source, repository: repo, observed_at: observed,
                };
                match x_data_mcp::gap_verification::SourceGapVerifier::verify(
                    &v, &target, &subject) {
                    Ok(b) => emit(&id, ok(serde_json::to_value(&b).unwrap())),
                    Err(e) => emit(&id, merr(e)),
                }
            }
            "run_item" | "run_items" => {
                let repo = repo_of(&op["repo"]);
                let run_id = op["run_id"].as_str().unwrap_or("run-1").to_string();
                let runner = TdxDailyRunner {
                    source: &source,
                    session: Some(&session),
                    repository: repo,
                    run_id,
                    published_at,
                };
                if op["op"] == "run_item" {
                    let r = runner.run_item(
                        op["task_id"].as_str().unwrap(), op["item_id"].as_str().unwrap());
                    emit(&id, match r {
                        Ok(e) => ok(execution_json(&e)),
                        Err(e) => merr(e),
                    });
                } else {
                    let ids: Vec<String> = serde_json::from_value(op["item_ids"].clone()).unwrap();
                    let gid = op["generation_id"].as_str().unwrap().to_string();
                    let r = runner.run_items(
                        op["task_id"].as_str().unwrap(), &ids, &gid);
                    emit(&id, match r {
                        Ok(Some(())) => ok(json!("some")),
                        Ok(None) => ok(Value::Null),
                        Err(e) => merr(e),
                    });
                }
                emit(&format!("{id}.trace"), json!({
                    "ops": runner.repository.ops.borrow().clone()}));
            }
            other => emit(&id, json!({"status": "error", "kind": "bad_op", "op": other})),
        }
    }
}
