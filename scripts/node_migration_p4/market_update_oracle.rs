// NM-082 golden-vector oracle for the market_update decision layer.
// Compiles standalone against the baseline rlib (edition 2024):
//   rustc --edition 2024 -L target/debug/deps \
//     --extern x_data_mcp=target/debug/libx_data_mcp.rlib \
//     --extern serde_json=<rlib> --extern chrono=<rlib> --extern rust_decimal=<rlib> \
//     -o /tmp/mu_oracle market_update_oracle.rs
// Reads scenario JSONL (argv[1] or stdin); emits one golden JSONL row per
// scenario: {seq, op, status:"ok"|"err", code?, message?, detail?, calls?}.
// Errors carry verbatim Display strings (serde decode, market_storage,
// task_control). The stub repository/provider are scaffolding; every
// function under test is the real Rust.
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::rc::Rc;

use chrono::{DateTime, NaiveDate, Utc};
use x_data_mcp::market_data::{Bar, EvidenceValue, MarketDataBatch, SourceError, UnitCurrencyEvidence};
use x_data_mcp::market_storage::compat::{CheckedRecord, check};
use x_data_mcp::market_storage::{
    Error, FixedFacts, GapCovering, MarketRepository, Publication, PublishIntent, PublishPlan,
    Publisher, Range, ReadPage, Receipt, Scope,
};
use x_data_mcp::market_update::publication::{CombinedEvidence, Preparation, RangeRepository};
use x_data_mcp::market_update::runner::{
    Acquisition, CombinedCandidate, ItemExecution, RangeProvider, RangeRunner, TypedAcquisition,
    TypedDecision,
};
use x_data_mcp::market_update::tasks::{Operation, RangeContract};
use x_data_mcp::market_update::{
    self, DateRange, Evidence, ContinuityEvidence, UpdateBaseFacts, UpdateBatchItem,
    UpdateBatchPrepared,
};
use x_data_mcp::publish::EffectiveVersion;
use x_data_mcp::task_control::{
    Change, CommandKey, Contract, Error as TcError, FrozenWork, ItemBinding, ItemCounts,
    ItemOutcome, ItemSpec, Stage, Status, TaskHead, TaskItem, TaskRecord, TaskSnapshot,
};

// ------------------------------------------------------------ helpers
fn date(v: &Value) -> NaiveDate {
    serde_json::from_value(v.clone()).unwrap()
}
fn dtc(s: &str) -> DateTime<Utc> {
    serde_json::from_value(json!(s)).unwrap()
}
fn parse_status(s: &str) -> Status {
    serde_json::from_value(json!(s)).unwrap()
}
fn parse_stage(s: &str) -> Stage {
    serde_json::from_value(json!(s)).unwrap()
}
fn reason_json(r: &market_update::Reason) -> Value {
    match r {
        market_update::Reason::CoverageUnknown => json!({"kind":"coverage_unknown"}),
        market_update::Reason::BasisUnknown => json!({"kind":"basis_unknown"}),
        market_update::Reason::UnitsIncompatible => json!({"kind":"units_incompatible"}),
        market_update::Reason::MissingOldDates(d) =>
            json!({"kind":"missing_old_dates","dates":d.iter().map(|d| d.to_string()).collect::<Vec<_>>()}),
        market_update::Reason::KnownValueLost(d) =>
            json!({"kind":"known_value_lost","dates":d.iter().map(|d| d.to_string()).collect::<Vec<_>>()}),
        market_update::Reason::RevisionUnverified(d) =>
            json!({"kind":"revision_unverified","dates":d.iter().map(|d| d.to_string()).collect::<Vec<_>>()}),
    }
}
fn evidence_of(v: &Value) -> Evidence {
    Evidence {
        base_version_id: v["base_version_id"].as_str().unwrap().to_string(),
        base_integrity_sha256: v["base_integrity_sha256"].as_str().unwrap().to_string(),
        candidate_sha256: v["candidate_sha256"].as_str().unwrap().to_string(),
        range: DateRange { start: date(&v["range"]["start"]), end: date(&v["range"]["end"]) },
        same_qfq_basis_ref: v["same_qfq_basis_ref"].as_str().unwrap().to_string(),
        revision_ref: v.get("revision_ref").and_then(|r| r.as_str().map(str::to_string)),
        continuity: v.get("continuity").filter(|c| !c.is_null()).map(|c| ContinuityEvidence {
            mode: c["mode"].as_str().unwrap().to_string(),
            day_member_sha256: c["day_member_sha256"].as_str().unwrap().to_string(),
            day_prefix_bytes: c["day_prefix_bytes"].as_u64().unwrap(),
            gbbq_events_sha256: c["gbbq_events_sha256"].as_str().unwrap().to_string(),
        }),
    }
}
fn mk_error(kind: &str, msg: &str) -> Error {
    match kind {
        "storage" => Error::Storage(msg.into()),
        "integrity" => Error::Integrity(msg.into()),
        "base" => Error::Base(msg.into()),
        "conflict" => Error::Conflict(msg.into()),
        "not_found" => Error::NotFound,
        "invalid" => Error::Invalid(msg.into()),
        "outcome_unknown" => Error::OutcomeUnknown(msg.into()),
        "basis_not_verified" => Error::BasisNotVerified(msg.into()),
        _ => Error::Storage(format!("UNEXPECTED kind {kind}")),
    }
}
fn tc_kind(e: &TcError) -> &'static str {
    match e {
        TcError::InvalidCommand(_) => "invalid_input",
        TcError::RequestConflict => "request_conflict",
        TcError::RangeConflict(_) => "range_conflict",
        TcError::NeedsVerification(_) => "needs_verification",
        TcError::StorageUnavailable(_) => "unavailable",
        TcError::OutcomeUnknown(_) => "outcome_unknown",
        TcError::RevisionConflict => "revision_conflict",
        TcError::UnsupportedContract => "unsupported_contract",
        TcError::TaskNotFound => "task_not_found",
        _ => "other_task_error",
    }
}
fn err_kind(e: &Error) -> &'static str {
    match e {
        Error::Storage(_) => "storage",
        Error::Integrity(_) => "integrity",
        Error::Base(_) => "base",
        Error::Conflict(_) => "conflict",
        Error::NotFound => "not_found",
        Error::Invalid(_) => "invalid",
        Error::OutcomeUnknown(_) => "outcome_unknown",
        Error::BasisNotVerified(_) => "basis_not_verified",
        Error::Task(t) => tc_kind(t),
    }
}
fn src_error(e: &Value) -> SourceError {
    let k = e["kind"].as_str().unwrap();
    let d = e["detail"].as_str().unwrap_or("");
    match k {
        "not_verified" => SourceError::NotVerified(d.into()),
        "unsupported" => SourceError::Unsupported(d.into()),
        "failed" => SourceError::Invocation(d.into()),
        "timed_out" => SourceError::Timeout { timeout_ms: d.parse().unwrap_or(0) },
        _ => SourceError::InvalidResponse(d.into()),
    }
}

// ---------------------------------------------------------- stub repo
// cfg keys drive canned answers; `calls` logs every invocation. The mini
// task machine mirrors decide_head's legal transitions used on the
// update path (claim/start/finish item/finish execution/finish
// cancellation/require-verification) — scaffolding only; the decision
// layer under test is the real Rust.
struct Inner {
    cfg: Value,
    head: RefCell<Option<TaskHead>>,
    item: RefCell<Option<TaskItem>>,
    calls: RefCell<Vec<Value>>,
}
#[derive(Clone)]
struct StubRepo {
    inner: Rc<Inner>,
}
fn mk_head(c: &Value) -> TaskHead {
    TaskHead {
        task_id: c["task_id"].as_str().unwrap().to_string(),
        key: CommandKey {
            operation: c["operation"].as_str().unwrap().to_string(),
            dataset: c["dataset"].as_str().unwrap_or("market.daily").to_string(),
            contract_version: c["contract_version"].as_str().unwrap().to_string(),
        },
        status: parse_status(c["status"].as_str().unwrap()),
        state_revision: c["state_revision"].as_u64().unwrap(),
        accepted_run_id: c["accepted_run_id"].as_str().unwrap().to_string(),
        owner_run_id: c["owner_run_id"].as_str().map(str::to_string),
        last_progress_at: dtc("2024-01-01T00:00:00Z"),
        result: c.get("result").filter(|r| !r.is_null()).cloned(),
        counts: ItemCounts {
            total: c["counts"]["total"].as_u64().unwrap(),
            pending: c["counts"]["pending"].as_u64().unwrap(),
            running: c["counts"]["running"].as_u64().unwrap(),
            finished: c["counts"]["finished"].as_u64().unwrap(),
            succeeded: c["counts"]["succeeded"].as_u64().unwrap(),
        },
        evidence_digest: c.get("evidence_digest").and_then(|e| e.as_str().map(str::to_string)),
    }
}
fn mk_item(c: &Value) -> TaskItem {
    TaskItem {
        spec: ItemSpec {
            item_id: c["item_id"].as_str().unwrap().to_string(),
            target: c["target"].clone(),
            progress_unit: c["progress_unit"].as_str().unwrap_or("security_period").to_string(),
            frozen_inputs: c["frozen_inputs"].clone(),
            scope_keys: c["scope_keys"]
                .as_array()
                .map(|a| a.iter().map(|v| v.as_str().unwrap().to_string()).collect())
                .unwrap_or_default(),
        },
        stage: parse_stage(c["stage"].as_str().unwrap()),
        outcome: c.get("outcome").filter(|o| !o.is_null()).map(|o| ItemOutcome {
            success: o["success"].as_bool().unwrap(),
            error: o["error"].as_str().map(str::to_string),
            evidence: o.get("evidence").cloned().filter(|e| !e.is_null()),
        }),
    }
}
impl StubRepo {
    fn new(cfg: Value) -> Self {
        let head = cfg.get("head").filter(|h| !h.is_null()).map(mk_head);
        let item = cfg.get("item").filter(|i| !i.is_null()).map(mk_item);
        StubRepo { inner: Rc::new(Inner { cfg, head: RefCell::new(head), item: RefCell::new(item), calls: RefCell::new(vec![]) }) }
    }
    fn log(&self, v: Value) {
        self.inner.calls.borrow_mut().push(v);
    }
    fn calls(&self) -> Value {
        json!(self.inner.calls.borrow().clone())
    }
    fn err_at(&self, key: &str) -> Option<Error> {
        let e = &self.inner.cfg["errors"][key];
        if e.is_null() {
            None
        } else {
            Some(mk_error(e["kind"].as_str().unwrap(), e["message"].as_str().unwrap_or("")))
        }
    }
    fn apply(&self, head: &mut TaskHead, mut item: Option<&mut TaskItem>, run: &str, ch: Change) -> Result<(), Error> {
        self.log(json!({"call":"transition_task_head","change":format!("{:?}", ch)}));
        let r = self.apply_inner(head, item.as_deref_mut(), run, ch);
        if r.is_ok() {
            // Row write-back: task_item_binding returns clones, so a real
            // backend UPDATE is modeled by storing the mutated row.
            *self.inner.head.borrow_mut() = Some(head.clone());
            if let Some(it) = item {
                *self.inner.item.borrow_mut() = Some(it.clone());
            }
        }
        r
    }
    // Mirrors decide_head's ordering for the transitions run_item uses
    // (terminal guard -> visibility guard -> arm -> state_revision++).
    fn apply_inner(&self, head: &mut TaskHead, item: Option<&mut TaskItem>, run: &str, ch: Change) -> Result<(), Error> {
        if run.trim().is_empty() {
            return Err(Error::Task(TcError::InvalidCommand("empty run identity".into())));
        }
        if head.status.terminal() {
            return match ch {
                Change::RequestCancel | Change::Reconcile => Ok(()),
                _ => Err(Error::Task(TcError::RevisionConflict)),
            };
        }
        let visible_nv = head.accepted_run_id != run
            || head.owner_run_id.as_deref().is_some_and(|o| o != run);
        if matches!(ch, Change::Reconcile) {
            if visible_nv {
                head.status = Status::NeedsVerification;
            } else {
                return Ok(());
            }
        } else {
            if visible_nv {
                return Err(Error::Task(TcError::NeedsVerification("old run or unresolved execution".into())));
            }
            match ch {
                Change::Claim => {
                    if head.status != Status::Pending || head.owner_run_id.is_some() {
                        return Err(Error::Task(TcError::RevisionConflict));
                    }
                    head.status = Status::Running;
                    head.owner_run_id = Some(run.into());
                }
                Change::RequestCancel => {
                    if head.status == Status::CancelRequested {
                        return Ok(());
                    }
                    head.status = Status::CancelRequested;
                }
                Change::StartItem(id) => {
                    if head.status != Status::Running || head.owner_run_id.as_deref() != Some(run) {
                        return Err(Error::Task(TcError::RevisionConflict));
                    }
                    if head.counts.running != 0 {
                        return Err(Error::Task(TcError::RevisionConflict));
                    }
                    let it = item.ok_or(Error::Task(TcError::RevisionConflict))?;
                    if it.spec.item_id != id || it.stage != Stage::Pending {
                        return Err(Error::Task(TcError::RevisionConflict));
                    }
                    it.stage = Stage::Running;
                    head.counts.pending -= 1;
                    head.counts.running += 1;
                }
                Change::FinishItem(id, outcome) => {
                    if head.owner_run_id.as_deref() != Some(run) {
                        return Err(Error::Task(TcError::RevisionConflict));
                    }
                    if outcome.success && outcome.error.is_some() {
                        return Err(Error::Task(TcError::InvalidCommand("success with error".into())));
                    }
                    let it = item.ok_or(Error::Task(TcError::RevisionConflict))?;
                    if it.spec.item_id != id || it.stage != Stage::Running {
                        return Err(Error::Task(TcError::RevisionConflict));
                    }
                    it.stage = Stage::Finished;
                    it.outcome = Some(outcome.clone());
                    head.counts.running -= 1;
                    head.counts.finished += 1;
                    if outcome.success {
                        head.counts.succeeded += 1;
                    }
                }
                Change::FinishExecution | Change::FinishCancellation => {
                    let cancellation_finished = matches!(ch, Change::FinishCancellation);
                    if cancellation_finished && head.status != Status::CancelRequested {
                        return Err(Error::Task(TcError::RevisionConflict));
                    }
                    let owned = head.owner_run_id.as_deref() == Some(run);
                    let never_started = head.owner_run_id.is_none()
                        && head.counts.pending == head.counts.total;
                    if (!owned && !never_started) || head.counts.running != 0 {
                        return Err(Error::Task(TcError::NeedsVerification(
                            "execution has not safely ended".into())));
                    }
                    let (finished, success, total) =
                        (head.counts.finished, head.counts.succeeded, head.counts.total);
                    if cancellation_finished && success < total {
                        head.status = Status::Cancelled;
                    } else if finished == total {
                        head.status = if success == finished {
                            Status::Succeeded
                        } else if success == 0 {
                            Status::Failed
                        } else {
                            Status::PartiallyFailed
                        };
                    } else if head.status == Status::CancelRequested {
                        head.status = Status::Cancelled;
                    } else {
                        return Err(Error::Task(TcError::RevisionConflict));
                    }
                    head.result = Some(json!({"finished_items":finished,"successful_items":success}));
                    if cancellation_finished && head.status == Status::Cancelled {
                        head.result.as_mut().unwrap().as_object_mut().unwrap()
                            .insert("cancellation_acknowledged".into(), json!(true));
                    }
                }
                Change::RequireVerification { item_id, reason } => {
                    let bound = item.as_deref().is_some_and(|it| {
                        it.spec.item_id == item_id && it.stage == Stage::Running
                    });
                    if reason.trim().is_empty()
                        || head.owner_run_id.as_deref() != Some(run)
                        || !matches!(head.status, Status::Running | Status::CancelRequested)
                        || !bound
                    {
                        return Err(Error::Task(TcError::RevisionConflict));
                    }
                    let previous = head.result.clone();
                    let previous_status = serde_json::to_value(head.status).unwrap();
                    head.status = Status::NeedsVerification;
                    head.result = Some(json!({"execution_uncertainty":{
                        "item_id":item_id,"reason":reason,
                        "previous_result":previous,"previous_status":previous_status}}));
                }
                Change::Reconcile => unreachable!(),
            }
        }
        head.state_revision += 1;
        head.last_progress_at = Utc::now();
        Ok(())
    }
}
impl MarketRepository for StubRepo {
    fn stage(&self, _raw: &Value) -> Result<(), Error> {
        self.log(json!({"call":"stage"}));
        Err(Error::Storage("unimplemented stage".into()))
    }
    fn candidate(&self, id: &str) -> Result<CheckedRecord, Error> {
        self.log(json!({"call":"candidate","id":id}));
        if let Some(e) = self.err_at("candidate") {
            return Err(e);
        }
        let raw = &self.inner.cfg["candidate"];
        if raw.is_null() {
            return Err(Error::Storage("UNEXPECTED candidate".into()));
        }
        check(raw)
    }
    fn mark_validated(&self, record: &CheckedRecord) -> Result<(), Error> {
        self.log(json!({"call":"mark_validated","version_id":record.version().version_id}));
        if let Some(e) = self.err_at("mark_validated") {
            return Err(e);
        }
        Ok(())
    }
    fn fixed(&self, id: &str) -> Result<CheckedRecord, Error> {
        self.log(json!({"call":"fixed","id":id}));
        if let Some(e) = self.err_at("fixed") {
            return Err(e);
        }
        let raw = &self.inner.cfg["fixed"];
        if raw.is_null() {
            return Err(Error::Storage("UNEXPECTED fixed".into()));
        }
        check(raw)
    }
    fn fixed_many(&self, ids: &[String]) -> Result<Vec<CheckedRecord>, Error> {
        self.log(json!({"call":"fixed_many","ids":ids}));
        Err(Error::Storage("UNEXPECTED fixed_many".into()))
    }
    fn fixed_facts(&self, id: &str) -> Result<FixedFacts, Error> {
        self.log(json!({"call":"fixed_facts","id":id}));
        if let Some(e) = self.err_at("fixed_facts") {
            return Err(e);
        }
        let by_id = &self.inner.cfg["facts_by_id"][id];
        let f = if by_id.is_null() { &self.inner.cfg["facts"] } else { by_id };
        if f.is_null() {
            return Err(Error::Storage("UNEXPECTED fixed_facts".into()));
        }
        Ok(FixedFacts {
            version_id: f["version_id"].as_str().unwrap().to_string(),
            symbol: f["symbol"].as_str().unwrap().to_string(),
            period: serde_json::from_value(f["period"].clone()).unwrap(),
            integrity_sha256: f["integrity_sha256"].as_str().unwrap().to_string(),
            first_day: date(&f["first_day"]),
            last_day: date(&f["last_day"]),
        })
    }
    fn fixed_window(&self, id: &str, a: NaiveDate, b: NaiveDate) -> Result<CheckedRecord, Error> {
        self.log(json!({"call":"fixed_window","id":id,"a":a.to_string(),"b":b.to_string()}));
        if let Some(e) = self.err_at("fixed_window") {
            return Err(e);
        }
        let raw = &self.inner.cfg["window"];
        if raw.is_null() {
            return Err(Error::Storage("UNEXPECTED fixed_window".into()));
        }
        check(raw)
    }
    fn fixed_window_years(&self, _id: &str, _a: NaiveDate, _b: NaiveDate) -> Result<BTreeSet<i32>, Error> {
        Err(Error::Storage("UNEXPECTED fixed_window_years".into()))
    }
    fn task(&self, id: &str) -> Result<TaskSnapshot, Error> {
        self.log(json!({"call":"task","id":id}));
        if let Some(e) = self.err_at("task") {
            return Err(e);
        }
        let head = self.inner.head.borrow().clone().ok_or(Error::Storage("UNEXPECTED task".into()))?;
        let item = self.inner.item.borrow().clone();
        Ok(TaskSnapshot {
            task: TaskRecord {
                task_id: head.task_id.clone(),
                key: head.key.clone(),
                status: head.status,
                state_revision: head.state_revision,
                accepted_run_id: head.accepted_run_id.clone(),
                owner_run_id: head.owner_run_id.clone(),
                created_at: head.last_progress_at,
                last_progress_at: head.last_progress_at,
                work: serde_json::from_value(self.inner.cfg["work"].clone()).unwrap_or(FrozenWork {
                    items: vec![],
                    scopes: BTreeSet::new(),
                    shared_evidence: None,
                }),
                result: head.result.clone(),
                retry_of: None,
            },
            items: item.into_iter().collect(),
            claims: vec![],
        })
    }
    fn task_head(&self, id: &str) -> Result<TaskHead, Error> {
        self.log(json!({"call":"task_head","id":id}));
        if let Some(e) = self.err_at("task_head") {
            return Err(e);
        }
        self.inner.head.borrow().clone().ok_or(Error::Task(TcError::TaskNotFound))
    }
    fn task_item_binding(&self, task: &str, item: &str) -> Result<Option<ItemBinding>, Error> {
        self.log(json!({"call":"task_item_binding","task":task,"item":item}));
        if let Some(e) = self.err_at("task_item_binding") {
            return Err(e);
        }
        let head = self.inner.head.borrow().clone();
        let it = self.inner.item.borrow().clone();
        match (head, it) {
            (Some(h), Some(i)) if h.task_id == task && i.spec.item_id == item => {
                Ok(Some(ItemBinding { head: h, item: i, scopes_bound: self.inner.cfg["scopes_bound"].as_bool().unwrap_or(true) }))
            }
            _ => Ok(None),
        }
    }
    fn transition_task_head(&self, head: &mut TaskHead, item: Option<&mut TaskItem>, run: &str, change: Change) -> Result<(), Error> {
        self.apply(head, item, run, change)
    }
    fn commit(&self, _plan: &PublishPlan) -> Result<Receipt, Error> {
        Err(Error::Storage("UNEXPECTED commit".into()))
    }
    fn receipt(&self, id: &str) -> Result<Option<Receipt>, Error> {
        self.log(json!({"call":"receipt","id":id}));
        Err(Error::Storage("UNEXPECTED receipt".into()))
    }
    fn range(&self, _s: &Scope, _v: Option<&str>, _r: &Range) -> Result<ReadPage, Error> {
        Err(Error::Storage("UNEXPECTED range".into()))
    }
    fn versions(&self, _s: &Scope) -> Result<Vec<EffectiveVersion>, Error> {
        Err(Error::Storage("UNEXPECTED versions".into()))
    }
}
fn prepared_json(p: &UpdateBatchPrepared) -> Value {
    match p {
        UpdateBatchPrepared::NoChange { base_version_id, range_start_day, range_end_day, continuity } => json!({
            "kind": "no_change", "base_version_id": base_version_id,
            "range_start_day": range_start_day, "range_end_day": range_end_day,
            "continuity": continuity,
        }),
        UpdateBatchPrepared::Append { base_version_id, result_version_id, upserts, continuity } => json!({
            "kind": "append", "base_version_id": base_version_id,
            "result_version_id": result_version_id,
            "upserts": serde_json::to_value(upserts).unwrap(), "continuity": continuity,
        }),
    }
}
impl RangeRepository for StubRepo {
    fn commit_range(&self, _plan: &x_data_mcp::market_update::publication::RangePlan) -> Result<Receipt, Error> {
        Err(Error::Storage("UNEXPECTED commit_range".into()))
    }
    fn update_base_facts(&self, id: &str) -> Result<Option<UpdateBaseFacts>, Error> {
        self.log(json!({"call":"update_base_facts","id":id}));
        if let Some(e) = self.err_at("update_base_facts") {
            return Err(e);
        }
        let f = &self.inner.cfg["base_facts"];
        if f.is_null() {
            return Ok(None);
        }
        let ev = |v: &Value| -> EvidenceValue { serde_json::from_value(v.clone()).unwrap() };
        Ok(Some(UpdateBaseFacts {
            version_id: f["version_id"].as_str().unwrap().to_string(),
            symbol: f["symbol"].as_str().unwrap().to_string(),
            record_count: f["record_count"].as_u64().unwrap() as u32,
            first_day: date(&f["first_day"]),
            last_day: date(&f["last_day"]),
            integrity_sha256: f["integrity_sha256"].as_str().unwrap().to_string(),
            coverage: serde_json::from_value(f["coverage"].clone()).unwrap(),
            unit_currency: UnitCurrencyEvidence {
                price_currency: ev(&f["unit_currency"]["price_currency"]),
                price_unit: ev(&f["unit_currency"]["price_unit"]),
                volume_unit: ev(&f["unit_currency"]["volume_unit"]),
                amount_currency: ev(&f["unit_currency"]["amount_currency"]),
                amount_unit: ev(&f["unit_currency"]["amount_unit"]),
            },
            source: f["source"].as_str().unwrap().to_string(),
            generation_id: f["generation_id"].as_str().unwrap_or("g").to_string(),
            generation_manifest_sha256: f["generation_manifest_sha256"].as_str().unwrap_or("m").to_string(),
            day_member_sha256: f["day_member_sha256"].as_str().unwrap_or("d").to_string(),
            security_event_sha256: f["security_event_sha256"].as_str().unwrap_or("s").to_string(),
        }))
    }
    fn base_bars_in_range(&self, id: &str, a: NaiveDate, b: NaiveDate) -> Result<Option<Vec<Bar>>, Error> {
        self.log(json!({"call":"base_bars_in_range","id":id,"a":a.to_string(),"b":b.to_string()}));
        if let Some(e) = self.err_at("base_bars_in_range") {
            return Err(e);
        }
        let v = &self.inner.cfg["base_bars"];
        if v.is_null() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_value::<Vec<Bar>>(v.clone()).unwrap()))
    }
    fn commit_update_batch(&self, task_id: &str, items: &[UpdateBatchItem]) -> Result<Option<Vec<Result<Receipt, Error>>>, Error> {
        self.log(json!({"call":"commit_update_batch","task_id":task_id,
            "items":items.iter().map(|i| json!({
                "task_id":i.task_id,"item_id":i.item_id,"publication_id":i.publication_id,
                "run_id":i.run_id,"symbol":i.symbol,"occupancy_key":i.occupancy_key,
                "prepared":prepared_json(&i.prepared)})).collect::<Vec<_>>()}));
        if let Some(e) = self.err_at("commit_update_batch") {
            return Err(e);
        }
        if self.inner.cfg["commit"].as_str() == Some("none") {
            return Ok(None);
        }
        let results = self.inner.cfg["commit_results"].as_array().cloned().unwrap_or_default();
        Ok(Some(results.iter().map(|r| {
            if let Some(rc) = r.get("receipt") {
                Ok(Receipt {
                    publication_id: rc["publication_id"].as_str().unwrap().to_string(),
                    version_id: rc["version_id"].as_str().unwrap().to_string(),
                    task_id: rc["task_id"].as_str().unwrap().to_string(),
                    task_revision: rc["task_revision"].as_u64().unwrap(),
                })
            } else {
                let e = &r["error"];
                Err(mk_error(e["kind"].as_str().unwrap(), e["message"].as_str().unwrap_or("")))
            }
        }).collect()))
    }
    fn range_receipt(&self, id: &str) -> Result<Option<Receipt>, Error> {
        self.log(json!({"call":"range_receipt","id":id}));
        Err(Error::Storage("UNEXPECTED range_receipt".into()))
    }
    fn item_receipt(&self, task: &str, item: &str) -> Result<Option<Receipt>, Error> {
        self.log(json!({"call":"item_receipt","task":task,"item":item}));
        if let Some(e) = self.err_at("item_receipt") {
            return Err(e);
        }
        let r = &self.inner.cfg["item_receipt"];
        if r.is_null() {
            Ok(None)
        } else {
            Ok(Some(Receipt {
                publication_id: r["publication_id"].as_str().unwrap().to_string(),
                version_id: r["version_id"].as_str().unwrap().to_string(),
                task_id: r["task_id"].as_str().unwrap().to_string(),
                task_revision: r["task_revision"].as_u64().unwrap(),
            }))
        }
    }
    fn transition_task(&self, task: &str, rev: u64, run: &str, change: Change) -> Result<TaskSnapshot, Error> {
        self.log(json!({"call":"transition_task","task":task,"rev":rev,"run":run,"change":format!("{:?}",change)}));
        Err(Error::Storage("UNEXPECTED transition_task".into()))
    }
    fn cover_gap(&self, c: &GapCovering) -> Result<bool, Error> {
        self.log(json!({"call":"cover_gap","gap_id":c.gap_id,"version_id":c.version_id}));
        if let Some(e) = self.err_at("cover_gap") {
            return Err(e);
        }
        Ok(true)
    }
    fn gap_coverings(&self, gap_id: &str) -> Result<Vec<GapCovering>, Error> {
        self.log(json!({"call":"gap_coverings","gap_id":gap_id}));
        if let Some(e) = self.err_at("gap_coverings") {
            return Err(e);
        }
        Ok(self.inner.cfg["coverings"][gap_id].as_array().cloned().unwrap_or_default().iter().map(|c| GapCovering {
            gap_id: c["gap_id"].as_str().unwrap().to_string(),
            version_id: c["version_id"].as_str().unwrap().to_string(),
            covered_at: dtc(c["covered_at"].as_str().unwrap_or("2024-01-01T00:00:00Z")),
        }).collect())
    }
}

// ------------------------------------------------------- stub provider
struct StubProvider {
    cfg: Value,
}
fn acq_of(v: &Value) -> Acquisition {
    Acquisition {
        batch: serde_json::from_value::<MarketDataBatch>(v["batch"].clone()).unwrap(),
        evidence: evidence_of(&v["evidence"]),
        combined: v.get("combined").filter(|c| !c.is_null()).map(|c| CombinedCandidate {
            record: c["record"].clone(),
            evidence: CombinedEvidence {
                base_integrity_sha256: c["evidence"]["base_integrity_sha256"].as_str().unwrap().to_string(),
                segment_sha256: c["evidence"]["segment_sha256"].as_str().unwrap().to_string(),
                combined_integrity_sha256: c["evidence"]["combined_integrity_sha256"].as_str().unwrap().to_string(),
                complete_coverage_ref: c["evidence"]["complete_coverage_ref"].as_str().unwrap().to_string(),
            },
        }),
    }
}
impl RangeProvider for StubProvider {
    fn acquire(&self, _base: &CheckedRecord, _range: &DateRange, _candidate_id: &str) -> Result<Acquisition, SourceError> {
        let a = &self.cfg["acquire"];
        if let Some(e) = a.get("error") {
            return Err(src_error(e));
        }
        Ok(acq_of(a))
    }
    fn acquire_typed(&self, _facts: &UpdateBaseFacts, _w: &[Bar], _range: &DateRange) -> Result<Option<TypedAcquisition>, SourceError> {
        let a = &self.cfg["typed"];
        if a.is_null() {
            return Ok(None);
        }
        if let Some(e) = a.get("error") {
            return Err(src_error(e));
        }
        let ev = evidence_of(&a["evidence"]);
        let decision = if a["decision"]["kind"].as_str() == Some("unchanged") {
            TypedDecision::Unchanged
        } else {
            TypedDecision::Append {
                upserts: serde_json::from_value::<Vec<Bar>>(a["decision"]["upserts"].clone()).unwrap(),
            }
        };
        Ok(Some(TypedAcquisition { decision, evidence: ev }))
    }
}

// --------------------------------------------------------------- ops
thread_local! {
    static TODAY: std::cell::Cell<NaiveDate> = std::cell::Cell::new(NaiveDate::from_ymd_opt(2024, 6, 30).unwrap());
}
fn today() -> NaiveDate {
    TODAY.with(|c| c.get())
}
fn op_of(s: &str) -> Operation {
    match s {
        "update" => Operation::Update,
        "backfill" => Operation::Backfill,
        _ => Operation::Republish,
    }
}
fn publication_of(v: &Value) -> Publication {
    let i = &v["intent"];
    let intent = if let Some(d) = i.get("Daily") {
        PublishIntent::Daily { expected_head: d["expected_head"].as_str().map(str::to_string) }
    } else if i.as_str() == Some("HistoricalFixedBase") {
        PublishIntent::HistoricalFixedBase
    } else if let Some(c) = i.get("CurrentHead") {
        PublishIntent::CurrentHead {
            expected_daily_head: c["expected_daily_head"].as_str().unwrap().to_string(),
            expected_yearly_head: c["expected_yearly_head"].as_str().map(str::to_string),
        }
    } else {
        serde_json::from_value(i.clone()).unwrap()
    };
    Publication {
        publication_id: v["publication_id"].as_str().unwrap().to_string(),
        version_id: v["version_id"].as_str().unwrap().to_string(),
        task_id: v["task_id"].as_str().unwrap().to_string(),
        item_id: v["item_id"].as_str().unwrap().to_string(),
        run_id: v["run_id"].as_str().unwrap().to_string(),
        expected_revision: v["expected_revision"].as_u64().unwrap(),
        intent,
    }
}
fn combined_of(v: &Value) -> CombinedEvidence {
    CombinedEvidence {
        base_integrity_sha256: v["base_integrity_sha256"].as_str().unwrap().to_string(),
        segment_sha256: v["segment_sha256"].as_str().unwrap().to_string(),
        combined_integrity_sha256: v["combined_integrity_sha256"].as_str().unwrap().to_string(),
        complete_coverage_ref: v["complete_coverage_ref"].as_str().unwrap().to_string(),
    }
}
fn decision_json(d: &market_update::Decision) -> Value {
    match d {
        market_update::Decision::NoChange { version_id } =>
            json!({"kind":"no_change","version_id":version_id}),
        market_update::Decision::NeedsVerification(r) =>
            json!({"kind":"needs_verification","reason":reason_json(r),"debug":format!("{:?}",r)}),
        market_update::Decision::Candidate(p) => json!({
            "kind":"candidate",
            "records":serde_json::to_string(&p.records).unwrap(),
            "changed_dates":p.changed_dates.iter().map(|d| d.to_string()).collect::<Vec<_>>(),
            "metadata_changed":p.metadata_changed,
            "affected_years":p.affected_years.iter().collect::<Vec<_>>(),
            "base_version_id":p.base_version_id,
            "base_integrity_sha256":p.base_integrity_sha256,
            "candidate_sha256":p.candidate_sha256,
            "requested_range":{"start":p.requested_range.start.to_string(),"end":p.requested_range.end.to_string()},
            "same_qfq_basis_ref":p.same_qfq_basis_ref,
            "revision_ref":p.revision_ref,
        }),
    }
}
// Timestamps stamped by decide_head (Utc::now()) are nondeterministic —
// scrub them before emission so vectors stay stable.
fn scrub(v: &mut Value) {
    match v {
        Value::Object(m) => {
            for (k, x) in m.iter_mut() {
                if matches!(k.as_str(), "last_progress_at" | "created_at" | "covered_at") {
                    *x = json!("2024-01-01T00:00:00Z");
                } else {
                    scrub(x);
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(scrub),
        _ => {}
    }
}
fn run_case(seq: u64, c: &Value) -> Value {
    let op = c["op"].as_str().unwrap();
    macro_rules! out {
        (ok, $d:expr) => {
            json!({"seq":seq,"op":op,"status":"ok","detail":$d})
        };
        (err, $e:expr) => {{
            let e: String = $e;
            json!({"seq":seq,"op":op,"status":"err","message":e})
        }};
    }
    match op {
        "dec" => match serde_json::from_value::<rust_decimal::Decimal>(c["value"].clone()) {
            Ok(d) => out!(ok, json!({"canon": d.to_string(), "serde": serde_json::to_string(&d).unwrap()})),
            Err(e) => out!(err, e.to_string()),
        },
        "date" => match serde_json::from_value::<NaiveDate>(c["value"].clone()) {
            Ok(d) => out!(ok, json!({"canon": d.to_string(), "serde": serde_json::to_string(&d).unwrap()})),
            Err(e) => out!(err, e.to_string()),
        },
        "datetime" => match serde_json::from_value::<DateTime<Utc>>(c["value"].clone()) {
            Ok(d) => out!(ok, json!({"canon": serde_json::to_string(&d).unwrap()})),
            Err(e) => out!(err, e.to_string()),
        },
        "parse_batch" => match serde_json::from_value::<MarketDataBatch>(c["batch"].clone()) {
            Ok(b) => out!(ok, json!({"serde": serde_json::to_string(&b).unwrap()})),
            Err(e) => out!(err, e.to_string()),
        },
        "digest" => match serde_json::from_value::<MarketDataBatch>(c["batch"].clone()) {
            Ok(b) => out!(ok, json!({
                "digest": market_update::candidate_digest(&b).unwrap(),
                "bytes": serde_json::to_string(&b).unwrap()})),
            Err(e) => out!(err, e.to_string()),
        },
        "prepare" => {
            let existing = match check(&c["existing"]) {
                Ok(r) => r,
                Err(e) => return out!(err, format!("CHECK:{}", e)),
            };
            let batch: MarketDataBatch = match serde_json::from_value(c["batch"].clone()) {
                Ok(b) => b,
                Err(e) => return out!(err, format!("PARSE:{}", e)),
            };
            let range = DateRange { start: date(&c["range"][0]), end: date(&c["range"][1]) };
            let evidence = c.get("evidence").filter(|e| !e.is_null()).map(evidence_of);
            match market_update::prepare(&existing, &batch, &range, evidence.as_ref()) {
                Ok(d) => out!(ok, decision_json(&d)),
                Err(e) => json!({"seq":seq,"op":op,"status":"err","code":err_kind(&e),"message":e.to_string()}),
            }
        }
        "key" => {
            let key = op_of(c["operation"].as_str().unwrap()).key();
            out!(ok, serde_json::to_value(key).unwrap())
        }
        "normalize" | "resolve" => {
            if let Some(t) = c.get("today") {
                TODAY.with(|cell| cell.set(date(t)));
            }
            let stub = StubRepo::new(c.get("stub").cloned().unwrap_or(json!({})));
            let contract = RangeContract { operation: op_of(c["operation"].as_str().unwrap()), repository: stub.clone(), today };
            let r: Result<Value, TcError> = if op == "normalize" {
                contract.normalize(c["raw"].clone())
            } else {
                contract.resolve(&c["raw"]).map(|w| serde_json::to_value(w).unwrap())
            };
            match r {
                Ok(v) => out!(ok, v),
                Err(e) => json!({"seq":seq,"op":op,"status":"err","code":tc_kind(&e),"message":e.to_string()}),
            }
        }
        "prepare_range" => {
            let stub = StubRepo::new(c["stub"].clone());
            let publisher = Publisher { repository: stub.clone() };
            let batch: MarketDataBatch = match serde_json::from_value(c["batch"].clone()) {
                Ok(b) => b,
                Err(e) => return out!(err, format!("PARSE:{}", e)),
            };
            let range = DateRange { start: date(&c["range"][0]), end: date(&c["range"][1]) };
            let evidence = evidence_of(&c["evidence"]);
            let combined = c.get("combined").filter(|v| !v.is_null()).map(combined_of);
            let command = publication_of(&c["command"]);
            match publisher.prepare_range(command, &batch, &range, &evidence, combined.as_ref()) {
                Ok(Preparation::NeedsVerification(r)) => {
                    let mut o = json!({"seq":seq,"op":op,"status":"ok",
                        "detail":{"kind":"needs_verification","reason":reason_json(&r),"debug":format!("{:?}",r)}});
                    o["calls"] = stub.calls();
                    o
                }
                Ok(Preparation::Ready(plan)) => {
                    // RangePlan fields are pub(crate): Debug emits them
                    // verbatim (audit is a Value -> compact sorted JSON).
                    let mut o = json!({"seq":seq,"op":op,"status":"ok","detail":{
                        "kind":"ready","no_change":plan.is_no_change(),
                        "command":serde_json::to_string(plan.command()).unwrap(),
                        "debug":format!("{:?}", plan)}});
                    o["calls"] = stub.calls();
                    o
                }
                Err(e) => {
                    let mut o = json!({"seq":seq,"op":op,"status":"err","code":err_kind(&e),"message":e.to_string()});
                    o["calls"] = stub.calls();
                    o
                }
            }
        }
        "run_items" => {
            let stub = StubRepo::new(c["stub"].clone());
            let runner = RangeRunner { repository: stub.clone(), run_id: c["run_id"].as_str().unwrap().to_string() };
            let provider = StubProvider { cfg: c.get("provider").cloned().unwrap_or(json!({})) };
            let ids: Vec<String> = c["item_ids"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
            match runner.run_items(c["task_id"].as_str().unwrap(), &ids, &provider) {
                Ok(r) => {
                    let mut o = json!({"seq":seq,"op":op,"status":"ok",
                        "detail":{"result": if r.is_some() {"some"} else {"none"}}});
                    o["calls"] = stub.calls();
                    o
                }
                Err(e) => {
                    let mut o = json!({"seq":seq,"op":op,"status":"err","code":err_kind(&e),"message":e.to_string()});
                    o["calls"] = stub.calls();
                    o
                }
            }
        }
        "run_item" => {
            let stub = StubRepo::new(c["stub"].clone());
            let runner = RangeRunner { repository: stub.clone(), run_id: c["run_id"].as_str().unwrap().to_string() };
            let provider = StubProvider { cfg: c.get("provider").cloned().unwrap_or(json!({})) };
            match runner.run_item(c["task_id"].as_str().unwrap(), c["item_id"].as_str().unwrap(), &provider) {
                Ok(ItemExecution::Published(r)) => {
                    let mut o = json!({"seq":seq,"op":op,"status":"ok",
                        "detail":{"kind":"published","receipt":serde_json::to_value(r).unwrap()}});
                    o["calls"] = stub.calls();
                    o
                }
                Ok(ItemExecution::NotPublished(s)) => {
                    let mut snap = serde_json::to_value(&*s).unwrap();
                    scrub(&mut snap);
                    let mut o = json!({"seq":seq,"op":op,"status":"ok",
                        "detail":{"kind":"not_published","snapshot":snap}});
                    o["calls"] = stub.calls();
                    o
                }
                Err(e) => {
                    let mut o = json!({"seq":seq,"op":op,"status":"err","code":err_kind(&e),"message":e.to_string()});
                    o["calls"] = stub.calls();
                    o
                }
            }
        }
        _ => out!(err, format!("unknown op {op}")),
    }
}
fn main() {
    let input: Box<dyn BufRead> = match std::env::args().nth(1) {
        Some(p) => Box::new(BufReader::new(std::fs::File::open(p).unwrap())),
        None => Box::new(BufReader::new(std::io::stdin())),
    };
    let stdout = std::io::stdout();
    let mut w = std::io::BufWriter::new(stdout.lock());
    for (i, line) in input.lines().enumerate() {
        let line = line.unwrap();
        if line.trim().is_empty() {
            continue;
        }
        let case: Value = serde_json::from_str(&line).unwrap();
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_case(i as u64, &case)));
        let row = match out {
            Ok(v) => v,
            Err(_) => json!({"seq":i,"op":case["op"],"status":"panic"}),
        };
        writeln!(w, "{}", serde_json::to_string(&row).unwrap()).unwrap();
    }
}
