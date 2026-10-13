//! Consistent task observations and bounded, authorization-bound snapshots.
use super::{
    Access,
    query::{MAX_BYTES, invalid, page_size},
    wire::*,
};
use crate::progress_trace::{Span, Stage, Trace};
use crate::{market_storage::Error, task_control::TaskSnapshot};
use chrono::{DateTime, Datelike, Utc};
use std::{cell::RefCell, collections::VecDeque};
#[derive(Clone)]
pub struct TaskFacts {
    pub request_id: String,
    pub snapshot: TaskSnapshot,
    pub publications: Vec<VersionRef>,
    pub range_results: Vec<RangeExecutionResult>,
    pub yearly_results: Vec<YearlyExecutionResult>,
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
    pub related_tasks: Vec<std::rc::Rc<TaskFacts>>,
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
    Ok(AvailabilityItem{entity:f.entity,version_ref:f.manifest.and_then(|m|m.version_ref),integrity:if unavailable{"failed"}else if availability==Availability::Absent{"not_applicable"}else{"verified"}.into(),confirmed_gaps,unresolved_ranges:if availability==Availability::Unknown{vec![range.clone()]}else{vec![]},availability,coverage_evidence,first_record:f.first_record,last_record:f.last_record,record_count:f.record_count,freshness:"unknown".into(),related_tasks_total:f.related_tasks_total,related_tasks_truncated:f.related_tasks_total>f.related_tasks.len(),related_tasks:f.related_tasks.iter().map(|f|summary(f,run)).collect(),stale,as_of_day,reason,limitations:vec!["range completeness is not inferred from dates or version coverage alone; no implicit collection or repair".into()]})
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
    pub facts: Vec<TaskFacts>,
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
        let mut facts: Vec<TaskFacts> = all
            .into_iter()
            .filter(|f| {
                scan.upper.is_none_or(|u| f.snapshot.task.created_at <= u)
                    && scan.after.as_ref().is_none_or(|(at, id)| {
                        let (c, t) = key(f);
                        c.as_str() < at.as_str() || (c == *at && t.as_str() > id.as_str())
                    })
            })
            .collect();
        let more = facts.len() > scan.batch;
        facts.truncate(scan.batch);
        let next_after = more.then(|| facts.last().map(key)).flatten();
        Ok(TaskScanBatch { facts, next_after })
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
    }
}
#[derive(Clone)]
enum Content {
    /// Keyset position of a `list_tasks` snapshot (XP-025 D): the list
    /// itself is never materialized.
    TaskCursor(Option<(String, String)>),
    Progress(Box<ProgressPage>),
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
            return Err(invalid("cursor conditions/authorization changed"));
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
    pub fn tasks(&self, mut args: TasksQuery, access: &Access) -> Result<TaskListPage, Error> {
        page_size(args.page_size)?;
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
                let sm = summary(f, &self.run_id);
                if args
                    .dataset
                    .is_none_or(|d| serde_json::to_value(d).unwrap() == sm.dataset)
                    && args
                        .operation
                        .is_none_or(|op| serde_json::to_value(op).unwrap() == sm.operation)
                    && args.status.as_ref().is_none_or(|v| v.contains(&sm.status))
                    && args.created_from.is_none_or(|t| sm.created_at >= t)
                    && args.created_to.is_none_or(|t| sm.created_at <= t)
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
    pub fn progress(
        &self,
        mut args: ProgressQuery,
        access: &Access,
    ) -> Result<ProgressPage, Error> {
        let trace = Trace::begin();
        page_size(args.page_size)?;
        let cursor = args.cursor.take();
        let key = format!("progress:{}", serde_json::to_string(&args).unwrap());
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
    ) -> Result<ProgressPage, Error> {
        let mut s = if let Some(c) = cursor {
            self.resume(&c, &key, access)?
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
                    unfinished.push(ProgressItem {
                        item_id: i.spec.item_id.clone(),
                        entity: i.spec.target["symbol"].as_str().and_then(entity),
                        period: i.spec.target["period"].as_str().map(str::to_owned),
                        progress_unit: i.spec.progress_unit.clone(),
                        stage: format!("{:?}", i.stage).to_lowercase(),
                        success: i.outcome.as_ref().map(|o| o.success),
                        error: i.outcome.as_ref().and_then(|o| o.error.clone()),
                    });
                }
            }
            unfinished.sort_by(|a, b| a.item_id.cmp(&b.item_id));
            budget(&unfinished)?;
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
            self.start(key, access, Content::Progress(Box::new(p)))
        };
        let Content::Progress(p) = &s.content else {
            return Err(invalid("cursor category"));
        };
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
        Ok(result)
    }
    pub fn availability(
        &self,
        mut args: Query,
        access: &Access,
    ) -> Result<AvailabilityPage, Error> {
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
        Ok(p)
    }
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
    let default =
        task.task.key.contract_version == crate::market_update::tasks::DEFAULT_CONTRACT_VERSION;
    if !explicit && !default {
        return Err(Error::Integrity("unknown frozen range contract".into()));
    }
    let mut out = vec![];
    for item in &task.items {
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
    if task.task.key.contract_version != crate::yearly_publication::tasks::CONTRACT_VERSION {
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
