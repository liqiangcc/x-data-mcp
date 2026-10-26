//! Stage B application contracts. No SQLite, MCP, download or production wiring.
pub mod compat;
pub mod query;
use crate::{
    market_data::{Adjustment, Bar, Period},
    publish::EffectiveVersion,
    task_control::{
        self, Change, ItemBinding, ItemOutcome, Stage, Status, TaskHead, TaskItem, TaskSnapshot,
    },
};
use std::collections::BTreeSet;

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, Utc};
use compat::CheckedRecord;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("MARKET_STORAGE: {0}")]
    Storage(String),
    #[error("MARKET_INTEGRITY: {0}")]
    Integrity(String),
    #[error("BASE_UNAVAILABLE: {0}")]
    Base(String),
    #[error("PUBLISH_PRECONDITION: {0}")]
    Conflict(String),
    #[error("MARKET_NOT_FOUND")]
    NotFound,
    /// SPEC-LATEST-ONLY §6.1 (schema 21): the pinned version id exists in the
    /// event log but is no longer the current series state — distinct from
    /// `NotFound` (never committed). Wire-mapped to `stale_version` in
    /// generic-data-v6.
    #[error("MARKET_STALE: {0}")]
    Stale(String),
    #[error("INVALID_MARKET_COMMAND: {0}")]
    Invalid(String),
    #[error("PUBLISH_OUTCOME_UNKNOWN: {0}; inspect the same publication_id")]
    OutcomeUnknown(String),
    #[error("GAP_BASIS_NOT_VERIFIED: {0}")]
    BasisNotVerified(String),
    #[error(transparent)]
    Task(#[from] task_control::Error),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    pub symbol: String,
    pub period: Period,
}
impl Scope {
    pub fn of(v: &EffectiveVersion) -> Self {
        Self {
            symbol: v.symbol.clone(),
            period: v.period,
        }
    }
    pub fn occupancy_key(&self) -> String {
        task_control::legacy::scope_id(
            &self.symbol,
            match self.period {
                Period::Day => "day",
                Period::Year => "year",
            },
        )
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PublishIntent {
    Daily {
        expected_head: Option<String>,
    },
    HistoricalFixedBase,
    CurrentHead {
        expected_daily_head: String,
        expected_yearly_head: Option<String>,
    },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Publication {
    pub publication_id: String,
    pub version_id: String,
    pub task_id: String,
    pub item_id: String,
    pub run_id: String,
    pub expected_revision: u64,
    pub intent: PublishIntent,
}
/// Sealed application decision; infrastructure cannot independently choose promotion.
#[derive(Debug, Clone)]
pub struct PublishPlan {
    pub(crate) command: Publication,
    pub(crate) record: CheckedRecord,
    pub(crate) required_heads: Vec<(Scope, Option<String>)>,
    pub(crate) promote: Option<Scope>,
    pub(crate) base_digest: Option<String>,
    /// The 不复权 side of a daily item commit (issue-383 §9.1). `None` on
    /// yearly/legacy plans and on callers that do not carry the raw batch.
    pub(crate) raw: Option<crate::market_data::RawBatch>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub publication_id: String,
    pub version_id: String,
    pub task_id: String,
    pub task_revision: u64,
}
#[derive(Debug, Clone)]
pub struct ReadPage {
    pub integrity_sha256: String,
    pub version: EffectiveVersion,
    pub rows: Vec<Bar>,
    pub next_after: Option<NaiveDate>,
}
#[derive(Debug, Clone)]
pub struct Range {
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub after: Option<NaiveDate>,
    pub limit: usize,
}
impl Range {
    pub fn validate(&self) -> Result<(), Error> {
        if self.start.year() <= 0
            || self.end.year() > 9999
            || self.start > self.end
            || self.limit == 0
            || self.limit > 1000
        {
            return Err(Error::Invalid("date range/page size (1..1000)".into()));
        }
        Ok(())
    }
}
/// O(1) published-version identity/integrity facts — the fields accept and
/// execution guards actually read, without bar materialization.
#[derive(Debug, Clone)]
pub struct FixedFacts {
    pub version_id: String,
    pub symbol: String,
    pub period: Period,
    /// Stored authoritative integrity digest of the complete version.
    pub integrity_sha256: String,
    /// Stored day bounds — a `[first_day, first_day]` window read returns
    /// the full version metadata while decoding only one segment.
    pub first_day: NaiveDate,
    pub last_day: NaiveDate,
}
/// One explicit-yearly accept read request.
pub struct YearlyAcceptWant {
    pub base_version_id: String,
    pub first: NaiveDate,
    pub last: NaiveDate,
    pub comparison_version_id: Option<String>,
}
/// Result of one `YearlyAcceptWant`.
pub struct YearlyAcceptRead {
    pub base: FixedFacts,
    pub years: BTreeSet<i32>,
    pub comparison: Option<FixedFacts>,
}
pub trait MarketRepository {
    fn stage(&self, raw: &Value) -> Result<(), Error>;
    fn candidate(&self, id: &str) -> Result<CheckedRecord, Error>;
    /// Binds the staged candidate to the combined record the update
    /// evidence describes (#288): the legacy backend compares the stored
    /// envelope `integrity_sha256`; the compact backend keeps no envelope,
    /// so it re-parses `whole` and compares the typed facts `stage`
    /// derived — records, coverage, units, adjustment evidence and
    /// snapshot bindings — a strictly stronger binding for the same check.
    fn staged_matches(&self, version_id: &str, whole: &Value) -> Result<bool, Error>;
    fn mark_validated(&self, record: &CheckedRecord) -> Result<(), Error>;
    fn fixed(&self, id: &str) -> Result<CheckedRecord, Error>;
    /// `fixed` for the PRIOR yearly head read by yearly prepare (XP-032 P1,
    /// user-approved verification layering difference): that head was fully
    /// verified when it was published, so backends may verify it at the
    /// committed tier — row integrity digest plus its own segment/clip
    /// checks — without re-aggregating its lineage from its older daily
    /// base. Never use it for anything but that comparison; the default is
    /// the full `fixed`.
    fn fixed_comparison(&self, id: &str) -> Result<CheckedRecord, Error> {
        self.fixed(id)
    }
    fn fixed_many(&self, ids: &[String]) -> Result<Vec<CheckedRecord>, Error>;
    /// O(1) header read for digest/identity checks on a published version.
    fn fixed_facts(&self, id: &str) -> Result<FixedFacts, Error>;
    /// Published read whose `version().records`/`batch.bars` contain at least
    /// every stored bar in `[first_day, last_day]`; the compact backend
    /// restricts to the window (O(window), verified per-clip digests), the
    /// legacy backend may return the full version. `raw()` is a projection
    /// for inspection only: `raw()["integrity_sha256"]` is the stored digest
    /// of the complete version, not a recompute over the window — never
    /// stage or re-digest a windowed record.
    fn fixed_window(
        &self,
        id: &str,
        first_day: NaiveDate,
        last_day: NaiveDate,
    ) -> Result<CheckedRecord, Error>;
    /// Distinct calendar years that have at least one stored bar of the
    /// fixed version inside `[first_day, last_day]`; the compact backend
    /// decodes only overlapping segments without building `Bar`s, the
    /// legacy backend may derive them from a full read. Same published-only
    /// `NotFound` semantics as `fixed_window`.
    fn fixed_window_years(
        &self,
        id: &str,
        first_day: NaiveDate,
        last_day: NaiveDate,
    ) -> Result<BTreeSet<i32>, Error>;
    /// Accept-time reads for a whole explicit-yearly batch (XP-025 B): base
    /// facts, the years present in the selected window, and the optional
    /// comparison facts for every want. Backends able to serve them from
    /// one snapshot override this; the default is the per-want reads the
    /// resolver always did. A top-level `Err` is a storage/transaction
    /// failure; per-want failures stay per want.
    fn yearly_accept_reads(
        &self,
        wants: &[YearlyAcceptWant],
    ) -> Result<Vec<Result<YearlyAcceptRead, Error>>, Error> {
        Ok(wants
            .iter()
            .map(|want| {
                Ok(YearlyAcceptRead {
                    base: self.fixed_facts(&want.base_version_id)?,
                    years: self.fixed_window_years(&want.base_version_id, want.first, want.last)?,
                    comparison: want
                        .comparison_version_id
                        .as_ref()
                        .map(|id| self.fixed_facts(id))
                        .transpose()?,
                })
            })
            .collect())
    }
    fn task(&self, id: &str) -> Result<TaskSnapshot, Error>;
    /// O(1) task-row projection for hot-loop guards; full snapshots are for
    /// explicit task/progress reads.
    fn task_head(&self, id: &str) -> Result<TaskHead, Error>;
    /// O(1) head + item + occupancy binding for per-item execution and
    /// commit binding checks. `None` means task or item row absent.
    fn task_item_binding(&self, task: &str, item: &str) -> Result<Option<ItemBinding>, Error>;
    /// Single-item light transition: applies `change` on the caller-held
    /// head+item, verified by task-row revision (refreshed in place when
    /// stale). Writes only the touched rows.
    fn transition_task_head(
        &self,
        head: &mut TaskHead,
        item: Option<&mut TaskItem>,
        run: &str,
        change: crate::task_control::Change,
    ) -> Result<(), Error>;
    fn commit(&self, plan: &PublishPlan) -> Result<Receipt, Error>;
    fn receipt(&self, id: &str) -> Result<Option<Receipt>, Error>;
    fn range(&self, scope: &Scope, version: Option<&str>, range: &Range)
    -> Result<ReadPage, Error>;
    fn versions(&self, scope: &Scope) -> Result<Vec<EffectiveVersion>, Error>;
}
/// Durable proven-gap fact. Only durable evidence paths may produce one;
/// this record carries opaque evidence identity and is never invented from
/// date discontinuity, missing rows, version absence, or the current date.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenGap {
    pub gap_id: String,
    pub symbol: String,
    pub period: Period,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub evidence_kind: String,
    pub evidence_id: String,
    pub evidence_digest: String,
    pub observed_at: DateTime<Utc>,
}
impl ProvenGap {
    /// Mechanical storage invariants only; evidence-object existence and
    /// digest verification belong to the consuming application layer.
    pub fn validate(&self) -> Result<(), Error> {
        if self.gap_id.trim().is_empty()
            || self.symbol.trim().is_empty()
            || self.evidence_kind.trim().is_empty()
            || self.evidence_id.trim().is_empty()
            || !crate::hash::is_sha256(&self.evidence_digest)
            || self.start_date > self.end_date
        {
            return Err(Error::Invalid("invalid proven gap fact".into()));
        }
        Ok(())
    }
}
/// Durable gap-verification decision (#155): the only evidence object that
/// may produce a `proven_gaps` row. Produced exclusively by an installed
/// trusted source-gap verifier port — never by task outcomes, empty
/// source responses, date discontinuity, or MCP caller fields. `period` is
/// day-only in the first phase; `subject_*` binds the verified published
/// QFQ daily version the gap is relative to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GapVerificationDecision {
    pub decision_id: String,
    pub symbol: String,
    pub period: Period,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub subject_version_id: String,
    pub subject_version_digest: String,
    /// Only `confirmed_missing_range` produces a gap row; other conclusions
    /// (e.g. source limitation) are stored as decisions alone.
    pub conclusion: String,
    pub basis_kind: String,
    pub basis_ref: String,
    pub basis_sha256: String,
    /// Server-derived installed-verifier identity; never caller-supplied.
    pub verifier_id: String,
    pub decided_at: DateTime<Utc>,
    pub decision_sha256: String,
}
impl GapVerificationDecision {
    /// Mechanical storage invariants only; subject-version binding, basis
    /// re-verification, verifier authorization, and digest recomputation
    /// belong to the producer application layer.
    pub fn validate(&self) -> Result<(), Error> {
        if self.decision_id.trim().is_empty()
            || self.symbol.trim().is_empty()
            || self.period != Period::Day
            || self.subject_version_id.trim().is_empty()
            || !crate::hash::is_sha256(&self.subject_version_digest)
            || self.conclusion.trim().is_empty()
            || self.basis_kind.trim().is_empty()
            || self.basis_ref.trim().is_empty()
            || !crate::hash::is_sha256(&self.basis_sha256)
            || self.verifier_id.trim().is_empty()
            || !crate::hash::is_sha256(&self.decision_sha256)
            || self.start_date > self.end_date
        {
            return Err(Error::Invalid("invalid gap verification decision".into()));
        }
        Ok(())
    }
    /// Canonical digest over every bound field except `decision_sha256`
    /// (`gap-decision-v1` domain-separated). The producer sets
    /// `decision_sha256` to this value; audits replay it.
    pub fn content_digest(&self) -> String {
        crate::hash::sha256(
            serde_json::json!([
                "gap-decision-v1",
                self.decision_id,
                self.symbol,
                match self.period {
                    Period::Day => "day",
                    Period::Year => "year",
                },
                self.start_date.to_string(),
                self.end_date.to_string(),
                self.subject_version_id,
                self.subject_version_digest,
                self.conclusion,
                self.basis_kind,
                self.basis_ref,
                self.basis_sha256,
                self.verifier_id,
                self.decided_at.to_rfc3339(),
            ])
            .to_string()
            .as_bytes(),
        )
    }
}
/// The only `proven_gaps.evidence_kind` a decision may produce.
pub const GAP_DECISION_EVIDENCE_KIND: &str = "gap_verification_decision";
/// The only conclusion that produces a `proven_gaps` row.
pub const CONFIRMED_MISSING_RANGE: &str = "confirmed_missing_range";
/// Deterministic gap identity: domain-separated canonical digest of the
/// decision's subject + range content (`gap-v1`). Re-deciding the same
/// range on the same subject version maps to the same gap row; never
/// random.
pub fn derive_gap_id(decision: &GapVerificationDecision) -> String {
    crate::hash::sha256(
        serde_json::json!([
            "gap-v1",
            decision.symbol,
            match decision.period {
                Period::Day => "day",
                Period::Year => "year",
            },
            decision.start_date.to_string(),
            decision.end_date.to_string(),
            decision.subject_version_id,
            decision.subject_version_digest,
        ])
        .to_string()
        .as_bytes(),
    )
}
/// Append-only binding: a later published version covers a proven gap.
/// The historical gap row is retained; covering bindings never erase it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GapCovering {
    pub gap_id: String,
    pub version_id: String,
    pub covered_at: DateTime<Utc>,
}
impl GapCovering {
    pub fn validate(&self) -> Result<(), Error> {
        if self.gap_id.trim().is_empty() || self.version_id.trim().is_empty() {
            return Err(Error::Invalid("invalid gap covering".into()));
        }
        Ok(())
    }
}
pub struct Publisher<R> {
    pub repository: R,
}
impl<R: MarketRepository> Publisher<R> {
    pub fn prepare(&self, command: Publication) -> Result<PublishPlan, Error> {
        let record = self.repository.candidate(&command.version_id)?;
        let plan = self.plan_record(command, record)?;
        self.repository.mark_validated(&plan.record)?;
        Ok(plan)
    }
    /// Shared application eligibility for a staged candidate or a verified reuse.
    pub(crate) fn plan_record(
        &self,
        command: Publication,
        record: CheckedRecord,
    ) -> Result<PublishPlan, Error> {
        self.plan_record_prefetched(command, record, None, None)
    }
    /// `plan_record` with caller-prefetched task binding and base facts/meta —
    /// identical check order and semantics, minus redundant storage reads.
    /// The yearly path already holds all three.
    pub(crate) fn plan_record_prefetched(
        &self,
        command: Publication,
        record: CheckedRecord,
        binding: Option<&crate::task_control::ItemBinding>,
        base_meta: Option<(&FixedFacts, &CheckedRecord)>,
    ) -> Result<PublishPlan, Error> {
        if command.publication_id.trim().is_empty() {
            return Err(Error::Invalid("empty publication_id".into()));
        }
        let v = &record.version;
        let scope = Scope::of(v);
        let (required_heads, promote) = match (&command.intent, v.period) {
            (PublishIntent::Daily { expected_head }, Period::Day) => {
                (vec![(scope.clone(), expected_head.clone())], Some(scope))
            }
            (PublishIntent::HistoricalFixedBase, Period::Year) => (vec![], None),
            (
                PublishIntent::CurrentHead {
                    expected_daily_head,
                    expected_yearly_head,
                },
                Period::Year,
            ) => {
                if v.yearly_derivation
                    .as_ref()
                    .map(|l| l.base_daily_version_id.as_str())
                    != Some(expected_daily_head)
                {
                    return Err(Error::Conflict(
                        "candidate must bind expected daily head".into(),
                    ));
                }
                (
                    vec![
                        (
                            Scope {
                                symbol: v.symbol.clone(),
                                period: Period::Day,
                            },
                            Some(expected_daily_head.clone()),
                        ),
                        (scope.clone(), expected_yearly_head.clone()),
                    ],
                    Some(scope),
                )
            }
            _ => return Err(Error::Invalid("intent/period mismatch".into())),
        };
        let mut plan = PublishPlan {
            command,
            record,
            required_heads,
            promote,
            base_digest: None,
            raw: None,
        };
        if let Some(lineage) = &plan.record.version.yearly_derivation {
            // Facts + memoized/windowed validation: the runner's derivation
            // already proved this exact series, so the memo usually makes
            // this a row read plus a 2-bar comparison — no full base
            // materialization, no second aggregation.
            let owned_facts;
            let facts = match base_meta {
                Some((facts, _)) => facts,
                None => {
                    owned_facts = self
                        .repository
                        .fixed_facts(&lineage.base_daily_version_id)
                        .map_err(base_error)?;
                    &owned_facts
                }
            };
            crate::stage_trace::timed(
                "yearly.plan_record.validate_base_facts",
                || match base_meta {
                    Some((_, meta)) => {
                        validate_base_facts_meta(&plan.record, &self.repository, facts, meta)
                    }
                    None => validate_base_facts(&plan.record, &self.repository, facts),
                },
            )?;
            plan.base_digest = Some(facts.integrity_sha256.clone());
        }
        let owned_binding;
        let binding = match binding {
            Some(binding) => binding,
            None => {
                owned_binding = self
                    .repository
                    .task_item_binding(&plan.command.task_id, &plan.command.item_id)?
                    .ok_or(Error::Conflict("missing task item".into()))?;
                &owned_binding
            }
        };
        plan.check_task_binding(binding)?;
        Ok(plan)
    }
}
impl PublishPlan {
    pub fn publication(&self) -> &Publication {
        &self.command
    }
    pub fn head_preconditions(&self) -> &[(Scope, Option<String>)] {
        &self.required_heads
    }
    pub fn head_promotion(&self) -> Option<&Scope> {
        self.promote.as_ref()
    }
    /// Binding guards shared by prepare-time pre-checks and the atomic
    /// commit re-evaluation. Revision/owner/state plus the item's
    /// target/occupancy binding.
    pub(crate) fn check_task_binding(&self, binding: &ItemBinding) -> Result<(), Error> {
        if binding.head.state_revision != self.command.expected_revision
            || binding.head.visible_status(&self.command.run_id) != Status::Running
            || binding.head.owner_run_id.as_deref() != Some(&self.command.run_id)
        {
            return Err(Error::Conflict("task revision/owner/state".into()));
        }
        let scope = Scope::of(&self.record.version);
        let item = &binding.item;
        if item.stage != Stage::Running
            || item.spec.item_id != self.command.item_id
            || item.spec.target
                != serde_json::json!({"symbol":scope.symbol,"period":match scope.period{Period::Day=>"day",Period::Year=>"year"}})
            || !item.spec.scope_keys.contains(&scope.occupancy_key())
            || !binding.scopes_bound
        {
            return Err(Error::Conflict("task target/occupancy".into()));
        }
        Ok(())
    }
    /// Same pure decision is re-evaluated against facts acquired inside commit.
    pub(crate) fn finish_task(&self, binding: &mut ItemBinding) -> Result<(), Error> {
        self.check_task_binding(binding)?;
        crate::task_control::decide_head(
            &mut binding.head,
            Some(&mut binding.item),
            &self.command.run_id,
            Change::FinishItem(
                self.command.item_id.clone(),
                ItemOutcome {
                    success: true,
                    error: None,
                    evidence: None,
                },
            ),
            Utc::now(),
        )?;
        if binding.head.counts.finished == binding.head.counts.total {
            crate::task_control::decide_head(
                &mut binding.head,
                None,
                &self.command.run_id,
                Change::FinishExecution,
                Utc::now(),
            )?;
        }
        Ok(())
    }
}
pub(crate) fn base_error(e: Error) -> Error {
    Error::Base(e.to_string())
}
pub(crate) fn validate_base_identity(
    record: &CheckedRecord,
    base: Option<&CheckedRecord>,
) -> Result<(), Error> {
    let v = &record.version;
    if v.period == Period::Day {
        return Ok(());
    }
    let lineage = v
        .yearly_derivation
        .as_ref()
        .ok_or_else(|| Error::Base("yearly lineage missing".into()))?;
    let daily = &base
        .ok_or_else(|| Error::Base("fixed base missing".into()))?
        .version;
    if daily.version_id != lineage.base_daily_version_id
        || daily.symbol != v.symbol
        || daily.period != Period::Day
        || daily.adjustment != v.adjustment
        || daily.source != v.source
        || daily.unit_currency != v.unit_currency
        || !Adjustment::Forward.matches_evidence(&daily.adjustment_evidence)
    {
        return Err(Error::Base(
            "fixed base identity/period/adjustment/source mismatch".into(),
        ));
    }
    Ok(())
}
/// Facts-based equivalent of `validate_base`: the base's identity fields
/// come from row facts plus a one-day window read (metadata only), and the
/// yearly re-aggregation is memoized on the base's stored integrity digest
/// — a hit proves the identical bytes the runner already verified. A miss
/// falls back to a lineage-bounded window read.
pub(crate) fn validate_base_facts<R: MarketRepository + ?Sized>(
    record: &CheckedRecord,
    repository: &R,
    facts: &FixedFacts,
) -> Result<(), Error> {
    let v = &record.version;
    if v.period == Period::Day {
        return Ok(());
    }
    // Same check-first ordering as the meta variant: fail before reading.
    let lineage = v
        .yearly_derivation
        .as_ref()
        .ok_or_else(|| Error::Base("yearly lineage missing".into()))?;
    if facts.version_id != lineage.base_daily_version_id
        || facts.symbol != v.symbol
        || facts.period != Period::Day
    {
        return Err(Error::Base(
            "fixed base identity/period/adjustment/source mismatch".into(),
        ));
    }
    let meta = repository
        .fixed_window(&facts.version_id, facts.first_day, facts.first_day)
        .map_err(base_error)?;
    validate_base_facts_meta(record, repository, facts, &meta)
}
/// `validate_base_facts` with the caller's already-loaded one-day base
/// window — identical checks, skipping the redundant materialize.
pub(crate) fn validate_base_facts_meta<R: MarketRepository + ?Sized>(
    record: &CheckedRecord,
    repository: &R,
    facts: &FixedFacts,
    meta: &CheckedRecord,
) -> Result<(), Error> {
    let v = &record.version;
    if v.period == Period::Day {
        return Ok(());
    }
    let lineage = v
        .yearly_derivation
        .as_ref()
        .ok_or_else(|| Error::Base("yearly lineage missing".into()))?;
    if facts.version_id != lineage.base_daily_version_id
        || facts.symbol != v.symbol
        || facts.period != Period::Day
    {
        return Err(Error::Base(
            "fixed base identity/period/adjustment/source mismatch".into(),
        ));
    }
    let daily = &meta.version();
    if daily.version_id != lineage.base_daily_version_id
        || daily.symbol != v.symbol
        || daily.period != Period::Day
        || daily.adjustment != v.adjustment
        || daily.source != v.source
        || daily.unit_currency != v.unit_currency
        || !Adjustment::Forward.matches_evidence(&daily.adjustment_evidence)
    {
        return Err(Error::Base(
            "fixed base identity/period/adjustment/source mismatch".into(),
        ));
    }
    let years: std::collections::BTreeSet<i32> = v.records.iter().map(|b| b.date.year()).collect();
    let as_of = v
        .published_at
        .with_timezone(&FixedOffset::east_opt(8 * 3600).unwrap())
        .date_naive();
    let key = crate::yearly::aggregate_key(&facts.integrity_sha256, &years, as_of);
    let base_id = facts.version_id.clone();
    crate::yearly::trace_key(
        "memo.key.validate_base_facts",
        &facts.integrity_sha256,
        &years,
        as_of,
    );
    let annual =
        crate::yearly::aggregate_memoized(key, as_of, Some(lineage.last_trading_date), || {
            let window = repository
                .fixed_window(
                    &base_id,
                    lineage.first_trading_date,
                    lineage.last_trading_date,
                )
                .map_err(base_error)?;
            crate::yearly::aggregate(window.version(), Some(&years), as_of)
                .map_err(|e| Error::Base(e.to_string()))
        })?;
    if annual
        .records
        .iter()
        .map(|r| r.bar.clone())
        .collect::<Vec<_>>()
        != v.records
        || lineage.algorithm_version != crate::yearly::ALGORITHM_VERSION
        || annual.records.first().map(|r| r.lineage.first_trading_date)
            != Some(lineage.first_trading_date)
        || annual.records.last().map(|r| r.lineage.last_trading_date)
            != Some(lineage.last_trading_date)
    {
        return Err(Error::Base(
            "yearly content/lineage does not match fixed daily".into(),
        ));
    }
    Ok(())
}
pub(crate) fn validate_base(
    record: &CheckedRecord,
    base: Option<&CheckedRecord>,
) -> Result<(), Error> {
    validate_base_identity(record, base)?;
    let v = &record.version;
    if v.period == Period::Day {
        return Ok(());
    }
    let lineage = v.yearly_derivation.as_ref().unwrap();
    let daily = &base.unwrap().version;
    let years = v.records.iter().map(|b| b.date.year()).collect();
    let as_of = v
        .published_at
        .with_timezone(&FixedOffset::east_opt(8 * 3600).unwrap())
        .date_naive();
    let annual = crate::yearly::aggregate(daily, Some(&years), as_of)
        .map_err(|e| Error::Base(e.to_string()))?;
    if annual
        .records
        .iter()
        .map(|r| r.bar.clone())
        .collect::<Vec<_>>()
        != v.records
        || lineage.algorithm_version != crate::yearly::ALGORITHM_VERSION
        || annual.records.first().map(|r| r.lineage.first_trading_date)
            != Some(lineage.first_trading_date)
        || annual.records.last().map(|r| r.lineage.last_trading_date)
            != Some(lineage.last_trading_date)
    {
        return Err(Error::Base(
            "yearly content/lineage does not match fixed daily".into(),
        ));
    }
    Ok(())
}
