//! Consistent task observations and bounded, authorization-bound snapshots.
use super::{
    Access,
    query::{MAX_BYTES, invalid, page_size},
    wire::*,
};
use crate::progress_trace::{Span, Stage, Trace};
use crate::{market_storage::Error, task_control::TaskSnapshot};
use chrono::{DateTime, Datelike, Utc};
use std::{
    cell::RefCell,
    collections::VecDeque,
    time::{Duration, Instant},
};
/// Tasks with more items than this switch to bounded reads: summary and
/// progress surfaces serve task-head columns plus a paginated
/// not-yet-successful item slice instead of materializing
/// `frozen_work_json` items and per-item output rows. Explicit v6-era
/// tasks are capped at 100 items, so the threshold never changes their
/// response shapes; plan-run tasks can carry thousands.
pub const LARGE_TASK_ITEMS: u64 = 100;
/// Upper bound on one `get_task_progress` server-side wait, in seconds.
/// Larger `wait_seconds` values are clamped to this (never rejected) so a
/// single call cannot occupy its connection thread indefinitely.
pub const MAX_WAIT_SECONDS: u32 = 50;
/// Long-poll re-read cadence (the 300–500ms window in the contract).
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(400);
/// Server wait budget for a caller-supplied `wait_seconds`: `None` when no
/// wait was requested (absent or 0 — the immediate-return behavior),
/// otherwise `min(seconds, 50)`.
fn wait_budget(seconds: u32) -> Option<Duration> {
    (seconds > 0).then_some(Duration::from_secs(u64::from(
        seconds.min(MAX_WAIT_SECONDS),
    )))
}
#[derive(Clone)]
pub struct TaskFacts {
    pub request_id: String,
    pub snapshot: TaskSnapshot,
    pub publications: Vec<VersionRef>,
    pub range_results: Vec<RangeExecutionResult>,
    pub yearly_results: Vec<YearlyExecutionResult>,
}
/// Head-only task facts for bounded read surfaces (PLANRUN-BOUNDED-READS):
/// every `summary` field and progress counter comes from `tasks` row
/// columns plus `shared_evidence` extracted at the DB layer — no
/// `frozen_work_json` item decode, no per-item output enumeration.
#[derive(Clone)]
pub struct TaskHeadFacts {
    pub request_id: String,
    pub head: crate::task_control::TaskHead,
    pub created_at: DateTime<Utc>,
    /// `frozen_work.shared_evidence` verbatim (`None` when the task
    /// carries none). For plan-run tasks it holds `plan_id`,
    /// `generation_id`, `target_day`, `counts`, `ineligible_reasons`.
    pub shared_evidence: Option<serde_json::Value>,
}
pub struct AvailabilityFacts {
    pub entity: Option<Security>,
    pub manifest: Option<ManifestEntry>,
    pub first_record: Option<chrono::NaiveDate>,
    pub last_record: Option<chrono::NaiveDate>,
    pub record_count: usize,
    pub integrity_failure: Option<String>,
    pub proven_gaps: Vec<crate::market_storage::ProvenGap>,
    /// Shared task facts: one availability call relates the same task set to
    /// every selected entity, so entries are shared rather than deep-cloned.
    pub related_tasks: Vec<std::rc::Rc<TaskHeadFacts>>,
    /// Related tasks before the in-flight + latest-N cut (XP-025 D-b).
    pub related_tasks_total: usize,
}
/// Domain classification: storage reports facts, never decides availability policy.
fn availability_item(
    f: AvailabilityFacts,
    dataset: Dataset,
    run: &str,
    range: &Filter,
) -> Result<AvailabilityItem, Error> {
    let unavailable = f.integrity_failure.is_some();
    let availability = if unavailable {
        Availability::Unavailable
    } else if f
        .manifest
        .as_ref()
        .is_some_and(|m| m.status == ManifestStatus::Absent)
    {
        Availability::Absent
    } else if dataset == Dataset::Catalog {
        Availability::Available
    } else {
        Availability::Unknown
    };
    let coverage_evidence = f.integrity_failure.unwrap_or_else(|| {
        f.manifest
            .as_ref()
            .and_then(|m| m.metadata.as_ref())
            .map(|m| m.coverage_reason.clone())
            .unwrap_or_else(|| "no published version".into())
    });
    let (stale, as_of_day, reason) = f
        .manifest
        .as_ref()
        .map(|m| (m.stale.clone(), m.as_of_day, m.reason))
        .unwrap_or((None, None, None));
    let confirmed_gaps = f
        .proven_gaps
        .iter()
        .map(confirmed_gap)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(AvailabilityItem{entity:f.entity,version_ref:f.manifest.and_then(|m|m.version_ref),integrity:if unavailable{"failed"}else if availability==Availability::Absent{"not_applicable"}else{"verified"}.into(),confirmed_gaps,unresolved_ranges:if availability==Availability::Unknown{vec![range.clone()]}else{vec![]},availability,coverage_evidence,first_record:f.first_record,last_record:f.last_record,record_count:f.record_count,freshness:"unknown".into(),related_tasks_total:f.related_tasks_total,related_tasks_truncated:f.related_tasks_total>f.related_tasks.len(),related_tasks:f.related_tasks.iter().map(|f|summary_head(f,run)).collect(),stale,as_of_day,reason,limitations:vec!["range completeness is not inferred from dates or version coverage alone; no implicit collection or repair".into()]})
}
/// Canonical wire projection of a stored proven gap: the evidence string only
/// references durable evidence identity (kind/id/sha256, fixed order) and never
/// verifies or interprets the evidence object itself.
fn confirmed_gap(g: &crate::market_storage::ProvenGap) -> Result<ConfirmedGap, Error> {
    #[derive(serde::Serialize)]
    struct EvidenceRef<'a> {
        kind: &'a str,
        id: &'a str,
        sha256: &'a str,
    }
    let range = match g.period {
        crate::market_data::Period::Day => Filter::Daily {
            start_date: g.start_date,
            end_date: g.end_date,
        },
        crate::market_data::Period::Year => {
            let start = chrono::NaiveDate::from_ymd_opt(g.start_date.year(), 1, 1);
            let end = chrono::NaiveDate::from_ymd_opt(g.end_date.year(), 12, 31);
            if start != Some(g.start_date) || end != Some(g.end_date) {
                return Err(Error::Integrity(
                    "yearly proven gap is not a natural-year range".into(),
                ));
            }
            Filter::Yearly {
                start_year: g.start_date.year(),
                end_year: g.end_date.year(),
            }
        }
    };
    Ok(ConfirmedGap {
        range,
        evidence: serde_json::to_string(&EvidenceRef {
            kind: &g.evidence_kind,
            id: &g.evidence_id,
            sha256: &g.evidence_digest,
        })
        .map_err(|e| Error::Integrity(format!("proven gap evidence: {e}")))?,
    })
}
/// The read surface one progress RPC needs: task facts plus every
/// verification the response still owes. Implementations may place all
/// six calls inside a single storage read transaction so verified work
/// can be reused *within* that snapshot — never across requests, pages,
/// commits or databases (XP-016). It is a transaction scope, not a
/// trust cache: every verifier and comparison still runs.
pub trait ProgressRead {
    fn task(&self, id: &str, access: &Access) -> Result<TaskFacts, Error>;
    /// Head-only facts for the bounded progress path
    /// (PLANRUN-BOUNDED-READS). The default rebuilds them from `task`
    /// so non-SQLite repositories keep working unchanged.
    fn task_head_facts(&self, id: &str, access: &Access) -> Result<TaskHeadFacts, Error> {
        let f = self.task(id, access)?;
        Ok(TaskHeadFacts {
            request_id: f.request_id,
            created_at: f.snapshot.task.created_at,
            head: f.snapshot.head(),
            shared_evidence: f.snapshot.task.work.shared_evidence,
        })
    }
    /// One keyset slice of not-yet-successful items — `item_id > after`,
    /// at most `limit` rows — for the bounded progress path. The default
    /// filters `task`'s materialized items.
    fn unfinished_items(
        &self,
        id: &str,
        after: Option<&str>,
        limit: usize,
        access: &Access,
    ) -> Result<Vec<crate::task_control::TaskItem>, Error> {
        let mut items = self
            .task(id, access)?
            .snapshot
            .items
            .into_iter()
            .filter(|i| !i.outcome.as_ref().is_some_and(|o| o.success))
            .filter(|i| after.is_none_or(|a| i.spec.item_id.as_str() > a))
            .collect::<Vec<_>>();
        items.sort_by(|a, b| a.spec.item_id.cmp(&b.spec.item_id));
        items.truncate(limit);
        Ok(items)
    }
    /// The first `limit` failed items by `item_id` (v8 PR-5, #387 §14): the
    /// bounded sample a v2 `get_task` diagnosis carries. The default filters
    /// `task`'s materialized items.
    fn failed_items(
        &self,
        id: &str,
        limit: usize,
        access: &Access,
    ) -> Result<Vec<crate::task_control::TaskItem>, Error> {
        let mut items = self
            .task(id, access)?
            .snapshot
            .items
            .into_iter()
            .filter(|i| i.outcome.as_ref().is_some_and(|o| !o.success))
            .collect::<Vec<_>>();
        items.sort_by(|a, b| a.spec.item_id.cmp(&b.spec.item_id));
        items.truncate(limit);
        Ok(items)
    }
    /// Failed items grouped by `reason_code`, for the bounded progress
    /// path. The default counts `task`'s materialized items.
    fn failed_reason_counts(
        &self,
        id: &str,
        access: &Access,
    ) -> Result<super::task_v2::FailedByReason, Error> {
        let mut counts = std::collections::BTreeMap::new();
        for item in self.task(id, access)?.snapshot.items {
            if let Some(outcome) = item.outcome.as_ref().filter(|o| !o.success) {
                *counts
                    .entry(reason_code(outcome.error.as_deref().unwrap_or("")).to_owned())
                    .or_insert(0) += 1;
            }
        }
        Ok(counts)
    }
    fn verify_versions(&self, refs: &[VersionRef]) -> Result<(), Error>;
    fn verify_range_results(&self, results: &[RangeExecutionResult]) -> Result<(), Error>;
    fn verify_yearly_results(&self, results: &[YearlyExecutionResult]) -> Result<(), Error>;
    fn verify_publication_check(
        &self,
        receipt: &Option<crate::publication_verification::decision::Receipt>,
    ) -> Result<(), Error>;
    fn verify_stopped_check(
        &self,
        receipt: &Option<crate::task_verification::decision::Receipt>,
    ) -> Result<(), Error>;
}
/// One bounded slice of the task list in `(created_at DESC, task_id ASC)`
/// order, resumed by keyset (`after` = text of `created_at` + `task_id`).
/// Filters here are coarse (stored values); the observer applies the exact
/// ones on the visible summary.
#[derive(Clone, Debug, Default)]
pub struct TaskScan {
    pub dataset: Option<String>,
    pub operation: Option<String>,
    /// Stored statuses that can possibly match (`None` = any).
    pub stored_statuses: Option<Vec<String>>,
    pub created_from: Option<DateTime<Utc>>,
    pub created_to: Option<DateTime<Utc>>,
    /// Snapshot bound: tasks created after the first page are never listed.
    pub upper: Option<DateTime<Utc>>,
    pub after: Option<(String, String)>,
    pub batch: usize,
}
pub struct TaskScanBatch {
    /// Head-only facts: `list_tasks` never materializes item work.
    pub facts: Vec<TaskHeadFacts>,
    /// Keyset to continue from; `None` once the scan is exhausted.
    pub next_after: Option<(String, String)>,
}
pub trait ObservationRepository {
    fn tasks(&self, access: &Access) -> Result<Vec<TaskFacts>, Error>;
    /// Bounded task listing. The default pages the full `tasks()` list in
    /// memory (test doubles); the SQLite repository pushes it into SQL.
    fn tasks_scan(&self, scan: &TaskScan, access: &Access) -> Result<TaskScanBatch, Error> {
        let mut all = self.tasks(access)?;
        all.sort_by(|a, b| {
            b.snapshot
                .task
                .created_at
                .cmp(&a.snapshot.task.created_at)
                .then(a.snapshot.task.task_id.cmp(&b.snapshot.task.task_id))
        });
        let key = |f: &TaskFacts| {
            (
                f.snapshot.task.created_at.to_rfc3339(),
                f.snapshot.task.task_id.clone(),
            )
        };
        let facts: Vec<TaskHeadFacts> = all
            .into_iter()
            .filter(|f| {
                scan.upper.is_none_or(|u| f.snapshot.task.created_at <= u)
                    && scan.after.as_ref().is_none_or(|(at, id)| {
                        let (c, t) = key(f);
                        c.as_str() < at.as_str() || (c == *at && t.as_str() > id.as_str())
                    })
            })
            .map(|f| TaskHeadFacts {
                request_id: f.request_id,
                created_at: f.snapshot.task.created_at,
                head: f.snapshot.head(),
                shared_evidence: f.snapshot.task.work.shared_evidence,
            })
            .collect();
        let more = facts.len() > scan.batch;
        let mut facts = facts;
        facts.truncate(scan.batch);
        let next_after = more
            .then(|| {
                facts
                    .last()
                    .map(|f| (f.created_at.to_rfc3339(), f.head.task_id.clone()))
            })
            .flatten();
        Ok(TaskScanBatch { facts, next_after })
    }
    /// Bounded head-only facts for summary surfaces (`get_task`,
    /// `list_tasks`): `tasks` row columns plus `shared_evidence`, without
    /// `frozen_work_json` item materialization. The default rebuilds it
    /// from `task` so non-SQLite repositories keep working unchanged;
    /// the SQLite repository reads columns only.
    fn task_head_facts(&self, id: &str, access: &Access) -> Result<TaskHeadFacts, Error> {
        let f = self.task(id, access)?;
        Ok(TaskHeadFacts {
            request_id: f.request_id,
            created_at: f.snapshot.task.created_at,
            head: f.snapshot.head(),
            shared_evidence: f.snapshot.task.work.shared_evidence,
        })
    }
    /// Failed items grouped by `reason_code`, for the `list_tasks` v2
    /// `reason_code` filter. The default reads the full task; the SQLite
    /// backend reads only failed rows (one narrow query per candidate task —
    /// the filter is a targeted discovery aid, not the default list path).
    fn failed_reason_counts(
        &self,
        id: &str,
        access: &Access,
    ) -> Result<super::task_v2::FailedByReason, Error> {
        let mut counts = std::collections::BTreeMap::new();
        for item in self.task(id, access)?.snapshot.items {
            if let Some(outcome) = item.outcome.as_ref().filter(|o| !o.success) {
                *counts
                    .entry(reason_code(outcome.error.as_deref().unwrap_or("")).to_owned())
                    .or_insert(0) += 1;
            }
        }
        Ok(counts)
    }
    fn task(&self, id: &str, access: &Access) -> Result<TaskFacts, Error>;
    fn availability(
        &self,
        plan: &super::query::QueryPlan,
        access: &Access,
    ) -> Result<Vec<AvailabilityFacts>, Error>;
    fn verify_versions(&self, refs: &[VersionRef]) -> Result<(), Error>;
    /// `verify_versions` at the committed-state tier (XP-027 D-a3), for
    /// metadata-only observations (availability). Same expected == actual
    /// `VersionRef` comparison; backends may skip chain reconstruction.
    /// Content reads (`query_data`) keep the full tier.
    fn verify_versions_committed(&self, refs: &[VersionRef]) -> Result<(), Error> {
        self.verify_versions(refs)
    }
    fn verify_range_results(&self, results: &[RangeExecutionResult]) -> Result<(), Error>;
    fn verify_yearly_results(&self, results: &[YearlyExecutionResult]) -> Result<(), Error>;
    fn verify_publication_check(
        &self,
        receipt: &Option<crate::publication_verification::decision::Receipt>,
    ) -> Result<(), Error>;
    fn verify_stopped_check(
        &self,
        receipt: &Option<crate::task_verification::decision::Receipt>,
    ) -> Result<(), Error>;
    /// One progress RPC scope. The default wraps `self` in a forwarding
    /// `ProgressRead` adapter — behavior identical to calling the six
    /// repository methods directly — so non-SQLite repositories keep
    /// working unchanged. Backends may override to run the whole RPC
    /// inside one read transaction.
    fn with_progress_read<T>(
        &self,
        f: impl FnOnce(&dyn ProgressRead) -> Result<T, Error>,
    ) -> Result<T, Error>
    where
        Self: Sized,
    {
        struct Forward<'a, R: ObservationRepository + ?Sized>(&'a R);
        impl<R: ObservationRepository + ?Sized> ProgressRead for Forward<'_, R> {
            fn task(&self, id: &str, access: &Access) -> Result<TaskFacts, Error> {
                self.0.task(id, access)
            }
            fn task_head_facts(&self, id: &str, access: &Access) -> Result<TaskHeadFacts, Error> {
                self.0.task_head_facts(id, access)
            }
            fn unfinished_items(
                &self,
                id: &str,
                after: Option<&str>,
                limit: usize,
                access: &Access,
            ) -> Result<Vec<crate::task_control::TaskItem>, Error> {
                let mut items = self
                    .0
                    .task(id, access)?
                    .snapshot
                    .items
                    .into_iter()
                    .filter(|i| !i.outcome.as_ref().is_some_and(|o| o.success))
                    .filter(|i| after.is_none_or(|a| i.spec.item_id.as_str() > a))
                    .collect::<Vec<_>>();
                items.sort_by(|a, b| a.spec.item_id.cmp(&b.spec.item_id));
                items.truncate(limit);
                Ok(items)
            }
            fn verify_versions(&self, refs: &[VersionRef]) -> Result<(), Error> {
                self.0.verify_versions(refs)
            }
            fn verify_range_results(&self, results: &[RangeExecutionResult]) -> Result<(), Error> {
                self.0.verify_range_results(results)
            }
            fn verify_yearly_results(
                &self,
                results: &[YearlyExecutionResult],
            ) -> Result<(), Error> {
                self.0.verify_yearly_results(results)
            }
            fn verify_publication_check(
                &self,
                receipt: &Option<crate::publication_verification::decision::Receipt>,
            ) -> Result<(), Error> {
                self.0.verify_publication_check(receipt)
            }
            fn verify_stopped_check(
                &self,
                receipt: &Option<crate::task_verification::decision::Receipt>,
            ) -> Result<(), Error> {
                self.0.verify_stopped_check(receipt)
            }
        }
        f(&Forward(self))
    }
}
pub fn entity(symbol: &str) -> Option<Security> {
    let (ex, code) = symbol.split_once('.')?;
    Some(Security {
        exchange: match ex {
            "sh" => Exchange::Sh,
            "sz" => Exchange::Sz,
            _ => return None,
        },
        code: code.into(),
    })
}
pub fn authorized(s: &TaskSnapshot, a: &Access) -> bool {
    if s.task.key.dataset == crate::task_verification::DATASET {
        return *a == Access::local();
    }
    let datasets = match s.task.key.dataset.as_str() {
        "market.daily" => vec![Dataset::Daily],
        "market.yearly" => vec![Dataset::Yearly],
        "securities.catalog" => vec![Dataset::Catalog],
        "legacy_market_compound" => s
            .items
            .iter()
            .map(|i| {
                if i.spec.target["period"] == "day" {
                    Dataset::Daily
                } else {
                    Dataset::Yearly
                }
            })
            .collect(),
        _ => return false,
    };
    datasets.into_iter().all(|d| a.allows(d, None))
        && s.items.iter().all(|i| {
            i.spec.target["symbol"].as_str().is_none_or(|s| {
                entity(s).is_some_and(|e| {
                    a.securities
                        .as_ref()
                        .is_none_or(|allowed| allowed.contains(&e))
                })
            })
        })
}
pub fn summary(f: &TaskFacts, run: &str) -> TaskSummary {
    let t = &f.snapshot.task;
    // Plan-run tasks carry the verified plan digest in shared_evidence;
    // surface it so progress reads can correlate task ↔ plan.
    let plan = (t.key.contract_version == crate::data_supply::plan::RUN_CONTRACT_VERSION)
        .then_some(f.snapshot.task.work.shared_evidence.as_ref())
        .flatten();
    TaskSummary {
        task_id: t.task_id.clone(),
        request_id: f.request_id.clone(),
        dataset: t.key.dataset.clone(),
        operation: t.key.operation.clone(),
        contract_version: t.key.contract_version.clone(),
        status: serde_json::from_value(
            serde_json::to_value(f.snapshot.visible_status(run)).unwrap(),
        )
        .unwrap(),
        state_revision: t.state_revision,
        created_at: t.created_at,
        last_progress_at: t.last_progress_at,
        plan_id: plan.and_then(|e| e["plan_id"].as_str()).map(str::to_owned),
        plan_counts: plan.and_then(|e| serde_json::from_value(e["counts"].clone()).ok()),
    }
}
/// `summary` at head depth (PLANRUN-BOUNDED-READS): identical fields —
/// the `TaskHead` projection and DB-extracted `shared_evidence` carry
/// everything the full snapshot carried — without item materialization.
pub fn summary_head(f: &TaskHeadFacts, run: &str) -> TaskSummary {
    let h = &f.head;
    let plan = (h.key.contract_version == crate::data_supply::plan::RUN_CONTRACT_VERSION)
        .then_some(f.shared_evidence.as_ref())
        .flatten();
    TaskSummary {
        task_id: h.task_id.clone(),
        request_id: f.request_id.clone(),
        dataset: h.key.dataset.clone(),
        operation: h.key.operation.clone(),
        contract_version: h.key.contract_version.clone(),
        status: serde_json::from_value(serde_json::to_value(h.visible_status(run)).unwrap())
            .unwrap(),
        state_revision: h.state_revision,
        created_at: f.created_at,
        last_progress_at: h.last_progress_at,
        plan_id: plan.and_then(|e| e["plan_id"].as_str()).map(str::to_owned),
        plan_counts: plan.and_then(|e| serde_json::from_value(e["counts"].clone()).ok()),
    }
}
#[derive(Clone)]
enum Content {
    /// Keyset position of a `list_tasks` snapshot (XP-025 D): the list
    /// itself is never materialized.
    TaskCursor(Option<(String, String)>),
    /// Failed items grouped by `reason_code`, counted once per fresh call so
    /// every cursor page of the same snapshot reports the same classification
    /// (v8 PR-5, #387 §14).
    Progress {
        page: Box<ProgressPage>,
        failed: super::task_v2::FailedByReason,
    },
    /// Bounded large-task progress (PLANRUN-BOUNDED-READS): `page` keeps
    /// the small head summary with `unfinished` emptied; `after` is the
    /// keyset position — the last emitted unfinished item_id. Cursor
    /// pages re-query `task_items` by `item_id > after`, so pagination
    /// never re-materializes the item set.
    ProgressKeyset {
        page: Box<ProgressPage>,
        after: Option<String>,
        failed: super::task_v2::FailedByReason,
    },
    Availability(Vec<AvailabilityItem>),
}
#[derive(Clone)]
struct Saved {
    token: String,
    key: String,
    access: Access,
    time: DateTime<Utc>,
    id: String,
    offset: usize,
    content: Content,
}
pub struct Observer<R> {
    pub repository: R,
    pub run_id: String,
    cache: RefCell<VecDeque<Saved>>,
    sequence: RefCell<u64>,
    instance: String,
}
impl<R: ObservationRepository> Observer<R> {
    pub fn new(repository: R, run_id: String) -> Self {
        Self {
            repository,
            run_id,
            cache: RefCell::new(VecDeque::new()),
            sequence: RefCell::new(0),
            instance: format!(
                "obs-{}-{}",
                std::process::id(),
                Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ),
        }
    }
    fn id(&self) -> String {
        let mut n = self.sequence.borrow_mut();
        *n += 1;
        format!("{}-{n}", self.instance)
    }
    fn start(&self, key: String, access: &Access, content: Content) -> Saved {
        Saved {
            token: String::new(),
            key,
            access: access.clone(),
            time: Utc::now(),
            id: self.id(),
            offset: 0,
            content,
        }
    }
    fn resume(&self, token: &str, key: &str, access: &Access) -> Result<Saved, Error> {
        let s = self
            .cache
            .borrow()
            .iter()
            .find(|s| s.token == token)
            .cloned()
            .ok_or_else(|| invalid("invalid/expired cursor"))?;
        if s.key != key || &s.access != access {
            return Err(invalid(&super::query::cursor_mismatch(&s.key, key)));
        }
        Ok(s)
    }
    fn save(&self, mut s: Saved) -> String {
        let token = self.id();
        s.token = token.clone();
        let mut c = self.cache.borrow_mut();
        if c.len() == 32 {
            c.pop_front();
        }
        c.push_back(s);
        token
    }
    pub fn tasks(&self, args: TasksQuery, access: &Access) -> Result<TaskListPage, Error> {
        self.tasks_page(args, access)
    }
    /// v2 task list (v8 PR-5, #387 §14): the v1 page, the wire contract
    /// version, and the per-row rename that keeps the task's own contract
    /// version apart from the wire one (§7.2 row 14).
    pub fn tasks_v2(&self, args: TasksQuery, access: &Access) -> Result<serde_json::Value, Error> {
        let page = self.tasks_page(args, access)?;
        let TaskListPage {
            snapshot_id,
            snapshot_time,
            items,
            next_cursor,
            page_complete,
        } = page;
        Ok(serde_json::json!({
            "wire_contract_version": ReadContractVersion::V2,
            "snapshot_id": snapshot_id,
            "snapshot_time": snapshot_time,
            "items": items.into_iter().map(super::task_v2::task_summary).collect::<Vec<_>>(),
            "next_cursor": next_cursor,
            "page_complete": page_complete,
        }))
    }
    /// v2 `get_task` (v8 PR-5, #387 §14): the summary, the counters and the
    /// bounded failure diagnosis in one read scope, so the counts and the
    /// sample cannot disagree and T6 needs one call instead of three.
    pub fn task_v2(&self, task_id: &str, access: &Access) -> Result<serde_json::Value, Error> {
        self.repository.with_progress_read(|read| {
            let head = read.task_head_facts(task_id, access)?;
            let counts = TaskCounts {
                total: head.head.counts.total as usize,
                pending: head.head.counts.pending as usize,
                running: head.head.counts.running as usize,
                finished: head.head.counts.finished as usize,
                succeeded: head.head.counts.succeeded as usize,
                failed: head
                    .head
                    .counts
                    .finished
                    .saturating_sub(head.head.counts.succeeded) as usize,
            };
            let mut failed = std::collections::BTreeMap::new();
            let mut sample = vec![];
            if counts.failed > 0 {
                failed = read.failed_reason_counts(task_id, access)?;
                sample = read
                    .failed_items(task_id, super::task_v2::ERROR_SAMPLE, access)?
                    .iter()
                    .map(progress_item)
                    .collect();
            }
            Ok(super::task_v2::task_view(
                summary_head(&head, &self.run_id),
                counts,
                failed,
                &sample,
            ))
        })
    }
    fn tasks_page(&self, mut args: TasksQuery, access: &Access) -> Result<TaskListPage, Error> {
        page_size(args.page_size)?;
        // v2 only (`reason_code` filters on item failures, which v1 never
        // reads): reject instead of silently ignoring it.
        if args.contract_version.is_v1() && args.reason_code.is_some() {
            return Err(invalid("reason_code requires contract_version v2"));
        }
        if let Some(code) = &args.reason_code
            && !super::task_v2::REASON_CODES.contains(&code.as_str())
        {
            return Err(invalid(&format!(
                "reason_code must be one of {}; got {code:?}",
                super::task_v2::REASON_CODES.join(", ")
            )));
        }
        if args
            .created_from
            .zip(args.created_to)
            .is_some_and(|(a, b)| a > b)
        {
            return Err(invalid("inverted task time range"));
        }
        if let Some(d) = args.dataset {
            access.require(d, None)?;
        }
        let cursor = args.cursor.take();
        let key = format!("tasks:{}", serde_json::to_string(&args).unwrap());
        let mut s = if let Some(c) = cursor {
            self.resume(&c, &key, access)?
        } else {
            self.start(key, access, Content::TaskCursor(None))
        };
        let Content::TaskCursor(after) = &s.content else {
            return Err(invalid("cursor category"));
        };
        let mut after = after.clone();
        let wire_string = |v: serde_json::Value| v.as_str().map(str::to_owned);
        let scan = |after: Option<(String, String)>| TaskScan {
            dataset: args
                .dataset
                .and_then(|d| wire_string(serde_json::to_value(d).unwrap())),
            operation: args
                .operation
                .and_then(|o| wire_string(serde_json::to_value(o).unwrap())),
            // The visible status can differ from the stored one (an old
            // run's active task reads as needs_verification), so the SQL
            // prefilter only drops terminal statuses that cannot match.
            stored_statuses: args.status.as_ref().map(|wanted| {
                let mut stored: Vec<String> = wanted
                    .iter()
                    .filter_map(|v| wire_string(serde_json::to_value(v).unwrap()))
                    .collect();
                for active in [
                    "pending",
                    "running",
                    "cancel_requested",
                    "needs_verification",
                ] {
                    if wanted.iter().any(|v| {
                        matches!(
                            v,
                            TaskStatus::Pending
                                | TaskStatus::Running
                                | TaskStatus::CancelRequested
                                | TaskStatus::NeedsVerification
                        )
                    }) && !stored.iter().any(|s| s == active)
                    {
                        stored.push(active.to_string());
                    }
                }
                stored
            }),
            created_from: args.created_from,
            created_to: args.created_to,
            upper: Some(s.time),
            after,
            batch: (args.page_size + 1).max(64),
        };
        let mut matched: Vec<TaskSummary> = vec![];
        'scan: loop {
            let batch = self.repository.tasks_scan(&scan(after.clone()), access)?;
            for f in &batch.facts {
                let sm = summary_head(f, &self.run_id);
                if args
                    .dataset
                    .is_none_or(|d| serde_json::to_value(d).unwrap() == sm.dataset)
                    && args
                        .operation
                        .is_none_or(|op| serde_json::to_value(op).unwrap() == sm.operation)
                    && args.status.as_ref().is_none_or(|v| v.contains(&sm.status))
                    && args.created_from.is_none_or(|t| sm.created_at >= t)
                    && args.created_to.is_none_or(|t| sm.created_at <= t)
                    && self.matches_reason(&sm.task_id, args.reason_code.as_deref(), access)?
                {
                    matched.push(sm);
                    if matched.len() > args.page_size {
                        break 'scan;
                    }
                }
            }
            match batch.next_after {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        let more = matched.len() > args.page_size;
        matched.truncate(args.page_size);
        budget(&matched)?;
        let mut p = TaskListPage {
            snapshot_id: s.id.clone(),
            snapshot_time: s.time,
            items: matched,
            next_cursor: None,
            page_complete: !more,
        };
        if more {
            let last = p.items.last().expect("page_size >= 1");
            s.content =
                Content::TaskCursor(Some((last.created_at.to_rfc3339(), last.task_id.clone())));
            p.next_cursor = Some(self.save(s));
        }
        Ok(p)
    }
    /// Whether a task has at least one failed item of the requested class
    /// (`list_tasks` v2 filter). One narrow failed-row read per candidate task
    /// on the filter path only; the unfiltered list stays head-only.
    fn matches_reason(
        &self,
        id: &str,
        reason: Option<&str>,
        access: &Access,
    ) -> Result<bool, Error> {
        match reason {
            None => Ok(true),
            Some(code) => Ok(self
                .repository
                .failed_reason_counts(id, access)?
                .contains_key(code)),
        }
    }
    /// Server-side long-poll for `get_task_progress`: while the task is
    /// non-terminal and the caller asked to wait, re-read only the task
    /// head (no item materialization) until the task reaches a terminal
    /// status, its `state_revision` moves past `since_revision`, or the
    /// clamped budget expires. Each iteration is its own short-lived read
    /// scope — no read transaction is held across the sleep, so the worker
    /// thread is never blocked. A missing or unauthorized task fails
    /// immediately through the same head read (never waited on).
    fn wait_for_progress(&self, args: &ProgressQuery, access: &Access) -> Result<(), Error> {
        let Some(budget) = wait_budget(args.wait_seconds.unwrap_or(0)) else {
            return Ok(());
        };
        let deadline = Instant::now() + budget;
        let mut baseline = args.since_revision;
        loop {
            let head = self.repository.task_head_facts(&args.task_id, access)?.head;
            if head.status.terminal() {
                return Ok(());
            }
            match baseline {
                Some(revision) if head.state_revision > revision => return Ok(()),
                Some(_) => {}
                // No caller baseline: the revision read at wait entry is it.
                None => baseline = Some(head.state_revision),
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(());
            }
            std::thread::sleep(WAIT_POLL_INTERVAL.min(deadline - now));
        }
    }
    pub fn progress(&self, args: ProgressQuery, access: &Access) -> Result<ProgressPage, Error> {
        Ok(self.progress_page(args, access)?.0)
    }
    /// v2 progress (v8 PR-5, #387 §14): the v1 page with structured item
    /// identities and one top-level `failed_by_reason`.
    pub fn progress_v2(
        &self,
        args: ProgressQuery,
        access: &Access,
    ) -> Result<serde_json::Value, Error> {
        let (page, failed) = self.progress_page(args, access)?;
        super::task_v2::progress_view(page, failed)
    }
    /// The page plus its failure classification; v1 keeps only the page and
    /// the classification reaches it through `plan.failed_by_reason` (bounded
    /// tasks), exactly as before.
    fn progress_page(
        &self,
        mut args: ProgressQuery,
        access: &Access,
    ) -> Result<(ProgressPage, super::task_v2::FailedByReason), Error> {
        let trace = Trace::begin();
        page_size(args.page_size)?;
        let cursor = args.cursor.take();
        // Long-poll before taking the read scope: the wait loop opens and
        // closes its own head-only reads, so no transaction is held across
        // a sleep. Cursor pages are a continuation and never wait.
        if cursor.is_none() {
            self.wait_for_progress(&args, access)?;
        }
        // Cursor identity must not depend on the wait parameters: a fresh
        // call may carry them while its continuation pages need not.
        let mut key_args = args.clone();
        key_args.wait_seconds = None;
        key_args.since_revision = None;
        let key = format!("progress:{}", serde_json::to_string(&key_args).unwrap());
        // One RPC (fresh or cursor page) = one read scope: backends may
        // consolidate all storage reads below into a single transaction
        // (XP-016). Cursor identity/authorization/expiry checks and the
        // fresh-assembly policy are unchanged.
        let result = self
            .repository
            .with_progress_read(|read| self.progress_in(read, key, cursor, args, access));
        trace.finish(result.is_ok());
        result
    }
    fn progress_in(
        &self,
        read: &dyn ProgressRead,
        key: String,
        cursor: Option<String>,
        args: ProgressQuery,
        access: &Access,
    ) -> Result<(ProgressPage, super::task_v2::FailedByReason), Error> {
        let mut s = if let Some(c) = cursor {
            self.resume(&c, &key, access)?
        } else {
            // Bounded path (PLANRUN-BOUNDED-READS): a head-only read decides
            // the shape. Tasks above `LARGE_TASK_ITEMS` never materialize
            // items — counters come from the tasks row and `unfinished`
            // is served by keyset pages below.
            let head_facts = {
                let _span = Span::enter(Stage::TaskAssembly);
                read.task_head_facts(&args.task_id, access)?
            };
            let plan_items = head_facts
                .shared_evidence
                .as_ref()
                .and_then(|evidence| evidence["counts"]["eligible"].as_u64());
            if bounded_progress(head_facts.head.counts.total, plan_items) {
                let finished = head_facts.head.counts.finished as usize;
                let succeeded = head_facts.head.counts.succeeded as usize;
                let total = head_facts.head.counts.total as usize;
                let mut plan = plan_progress(&head_facts.shared_evidence);
                // v8 PR-5 (#387 §14): the classification is computed for the
                // top-level v2 field; v1 keeps reading it under `plan`, so the
                // v1 shape and cost are unchanged when nothing failed.
                let failed = if finished > succeeded {
                    read.failed_reason_counts(&args.task_id, access)?
                } else {
                    Default::default()
                };
                if let Some(plan) = plan.as_mut() {
                    plan.failed_by_reason = failed.clone();
                }
                let p = ProgressPage {
                    snapshot_id: String::new(),
                    snapshot_time: Utc::now(),
                    task: summary_head(&head_facts, &self.run_id),
                    total,
                    finished,
                    succeeded,
                    failed: finished - succeeded,
                    pending: head_facts.head.counts.pending as usize,
                    running: head_facts.head.counts.running as usize,
                    execution_percent: ((rust_decimal::Decimal::from(finished)
                        * rust_decimal::Decimal::from(100))
                        / rust_decimal::Decimal::from(total.max(1)))
                    .round_dp(2)
                    .to_string(),
                    unfinished: vec![],
                    next_cursor: None,
                    page_complete: false,
                    plan,
                    publication_versions: vec![],
                    frozen_daily_ranges: vec![],
                    range_results: vec![],
                    frozen_yearly_inputs: vec![],
                    yearly_results: vec![],
                    publication_check: head_facts
                        .head
                        .result
                        .as_ref()
                        .and_then(|r| r.get("publication_check"))
                        .cloned()
                        .map(serde_json::from_value)
                        .transpose()
                        .map_err(|e| Error::Integrity(format!("publication check result: {e}")))?,
                    stopped_write_check: head_facts
                        .head
                        .result
                        .as_ref()
                        .and_then(|r| r.get("stopped_write_check"))
                        .cloned()
                        .map(serde_json::from_value)
                        .transpose()
                        .map_err(|e| Error::Integrity(format!("stopped check result: {e}")))?,
                };
                self.start(
                    key,
                    access,
                    Content::ProgressKeyset {
                        page: Box::new(p),
                        after: None,
                        failed,
                    },
                )
            } else {
                let facts = {
                    let _span = Span::enter(Stage::TaskAssembly);
                    read.task(&args.task_id, access)?
                };
                let mut unfinished = vec![];
                let mut finished = 0;
                let mut succeeded = 0;
                for i in &facts.snapshot.items {
                    if let Some(o) = &i.outcome {
                        finished += 1;
                        if o.success {
                            succeeded += 1;
                        }
                    }
                    if !i.outcome.as_ref().is_some_and(|o| o.success) {
                        unfinished.push(progress_item(i));
                    }
                }
                unfinished.sort_by(|a, b| a.item_id.cmp(&b.item_id));
                budget(&unfinished)?;
                // v8 PR-5 (#387 §14): one classification shared with the v2
                // projection and with the bounded path.
                let failed = super::task_v2::failed_by_reason(&unfinished);
                let p = ProgressPage {
                    snapshot_id: String::new(),
                    snapshot_time: Utc::now(),
                    task: summary(&facts, &self.run_id),
                    total: facts.snapshot.items.len(),
                    finished,
                    succeeded,
                    failed: finished - succeeded,
                    pending: facts
                        .snapshot
                        .items
                        .iter()
                        .filter(|i| i.stage == crate::task_control::Stage::Pending)
                        .count(),
                    running: facts
                        .snapshot
                        .items
                        .iter()
                        .filter(|i| i.stage == crate::task_control::Stage::Running)
                        .count(),
                    execution_percent: ((rust_decimal::Decimal::from(finished)
                        * rust_decimal::Decimal::from(100))
                        / rust_decimal::Decimal::from(facts.snapshot.items.len()))
                    .round_dp(2)
                    .to_string(),
                    unfinished,
                    next_cursor: None,
                    page_complete: false,
                    frozen_daily_ranges: frozen_daily_ranges(&facts.snapshot)?,
                    publication_versions: facts.publications,
                    range_results: facts.range_results,
                    yearly_results: facts.yearly_results,
                    publication_check: facts
                        .snapshot
                        .task
                        .result
                        .as_ref()
                        .and_then(|r| r.get("publication_check"))
                        .cloned()
                        .map(serde_json::from_value)
                        .transpose()
                        .map_err(|e| Error::Integrity(format!("publication check result: {e}")))?,
                    stopped_write_check: facts
                        .snapshot
                        .task
                        .result
                        .as_ref()
                        .and_then(|r| r.get("stopped_write_check"))
                        .cloned()
                        .map(serde_json::from_value)
                        .transpose()
                        .map_err(|e| Error::Integrity(format!("stopped check result: {e}")))?,
                    frozen_yearly_inputs: frozen_yearly_inputs(&facts.snapshot)?,
                    plan: None,
                };
                let wire_bytes = {
                    let _span = Span::enter(Stage::WireBudget);
                    serde_json::to_vec(&p)
                        .map_err(|e| invalid(&e.to_string()))?
                        .len()
                };
                if wire_bytes > MAX_BYTES {
                    return Err(invalid("progress snapshot byte budget exceeded"));
                }
                self.start(
                    key,
                    access,
                    Content::Progress {
                        page: Box::new(p),
                        failed,
                    },
                )
            }
        };
        if let Content::ProgressKeyset {
            page,
            after,
            failed,
        } = &mut s.content
        {
            let failed = std::collections::BTreeMap::clone(failed);
            // Bounded large-task page: head counters and plan summary were
            // saved once; each page re-queries not-yet-successful items by
            // keyset — no stored item vector.
            read.verify_stopped_check(&page.stopped_write_check)?;
            read.verify_publication_check(&page.publication_check)?;
            let items =
                read.unfinished_items(&args.task_id, after.as_deref(), args.page_size + 1, access)?;
            let more = items.len() > args.page_size;
            let mut result = (**page).clone();
            result.unfinished = items
                .iter()
                .take(args.page_size)
                .map(|i| bound_item(progress_item(i)))
                .collect();
            result.snapshot_id = s.id.clone();
            result.snapshot_time = s.time;
            result.page_complete = !more;
            if more {
                *after = result.unfinished.last().map(|i| i.item_id.clone());
                result.next_cursor = Some(self.save(s));
            }
            let wire_bytes = serde_json::to_vec(&result)
                .map_err(|e| invalid(&e.to_string()))?
                .len();
            if wire_bytes > MAX_BYTES {
                return Err(invalid("progress snapshot byte budget exceeded"));
            }
            return Ok((result, failed));
        }
        let Content::Progress { page: p, failed } = &s.content else {
            return Err(invalid("cursor category"));
        };
        let failed = std::collections::BTreeMap::clone(failed);
        {
            let _span = Span::enter(Stage::VerifyVersions);
            read.verify_versions(&p.publication_versions)?;
        }
        {
            let _span = Span::enter(Stage::VerifyRanges);
            read.verify_range_results(&p.range_results)?;
        }
        {
            let _span = Span::enter(Stage::VerifyYearly);
            read.verify_yearly_results(&p.yearly_results)?;
        }
        read.verify_stopped_check(&p.stopped_write_check)?;
        read.verify_publication_check(&p.publication_check)?;
        let end = (s.offset + args.page_size).min(p.unfinished.len());
        let more = end < p.unfinished.len();
        let mut result = (**p).clone();
        result.unfinished = p.unfinished[s.offset..end].to_vec();
        result.snapshot_id = s.id.clone();
        result.snapshot_time = s.time;
        result.page_complete = !more;
        s.offset = end;
        if more {
            result.next_cursor = Some(self.save(s));
        }
        Ok((result, failed))
    }
    pub fn availability(&self, args: Query, access: &Access) -> Result<AvailabilityPage, Error> {
        Ok(self.availability_page(args, access)?.0)
    }
    /// v2 availability table (v8 PR-3, #387 §4.1/§5): one row per entity with
    /// the related-task objects shared once under `tasks`.
    pub fn availability_v2(
        &self,
        args: Query,
        access: &Access,
    ) -> Result<serde_json::Value, Error> {
        let fields = super::columnar::select(
            args.fields.as_deref(),
            super::columnar::AVAILABILITY_FIELDS,
            super::columnar::AVAILABILITY_FIELDS,
            "get_data_availability",
        )?;
        let (page, plan) = self.availability_page(args, access)?;
        let meta = super::columnar::PageMeta {
            snapshot_id: &page.snapshot_id,
            snapshot_time: page.snapshot_time,
            next_cursor: page.next_cursor.as_deref(),
            page_complete: page.page_complete,
            limitations: &[],
        };
        Ok(super::columnar::availability_table(
            plan.args.dataset,
            &page.items,
            &fields,
            &meta,
        ))
    }
    fn availability_page(
        &self,
        mut args: Query,
        access: &Access,
    ) -> Result<(AvailabilityPage, super::query::QueryPlan), Error> {
        let cursor = args.cursor.take();
        let plan = super::query::QueryPlan::new(args, access)?;
        let key = format!(
            "availability:{}",
            serde_json::to_string(&plan.args).unwrap()
        );
        let mut s = if let Some(c) = cursor {
            self.resume(&c, &key, access)?
        } else {
            let facts = crate::stage_trace::timed("availability.repository", || {
                self.repository.availability(&plan, access)
            })?;
            let items = crate::stage_trace::timed("availability.items", || {
                facts
                    .into_iter()
                    .map(|f| {
                        availability_item(f, plan.args.dataset, &self.run_id, &plan.args.filter)
                    })
                    .collect::<Result<Vec<_>, _>>()
            })?;
            crate::stage_trace::timed("availability.budget", || budget(&items))?;
            self.start(key, access, Content::Availability(items))
        };
        let Content::Availability(items) = &s.content else {
            return Err(invalid("cursor category"));
        };
        crate::stage_trace::timed("availability.verify_versions", || {
            self.repository.verify_versions_committed(
                &items
                    .iter()
                    .filter_map(|i| i.version_ref.clone())
                    .collect::<Vec<_>>(),
            )
        })?;
        let end = (s.offset + plan.args.page_size).min(items.len());
        let more = end < items.len();
        let result = items[s.offset..end].to_vec();
        s.offset = end;
        let mut p = AvailabilityPage {
            snapshot_id: s.id.clone(),
            snapshot_time: s.time,
            items: result,
            next_cursor: None,
            page_complete: !more,
        };
        if more {
            p.next_cursor = Some(self.save(s));
        }
        Ok((p, plan))
    }
}
/// The plan summary a bounded progress page carries instead of per-item
/// arrays; `None` when the task has no shared evidence or no plan id.
fn plan_progress(evidence: &Option<serde_json::Value>) -> Option<PlanProgress> {
    let e = evidence.as_ref()?;
    Some(PlanProgress {
        plan_id: e["plan_id"].as_str()?.to_owned(),
        generation_id: e["generation_id"].as_str().map(str::to_owned),
        target_day: serde_json::from_value(e["target_day"].clone()).ok(),
        failed_by_reason: Default::default(),
    })
}
/// Whether a task is served by the bounded (keyset) progress shape.
///
/// The threshold is about the plan the caller holds, not the task's live row
/// count: a plan-run task also carries the server-internal derived items of
/// issue-383 §9.2 (10 rows), and a 100-entity plan must keep the shape its
/// acceptance (`item_count`) announced. Tasks without plan evidence keep the
/// plain row-count rule.
fn bounded_progress(total_items: u64, plan_items: Option<u64>) -> bool {
    match plan_items {
        Some(eligible) => eligible > LARGE_TASK_ITEMS,
        None => total_items > LARGE_TASK_ITEMS,
    }
}

/// The `unfinished` entry one task item produces — shared by the
/// materialized and the keyset paths.
fn progress_item(i: &crate::task_control::TaskItem) -> ProgressItem {
    ProgressItem {
        item_id: i.spec.item_id.clone(),
        entity: i.spec.target["symbol"].as_str().and_then(entity),
        period: i.spec.target["period"].as_str().map(str::to_owned),
        progress_unit: i.spec.progress_unit.clone(),
        stage: format!("{:?}", i.stage).to_lowercase(),
        success: i.outcome.as_ref().map(|o| o.success),
        error: i.outcome.as_ref().and_then(|o| o.error.clone()),
        reason_code: None,
    }
}
/// Longest `error` a bounded large-task page carries (see
/// `task_v2::BOUNDED_ERROR_CHARS`; the mapping and the bound live in the v2
/// projection module so both paths share one policy).
pub use super::task_v2::{BOUNDED_ERROR_CHARS, reason_code};
/// Large-task view of one unfinished item: failed items get their
/// `reason_code` and a truncated `error`.
fn bound_item(mut item: ProgressItem) -> ProgressItem {
    if item.success == Some(false) {
        let error = item.error.take().unwrap_or_default();
        item.reason_code = Some(reason_code(&error).to_owned());
        item.error = Some(super::task_v2::bounded_error(&error));
    }
    item
}
fn budget<T: serde::Serialize>(v: &[T]) -> Result<(), Error> {
    if v.len() > 10000 || serde_json::to_vec(v).unwrap().len() > MAX_BYTES {
        Err(invalid("observation snapshot budget exceeded"))
    } else {
        Ok(())
    }
}

fn frozen_daily_ranges(task: &TaskSnapshot) -> Result<Vec<FrozenDailyRange>, Error> {
    if task.task.key.dataset != "market.daily" {
        return Ok(vec![]);
    }
    if !matches!(
        task.task.key.operation.as_str(),
        "update_data" | "backfill_data" | "republish_data"
    ) {
        return Ok(vec![]);
    }
    let explicit = matches!(
        task.task.key.contract_version.as_str(),
        crate::market_update::tasks::CONTRACT_VERSION
            | crate::market_update::tasks::REPUBLISH_CONTRACT_VERSION
    );
    let default = matches!(
        task.task.key.contract_version.as_str(),
        crate::market_update::tasks::DEFAULT_CONTRACT_VERSION
            | crate::data_supply::plan::RUN_CONTRACT_VERSION
    );
    if !explicit && !default {
        return Err(Error::Integrity("unknown frozen range contract".into()));
    }
    let mut out = vec![];
    for item in &task.items {
        // Server-internal derived items carry a dataset target, not a security
        // range (issue-383 §9.2); they are not projected as frozen ranges.
        if crate::task_control::derived::is_derived(&item.spec.item_id) {
            continue;
        }
        let frozen = &item.spec.frozen_inputs;
        let entity = item.spec.target["symbol"]
            .as_str()
            .and_then(entity)
            .ok_or_else(|| Error::Integrity("frozen range entity missing".into()))?;
        let mut ranges: Vec<serde_json::Value> = vec![];
        if explicit {
            let range = &frozen["range"];
            ranges.push(serde_json::json!({
                "start_date":range["start"],"end_date":range["end"]
            }));
        } else {
            if let Some(refresh) = frozen.get("refresh_range") {
                ranges.push(refresh.clone());
            }
            for gap in frozen["proven_gap_ranges"]
                .as_array()
                .ok_or_else(|| Error::Integrity("frozen proven gap ranges missing".into()))?
            {
                ranges.push(gap["range"].clone());
            }
        }
        for range in ranges {
            out.push(serde_json::from_value(serde_json::json!({
                "item_id":item.spec.item_id,"entity":entity,
                "base_version_id":frozen["base_version_id"],"base_integrity_sha256":frozen["base_integrity_sha256"],
                "start_date":range["start_date"],"end_date":range["end_date"]
            })).map_err(|e|Error::Integrity(format!("frozen range: {e}")))?);
        }
    }
    Ok(out)
}

fn frozen_yearly_inputs(task: &TaskSnapshot) -> Result<Vec<FrozenYearlyInput>, Error> {
    if task.task.key.dataset != "market.yearly"
        || !matches!(
            task.task.key.operation.as_str(),
            "update_data" | "backfill_data"
        )
    {
        return Ok(vec![]);
    }
    if !matches!(
        task.task.key.contract_version.as_str(),
        crate::yearly_publication::tasks::CONTRACT_VERSION
            | crate::data_supply::plan::RUN_CONTRACT_VERSION
    ) {
        return Err(Error::Integrity("unknown frozen yearly contract".into()));
    }
    task.items.iter().map(|item|{
        let f=&item.spec.frozen_inputs;
        let entity=item.spec.target["symbol"].as_str().and_then(entity).ok_or_else(||Error::Integrity("frozen yearly entity missing".into()))?;
        let intent:crate::market_storage::PublishIntent=serde_json::from_value(f["intent"].clone()).map_err(|e|Error::Integrity(e.to_string()))?;
        let publication=match intent {
            crate::market_storage::PublishIntent::HistoricalFixedBase=>YearlyPublication::HistoricalFixedBase{comparison_version_id:f["comparison"]["version_id"].as_str().map(str::to_owned)},
            crate::market_storage::PublishIntent::CurrentHead{expected_yearly_head,..}=>YearlyPublication::CurrentHead{expected_yearly_head},
            _=>return Err(Error::Integrity("daily intent in frozen yearly task".into())),
        };
        serde_json::from_value(serde_json::json!({"item_id":item.spec.item_id,"entity":entity,"base_daily_version_id":f["base_daily_version_id"],"base_integrity_sha256":f["base_integrity_sha256"],"years":f["years"],"publication":publication,"comparison":f["comparison"]})).map_err(|e|Error::Integrity(e.to_string()))
    }).collect()
}

#[cfg(test)]
mod reason_tests {
    use super::*;

    fn failed(error: &str) -> ProgressItem {
        ProgressItem {
            item_id: "[\"market\",\"sh.600039\",\"day\"]".into(),
            entity: None,
            period: Some("day".into()),
            progress_unit: "item".into(),
            stage: "finished".into(),
            success: Some(false),
            error: Some(error.into()),
            reason_code: None,
        }
    }

    // issue-383 §9.2: the internal derived items are not plan items, so the
    // bounded-read shape must follow the plan the caller holds. A 100-entity
    // plan keeps the small shape even though its task row carries 110 items.
    #[test]
    fn bounded_progress_follows_the_plan_not_the_internal_items() {
        assert!(!bounded_progress(110, Some(100)));
        assert!(bounded_progress(111, Some(101)));
        assert!(!bounded_progress(100, None));
        assert!(bounded_progress(101, None));
        // A plan-run task always carries its evidence; the row-count rule is
        // the non-plan fallback.
        assert!(!bounded_progress(50, Some(50)));
    }

    /// The error classes the v7 contract promises (generic-data-v7.md §5),
    /// built from the texts the 10-13 market-wide run actually produced.
    #[test]
    fn reason_code_classes_are_stable() {
        let revised = "INVALID_MARKET_COMMAND: NEEDS_VERIFICATION: RevisionUnverified([2003-03-25, 2003-03-26])";
        let frozen = "PUBLISH_PRECONDITION: range publication differs from accepted frozen inputs";
        let halted = "no bars inside the accepted range";
        assert_eq!(reason_code(revised), "REVISION_UNVERIFIED");
        assert_eq!(reason_code(frozen), "PUBLISH_PRECONDITION");
        assert_eq!(reason_code(halted), "NO_BARS_IN_RANGE");
        assert_eq!(reason_code("something else entirely"), "OTHER");
        assert_eq!(reason_code(""), "OTHER");
    }

    #[test]
    fn bounded_item_carries_class_and_a_short_error() {
        let long = format!(
            "INVALID_MARKET_COMMAND: NEEDS_VERIFICATION: RevisionUnverified([{}])",
            "2003-03-25, ".repeat(8_000)
        );
        let item = bound_item(failed(&long));
        assert_eq!(item.reason_code.as_deref(), Some("REVISION_UNVERIFIED"));
        let error = item.error.unwrap();
        assert!(error.chars().count() < BOUNDED_ERROR_CHARS + 40, "{error}");
        assert!(error.starts_with("INVALID_MARKET_COMMAND"));
        assert!(error.contains("chars total"));
        // a short error is kept verbatim
        let kept = bound_item(failed("PUBLISH_PRECONDITION: x"));
        assert_eq!(kept.error.as_deref(), Some("PUBLISH_PRECONDITION: x"));
        assert_eq!(kept.reason_code.as_deref(), Some("PUBLISH_PRECONDITION"));
    }

    #[test]
    fn bounded_item_leaves_pending_and_succeeded_items_alone() {
        let mut pending = failed("");
        pending.success = None;
        pending.error = None;
        let same = bound_item(pending.clone());
        assert_eq!(same, pending);
        let mut done = failed("");
        done.success = Some(true);
        done.error = None;
        assert_eq!(bound_item(done.clone()), done);
    }

    /// The `wait_seconds` clamp: 0/absent means no wait; anything above the
    /// 50-second cap is clamped rather than rejected.
    #[test]
    fn wait_budget_clamps_to_the_server_cap() {
        assert_eq!(wait_budget(0), None);
        assert_eq!(wait_budget(1), Some(Duration::from_secs(1)));
        assert_eq!(wait_budget(45), Some(Duration::from_secs(45)));
        assert_eq!(wait_budget(50), Some(Duration::from_secs(50)));
        assert_eq!(wait_budget(51), Some(Duration::from_secs(50)));
        assert_eq!(wait_budget(u32::MAX), Some(Duration::from_secs(50)));
    }
}
