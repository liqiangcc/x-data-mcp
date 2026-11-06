//! Query selection and pagination decisions, independent of SQLite and MCP.
use super::{
    Access,
    cursor_cache::{Content, CursorCache, CursorLimits, CursorPool, Saved},
    read_limits::{Permit, ReadBudget, ReadLimits},
    wire::*,
};
use crate::market_storage::Error;
use chrono::{NaiveDate, Utc};
use std::{cell::RefCell, collections::BTreeSet, ops::Range, sync::Arc};
pub const MAX_ROWS: usize = 100_000;
pub const MAX_BYTES: usize = 16 * 1024 * 1024;
#[derive(Clone)]
pub struct QueryPlan {
    pub args: Query,
    pub entities: Vec<Security>,
    /// Concrete catalog version a `universe` selection resolved to; `None`
    /// for entity selections and for `market.index` (static registry).
    pub catalog_version: Option<String>,
    /// Trading days of a `cross_section` filter, resolved from the calendar.
    pub trade_days: Vec<NaiveDate>,
    /// M2 point-in-time arguments with `as_of` resolved (issue-391 §12.2).
    /// `None` for datasets that declare neither parameter.
    pub temporal: Option<TemporalPlan>,
}
/// The resolved M2 point-in-time state of one query. `as_of` is resolved in
/// [`QueryPlan::resolve`] (the plan itself never reads the clock); the
/// information-set cutoff is the caller's value, never defaulted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TemporalPlan {
    pub as_of: NaiveDate,
    pub known_at: Option<NaiveDate>,
}
impl QueryPlan {
    pub fn new(mut args: Query, access: &Access) -> Result<Self, Error> {
        access.require(args.dataset, None)?;
        page_size(args.page_size)?;
        // M3-2 PR-C (D-70 Q4): only `market.daily` has a read 口径 switch.
        if args.adjustment.is_some() && args.dataset != Dataset::Daily {
            return Err(invalid("adjustment is supported only for market.daily"));
        }
        // v2 shape only (v8 PR-3, #387 §4.3): `fields` projects columns, so a
        // v1 request carrying it is rejected rather than silently ignored.
        if args.contract_version == ReadContractVersion::V1 {
            if args.fields.is_some() {
                return Err(invalid("fields requires contract_version v2"));
            }
            if args.dataset.is_v2_only() {
                return Err(invalid(&format!(
                    "{} requires contract_version v2",
                    args.dataset.name()
                )));
            }
            if matches!(
                args.selection,
                Selection::Universe { .. } | Selection::Market { .. }
            ) {
                return Err(invalid("selection kind requires contract_version v2"));
            }
            if matches!(args.filter, Filter::CrossSection { .. }) {
                return Err(invalid(
                    "filter.kind=cross_section requires contract_version v2",
                ));
            }
        }
        // M3 #446 §10.3: `universe.scope` is a v2-only widening; v1 above
        // already rejects every `universe` selection, so a v1 request cannot
        // quietly acquire the listed-ever member set.
        universe_scope(args.dataset, &args.selection)?;
        let entities = entities(args.dataset, &args.selection, access)?;
        match (&args.filter, args.dataset) {
            (
                Filter::Daily {
                    start_date,
                    end_date,
                    ..
                },
                dataset,
            ) if matches!(
                dataset,
                Dataset::Daily
                    | Dataset::Index
                    | Dataset::AdjustmentEvents
                    | Dataset::Calendar
                    | Dataset::SecurityMaster
                    | Dataset::SecurityNames
                    | Dataset::CapitalEvents
                    | Dataset::RiskWarnings
                    | Dataset::Suspensions
                    | Dataset::PriceLimitRules
                    | Dataset::IndustryAssignments
            ) && valid_range(*start_date, *end_date) => {}
            (
                Filter::CrossSection {
                    start_date,
                    end_date,
                },
                Dataset::Daily | Dataset::Index,
            ) if valid_range(*start_date, *end_date) => {}
            (
                Filter::Yearly {
                    start_year,
                    end_year,
                },
                Dataset::Yearly,
            ) if *start_year >= 1 && end_year >= start_year && *end_year <= 9999 => {}
            (Filter::Catalog { .. }, Dataset::Catalog) => {}
            _ => return Err(invalid("dataset/filter range mismatch")),
        }
        match &mut args.version {
            VersionChoice::LatestPerEntity {} => {}
            VersionChoice::CatalogVersion { version_id }
                if args.dataset == Dataset::Catalog && !version_id.is_empty() => {}
            VersionChoice::ExplicitVersionMap { versions }
                if matches!(
                    args.dataset,
                    Dataset::Daily
                        | Dataset::Yearly
                        | Dataset::Index
                        | Dataset::AdjustmentEvents
                        | Dataset::CapitalEvents
                ) && !matches!(args.selection, Selection::Universe { .. }) =>
            {
                versions.sort_by(|a, b| a.entity.cmp(&b.entity));
                if versions
                    .iter()
                    .map(|v| v.entity.clone())
                    .collect::<Vec<_>>()
                    != entities
                    || versions.iter().any(|v| v.version_id.is_empty())
                {
                    return Err(invalid("explicit version map must match selection exactly"));
                }
            }
            _ => return Err(invalid("invalid version selection")),
        }
        if let Selection::Batch { entities: e } = &mut args.selection {
            *e = entities.clone();
        }
        // M2 point-in-time parameters are accepted only where declared
        // (issue-391 §12.3). The decode layer already rejects them; this is
        // the typed-request backstop for callers that bypass normalization.
        let temporal = args.dataset.temporal();
        let as_of = args.filter.as_of();
        if as_of.is_some() && !temporal.as_of {
            return Err(invalid(&unsupported("filter.as_of", args.dataset)));
        }
        if args.known_at.is_some() && !temporal.known_at {
            return Err(invalid(&unsupported("known_at", args.dataset)));
        }
        // §11: `catalog_version` (the main table's projection of today) and
        // `as_of` (a historical membership time point) cannot both define the
        // same universe.
        if args.dataset.is_temporal_fact()
            && matches!(
                &args.selection,
                Selection::Universe {
                    catalog_version: Some(_),
                    ..
                }
            )
            && as_of.is_some()
        {
            return Err(invalid(
                "selection.catalog_version and filter.as_of are mutually exclusive for point-in-time universes",
            ));
        }
        let temporal =
            args.dataset
                .is_temporal_fact()
                .then(|| crate::data_supply::query::TemporalPlan {
                    as_of: as_of.unwrap_or_else(|| {
                        // Replaced in `resolve` once the calendar is available; the
                        // value here only keeps `TemporalPlan` non-optional.
                        match &args.filter {
                            Filter::Daily { end_date, .. } => *end_date,
                            _ => NaiveDate::from_ymd_opt(1970, 1, 1).unwrap(),
                        }
                    }),
                    known_at: args.known_at,
                });
        Ok(Self {
            args,
            entities,
            catalog_version: None,
            trade_days: Vec::new(),
            temporal,
        })
    }
    /// Resolve the parts that need storage: a universe's members and the
    /// trading days of a cross-section. Runs before the cursor key is built so
    /// the key contains the concrete catalog version.
    pub fn resolve<R: ReadRepository>(&mut self, repo: &R, access: &Access) -> Result<(), Error> {
        if let Some(temporal) = &self.temporal {
            let as_of = match self.args.filter.as_of() {
                Some(as_of) => as_of,
                None => repo.last_trading_day_upto(today())?,
            };
            self.temporal = Some(TemporalPlan {
                as_of,
                known_at: temporal.known_at,
            });
        }
        let (as_of, known_at) = self
            .temporal
            .map_or((None, None), |t| (Some(t.as_of), t.known_at));
        if let Selection::Universe {
            catalog_version,
            scope,
        } = &mut self.args.selection
        {
            let scope = *scope;
            let (resolved, members) = repo.universe_members(
                self.args.dataset,
                catalog_version.as_deref(),
                scope,
                as_of,
                known_at,
            )?;
            let resolved = (!resolved.is_empty()).then_some(resolved);
            self.catalog_version = resolved.clone();
            *catalog_version = resolved;
            self.entities = checked_members(self.args.dataset, members, access)?;
        }
        if let Filter::CrossSection {
            start_date,
            end_date,
        } = self.args.filter
        {
            let days = repo.calendar_days(start_date, end_date)?;
            if days.len() > 5 {
                return Err(invalid(&format!(
                    "cross_section window is {} trading days; at most 5 are allowed. \
                     Use filter.kind=daily with selection.kind=batch for a longer panel",
                    days.len()
                )));
            }
            self.trade_days = days;
        }
        Ok(())
    }
}
/// The adapter's "today": the calendar day, never a data fact. The read model
/// itself never reads the clock (issue-391 §12.2); resolving the default here
/// keeps that property.
///
/// This is the Shanghai day, not the host's local day. It used to be
/// `chrono::Local::now()`, so the default `as_of` depended on the host's
/// timezone: vm2-126 runs Asia/Shanghai and aws-node runs UTC, and between
/// 16:00 and 24:00 UTC the two hosts disagreed by one day.
pub fn today() -> NaiveDate {
    crate::calendar::shanghai_today()
}
fn unsupported(field: &str, dataset: Dataset) -> String {
    format!(
        "unsupported_for_dataset: {field} is not declared for dataset {}",
        dataset.name()
    )
}
fn valid_range(start_date: NaiveDate, end_date: NaiveDate) -> bool {
    start_date <= end_date
        && start_date >= NaiveDate::from_ymd_opt(1, 1, 1).unwrap()
        && end_date <= NaiveDate::from_ymd_opt(9999, 12, 31).unwrap()
}

/// Validate the optional `universe.scope` (#446 §10.3).
///
/// `None` is the pre-M3 member set (a catalog version, or the as-of universe
/// for the M2 datasets), so omitting the field changes nothing. The only
/// accepted value resolves `market.daily`'s members from the main table, and it
/// cannot be combined with `catalog_version`: two different member sources must
/// not silently override one another.
fn universe_scope(dataset: Dataset, selection: &Selection) -> Result<(), Error> {
    let Selection::Universe {
        catalog_version,
        scope,
    } = selection
    else {
        return Ok(());
    };
    match scope {
        None => Ok(()),
        Some(CatalogScope::CurrentShSzA) => Err(invalid(
            "selection.scope current_sh_sz_a is the catalog default; omit scope",
        )),
        Some(
            scope @ (CatalogScope::ShSzAListedEver
            | CatalogScope::ShSzEtfCurrent
            | CatalogScope::ShSzEtfListedEver),
        ) => {
            if dataset != Dataset::Daily {
                return Err(invalid(&format!(
                    "selection.scope {} is only supported for market.daily",
                    scope_name(*scope)
                )));
            }
            if catalog_version.is_some() {
                return Err(invalid(
                    "selection.catalog_version and selection.scope are mutually exclusive",
                ));
            }
            Ok(())
        }
    }
}
fn scope_name(scope: CatalogScope) -> &'static str {
    match scope {
        CatalogScope::CurrentShSzA => "current_sh_sz_a",
        CatalogScope::ShSzAListedEver => "sh_sz_a_listed_ever",
        CatalogScope::ShSzEtfCurrent => "sh_sz_etf_current",
        CatalogScope::ShSzEtfListedEver => "sh_sz_etf_listed_ever",
    }
}

fn checked_members(
    dataset: Dataset,
    mut members: Vec<Security>,
    access: &Access,
) -> Result<Vec<Security>, Error> {
    members.sort();
    if members.windows(2).any(|w| w[0] == w[1]) {
        return Err(invalid("duplicate universe entity"));
    }
    for entity in &members {
        entity.validate().map_err(Error::Invalid)?;
        access.require(dataset, Some(entity))?;
    }
    Ok(members)
}
pub fn invalid(s: &str) -> Error {
    Error::Invalid(s.into())
}
pub fn page_size(n: usize) -> Result<(), Error> {
    if (1..=1000).contains(&n) {
        Ok(())
    } else {
        Err(invalid("page_size must be 1..1000"))
    }
}
pub fn entities(dataset: Dataset, s: &Selection, access: &Access) -> Result<Vec<Security>, Error> {
    // `sh_sz_a_listed_ever` is a write-selection scope resolved from the
    // security master; a catalog read must fail closed instead of returning
    // the current-member subset under the same name (M3 #446 §4.1).
    if let (Dataset::Catalog, Selection::Catalog { scope }) = (dataset, s)
        && *scope != CatalogScope::CurrentShSzA
    {
        return Err(invalid(
            "securities.catalog only supports scope current_sh_sz_a; \
             sh_sz_a_listed_ever / sh_sz_etf_current / sh_sz_etf_listed_ever \
             are market.daily scopes resolved from the main table",
        ));
    }
    let mut e = match (dataset, s) {
        (Dataset::Catalog, Selection::Catalog { .. }) => vec![],
        (Dataset::Calendar | Dataset::PriceLimitRules, Selection::Market { .. }) => vec![],
        (_, Selection::Universe { .. }) => vec![],
        (
            Dataset::Daily
            | Dataset::Yearly
            | Dataset::Index
            | Dataset::AdjustmentEvents
            | Dataset::SecurityMaster
            | Dataset::SecurityNames
            | Dataset::CapitalEvents
            | Dataset::RiskWarnings
            | Dataset::Suspensions
            | Dataset::IndustryAssignments,
            Selection::Single { entity },
        ) => vec![entity.clone()],
        (
            Dataset::Daily
            | Dataset::Yearly
            | Dataset::Index
            | Dataset::AdjustmentEvents
            | Dataset::SecurityMaster
            | Dataset::SecurityNames
            | Dataset::CapitalEvents
            | Dataset::RiskWarnings
            | Dataset::Suspensions
            | Dataset::IndustryAssignments,
            Selection::Batch { entities },
        ) => entities.clone(),
        _ => return Err(invalid("invalid selection for dataset")),
    };
    if !matches!(
        (dataset, s),
        (Dataset::Catalog, Selection::Catalog { .. })
            | (
                Dataset::Calendar | Dataset::PriceLimitRules,
                Selection::Market { .. }
            )
            | (_, Selection::Universe { .. })
    ) && (e.is_empty() || e.len() > 100)
    {
        return Err(invalid("selection must contain 1..100 entities"));
    }
    e.sort();
    if e.windows(2).any(|w| w[0] == w[1]) {
        return Err(invalid("duplicate query entity"));
    }
    for e in &e {
        e.validate().map_err(Error::Invalid)?;
        access.require(dataset, Some(e))?;
    }
    Ok(e)
}
/// Whether `plan` is a v2 `universe` read of a price dataset, the only shape
/// whose pages are cut down to their own entities.
fn is_universe_price_read(plan: &QueryPlan) -> bool {
    plan.args.contract_version == ReadContractVersion::V2
        && matches!(plan.args.selection, Selection::Universe { .. })
        && matches!(plan.args.dataset, Dataset::Daily | Dataset::Index)
}

/// Index of `rows[at]`'s entity in the sorted `plan.entities`.
fn row_entity(plan: &QueryPlan, rows: &[Row], at: usize) -> Option<usize> {
    match rows.get(at)? {
        Row::Market { entity, .. } => plan.entities.binary_search(entity).ok(),
        _ => None,
    }
}

/// The entities of `plan.entities` that the page `rows[start..end]` reports.
///
/// Rows and manifest entries both follow the sorted entity order, one manifest
/// entry per entity (present or absent). The page reports the entities of its
/// rows, from the entity of its first row up to where the next page starts, so
/// an entity without any row (an absent version, or no bar in the window) is
/// reported by the page before the next entity that has rows. The first page
/// starts at 0 and the last ends at the entity count, so every manifest entry
/// is reported by some page. An entity whose rows straddle two pages is
/// reported by both, which keeps `entities`, `versions` and `columns` aligned
/// inside each page.
fn universe_span(
    plan: &QueryPlan,
    data: &QuerySnapshot,
    start: usize,
    end: usize,
) -> Option<Range<usize>> {
    if !is_universe_price_read(plan) || data.manifest.len() != plan.entities.len() {
        return None;
    }
    let count = plan.entities.len();
    if data.rows.is_empty() {
        return Some(0..count);
    }
    let lo = if start == 0 {
        0
    } else {
        row_entity(plan, &data.rows, start)?
    };
    let hi = if end >= data.rows.len() {
        count
    } else {
        let last = row_entity(plan, &data.rows, end - 1)?;
        let next = row_entity(plan, &data.rows, end)?;
        if next == last { last + 1 } else { next }
    };
    Some(lo..hi)
}

/// Serialized size of the whole snapshot in the v2 columnar answer it will be
/// given page by page (every entity, every row, the requested fields), for a
/// v2 `universe` price read; `None` for every other query.
fn universe_output_bytes(plan: &QueryPlan, data: &QuerySnapshot) -> Option<usize> {
    if !is_universe_price_read(plan) {
        return None;
    }
    let (allowed, default) = super::columnar::query_fields(plan.args.dataset);
    let fields = super::columnar::select(
        plan.args.fields.as_deref(),
        allowed,
        default,
        plan.args.dataset.name(),
    )
    .ok()?;
    let meta = super::columnar::PageMeta {
        snapshot_id: "",
        snapshot_time: Utc::now(),
        next_cursor: None,
        page_complete: true,
        limitations: &[],
        temporal: None,
    };
    let whole = if matches!(plan.args.filter, Filter::CrossSection { .. }) {
        super::columnar::cross_section_page(
            plan.args.dataset,
            &data.manifest,
            &plan.trade_days,
            &plan.entities,
            &data.rows,
            &fields,
            &meta,
            None,
        )
    } else {
        super::columnar::panel_page(
            plan.args.dataset,
            &data.manifest,
            &plan.entities,
            &data.rows,
            &fields,
            &meta,
            None,
        )
    };
    Some(serde_json::to_vec(&whole).ok()?.len())
}
/// What [`Queries::query_page`] hands to the v1 and v2 projections.
struct Page {
    page: DataPage,
    plan: QueryPlan,
    span: Option<Range<usize>>,
    snapshot: Arc<QuerySnapshot>,
}
#[derive(Clone, serde::Serialize)]
pub struct QuerySnapshot {
    pub manifest: Vec<ManifestEntry>,
    pub rows: Vec<Row>,
    /// M2 point-in-time facts (issue-391 §12.2); `None` when the dataset
    /// declares neither parameter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temporal: Option<TemporalBlock>,
    /// Dataset-level limits the reader must be able to state truthfully
    /// (coverage windows, event-level sources, an unregistered dataset). They
    /// are appended to the page's fixed `limitations`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub limitations: Vec<String>,
}
pub trait ReadRepository {
    fn query(&self, plan: &QueryPlan, access: &Access) -> Result<QuerySnapshot, Error>;
    fn verify(&self, versions: &[VersionRef]) -> Result<(), Error>;
    fn versions(&self, args: &VersionsQuery, access: &Access) -> Result<Vec<VersionInfo>, Error>;
    /// Members of `selection.kind=universe`, plus the concrete catalog
    /// version echoed back. `market.index` returns its static registry and an
    /// empty label (no catalog is involved). M2 datasets resolve the T-day
    /// stock pool from `security_master` (issue-391 §11), using `as_of` when
    /// the caller supplied one and the last trading day up to today otherwise.
    /// `scope=sh_sz_a_listed_ever` (#446 §10.3) resolves `market.daily` from
    /// the main table instead and has no catalog version to echo.
    fn universe_members(
        &self,
        dataset: Dataset,
        catalog_version: Option<&str>,
        scope: Option<CatalogScope>,
        as_of: Option<NaiveDate>,
        known_at: Option<NaiveDate>,
    ) -> Result<(String, Vec<Security>), Error>;
    /// Trading days in `[start, end]`, from the derived calendar.
    fn calendar_days(&self, start: NaiveDate, end: NaiveDate) -> Result<Vec<NaiveDate>, Error>;
    /// The last trading day on or before `day`. Used only to resolve the M2
    /// `as_of` default (issue-391 §12.2: "today", the last trading day).
    fn last_trading_day_upto(&self, day: NaiveDate) -> Result<NaiveDate, Error>;
}
pub struct Queries<R> {
    pub repository: R,
    cache: CursorCache,
    budget: Arc<ReadBudget>,
    /// Heap reserved for the call in progress on this connection thread.
    inflight: RefCell<Option<Permit>>,
    instance: String,
    sequence: RefCell<u64>,
}
/// Releases the call's memory reservation when the response is built.
struct Inflight<'a>(&'a RefCell<Option<Permit>>);
impl Drop for Inflight<'_> {
    fn drop(&mut self) {
        self.0.borrow_mut().take();
    }
}
impl<R: ReadRepository> Queries<R> {
    /// Stand-alone limits: a private cursor pool with the production caps and
    /// no memory budget. The service uses [`Queries::with_limits`] so every
    /// connection shares one pool.
    pub fn new(repository: R) -> Self {
        Self::with_limits(
            repository,
            &ReadLimits::new(CursorLimits::PRODUCTION, ReadBudget::unlimited()),
        )
    }
    /// One connection's `Queries` over the process-wide pool and budget.
    pub fn with_limits(repository: R, limits: &ReadLimits) -> Self {
        Self {
            repository,
            cache: CursorCache::new(limits.pool.clone()),
            budget: limits.budget.clone(),
            inflight: RefCell::new(None),
            instance: format!(
                "{}-{}",
                std::process::id(),
                Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ),
            sequence: RefCell::new(0),
        }
    }
    /// Swap the build headroom (if any) for the working set of one page.
    /// The old permit goes first so the call is never counted twice.
    fn reserve_pages(&self, snapshot_bytes: usize) -> Result<(), Error> {
        self.inflight.borrow_mut().take();
        let permit = self.budget.reserve(snapshot_bytes)?;
        *self.inflight.borrow_mut() = Some(permit);
        Ok(())
    }
    fn id(&self) -> String {
        let mut n = self.sequence.borrow_mut();
        *n += 1;
        // The owner makes ids unique across connections sharing a pool.
        format!("{}-{}-{n}", self.instance, self.cache.owner())
    }
    /// Test seam: shrink the cache caps so eviction behavior is exercisable
    /// without multi-MB fixtures. Production uses `new`/`with_limits`.
    #[doc(hidden)]
    pub fn with_cache_limits(repository: R, entry_cap: usize, byte_cap: usize) -> Self {
        Self::with_limits(
            repository,
            &ReadLimits::new(
                CursorLimits {
                    total_bytes: byte_cap,
                    owner_bytes: byte_cap,
                    owner_entries: entry_cap,
                },
                ReadBudget::unlimited(),
            ),
        )
    }
    /// Test seam: (entries, distinct-snapshot bytes, distinct snapshots) of
    /// this connection.
    #[doc(hidden)]
    pub fn cache_stats(&self) -> (usize, usize, usize) {
        self.cache.stats()
    }
    /// Test seam: this connection's id in the shared pool.
    #[doc(hidden)]
    pub fn pool(&self) -> &CursorPool {
        self.cache.pool()
    }
    fn save(&self, mut s: Saved) -> String {
        let token = self.id();
        s.token = token.clone();
        self.cache.push(s);
        token
    }
    fn resume(&self, token: &str, key: &str, access: &Access) -> Result<Saved, Error> {
        let s = self
            .cache
            .resume(token)
            .ok_or_else(|| invalid("invalid/expired cursor"))?;
        if s.key != key || &s.access != access {
            return Err(invalid(&cursor_mismatch(&s.key, key)));
        }
        Ok(s)
    }
    pub fn query(&self, args: Query, access: &Access) -> Result<DataPage, Error> {
        let _inflight = Inflight(&self.inflight);
        Ok(self.query_page(args, access, true)?.page)
    }
    /// v2 columnar read (v8 PR-3, #387 §5): the same snapshot, page slice and
    /// version verification as v1; only the projection differs.
    pub fn query_v2(&self, args: Query, access: &Access) -> Result<serde_json::Value, Error> {
        let _inflight = Inflight(&self.inflight);
        let (allowed, default) = super::columnar::query_fields(args.dataset);
        let fields = super::columnar::select(
            args.fields.as_deref(),
            allowed,
            default,
            args.dataset.name(),
        )?;
        // The columnar answer reads the snapshot's own manifest, so the page
        // does not carry a copy of it.
        let Page {
            page,
            plan,
            span,
            snapshot,
        } = self.query_page(args, access, false)?;
        let manifest = &snapshot.manifest;
        let meta = super::columnar::PageMeta {
            snapshot_id: &page.snapshot_id,
            snapshot_time: page.query_time,
            next_cursor: page.next_cursor.as_deref(),
            page_complete: page.page_complete,
            limitations: &page.limitations,
            temporal: page.temporal.as_ref(),
        };
        let mut page = match plan.args.dataset {
            Dataset::Catalog => {
                super::columnar::catalog_table(manifest, &page.records, &fields, &meta)
            }
            Dataset::AdjustmentEvents | Dataset::Calendar => super::columnar::row_table(
                plan.args.dataset,
                manifest,
                &page.records,
                &fields,
                &meta,
            ),
            dataset if dataset.is_temporal_fact() => {
                super::columnar::fact_table(dataset, manifest, &page.records, &fields, &meta)
            }
            Dataset::Daily | Dataset::Index
                if matches!(&plan.args.filter, Filter::CrossSection { .. }) =>
            {
                super::columnar::cross_section_page(
                    plan.args.dataset,
                    manifest,
                    &plan.trade_days,
                    &plan.entities,
                    &page.records,
                    &fields,
                    &meta,
                    span,
                )
            }
            dataset => super::columnar::panel_page(
                dataset,
                manifest,
                &plan.entities,
                &page.records,
                &fields,
                &meta,
                span,
            ),
        };
        if let Some(catalog_version) = &plan.catalog_version
            && let Some(object) = page.as_object_mut()
        {
            object.insert("catalog_version".into(), serde_json::json!(catalog_version));
        }
        Ok(page)
    }
    /// One page of a query: the v1 page, the resolved plan and, for a v2
    /// `universe` price read, the range of `plan.entities` this page reports
    /// (see [`universe_span`]). `None` means "every entity of the plan".
    /// `with_manifest` fills `page.version_manifest` (v1 needs it); v2 reads
    /// `snapshot.manifest` instead, so it skips the copy.
    fn query_page(&self, args: Query, access: &Access, with_manifest: bool) -> Result<Page, Error> {
        let mut plan = QueryPlan::new(args, access)?;
        plan.resolve(&self.repository, access)?;
        let cursor = plan.args.cursor.clone();
        let mut keyargs = plan.args.clone();
        keyargs.cursor = None;
        let key = format!("query:{}", serde_json::to_string(&keyargs).unwrap());
        let mut saved = if let Some(cursor) = cursor {
            self.resume(&cursor, &key, access)?
        } else {
            *self.inflight.borrow_mut() = Some(self.budget.reserve_build()?);
            let data = crate::stage_trace::timed("query.repository", || {
                self.repository.query(&plan, access)
            })?;
            let bytes = crate::stage_trace::timed("query.budget", || {
                serde_json::to_vec(&data).unwrap().len()
            });
            // The byte limit bounds what the caller receives. A v2 `universe`
            // price read is answered in columnar form, which does not repeat
            // the per-row `version_ref` that the snapshot's own serialization
            // above carries, so the gate measures that answer instead.
            if data.rows.len() > MAX_ROWS {
                return Err(invalid("snapshot budget exceeded"));
            }
            let gate = crate::stage_trace::timed("query.budget.output", || {
                universe_output_bytes(&plan, &data)
            })
            .unwrap_or(bytes);
            if gate > MAX_BYTES {
                return Err(invalid("snapshot budget exceeded"));
            }
            Saved {
                token: String::new(),
                key,
                access: access.clone(),
                time: Utc::now(),
                id: self.id(),
                offset: 0,
                content: Content::Data(Arc::new(data)),
                bytes,
            }
        };
        let Content::Data(snapshot) = &saved.content else {
            return Err(invalid("cursor category mismatch"));
        };
        let snapshot = snapshot.clone();
        let data = &*snapshot;
        self.reserve_pages(saved.bytes)?;
        let refs = data
            .manifest
            .iter()
            .filter_map(|m| m.version_ref.clone())
            .collect::<Vec<_>>();
        crate::stage_trace::timed("query.verify", || self.repository.verify(&refs))?;
        let start = saved.offset;
        let end = (saved.offset + plan.args.page_size).min(data.rows.len());
        let records = data.rows[saved.offset..end].to_vec();
        let manifest = if with_manifest {
            data.manifest.clone()
        } else {
            Vec::new()
        };
        let span = universe_span(&plan, data, start, end);
        let more = end < data.rows.len();
        saved.offset = end;
        let mut result = DataPage {
            dataset: plan.args.dataset,
            contract_version: ContractVersion::V1,
            snapshot_id: saved.id.clone(),
            query_time: saved.time,
            version_manifest: manifest,
            records,
            next_cursor: None,
            page_complete: !more,
            selection_complete: true,
            temporal: data.temporal.clone(),
            limitations: vec![
                "fixed version selection is not a shared source acquisition time".into(),
                "declared scope: owner personal research, local-only (#71 comment-5749934485); coverage is not inferred from dates".into(),
            ]
            .into_iter()
            .chain(data.limitations.iter().cloned())
            .collect(),
        };
        if more {
            result.next_cursor = Some(self.save(saved));
        }
        Ok(Page {
            page: result,
            plan,
            span,
            snapshot,
        })
    }
    pub fn versions(&self, args: VersionsQuery, access: &Access) -> Result<VersionPage, Error> {
        let _inflight = Inflight(&self.inflight);
        Ok(self.versions_page(args, access)?.0)
    }
    /// v2 version table (v8 PR-3, #387 §5.1): one row per version, the list
    /// appears once and `integrity_sha256` is gone.
    pub fn versions_v2(
        &self,
        args: VersionsQuery,
        access: &Access,
    ) -> Result<serde_json::Value, Error> {
        let _inflight = Inflight(&self.inflight);
        let (page, args) = self.versions_page(args, access)?;
        let meta = super::columnar::PageMeta {
            snapshot_id: &page.snapshot_id,
            snapshot_time: page.snapshot_time,
            next_cursor: page.next_cursor.as_deref(),
            page_complete: page.page_complete,
            limitations: &[],
            temporal: None,
        };
        let mut out = super::columnar::versions_table(args.dataset, &page.items, &meta);
        if let Selection::Universe {
            catalog_version: Some(catalog_version),
            ..
        } = &args.selection
            && let Some(object) = out.as_object_mut()
        {
            object.insert("catalog_version".into(), serde_json::json!(catalog_version));
        }
        Ok(out)
    }
    fn versions_page(
        &self,
        mut args: VersionsQuery,
        access: &Access,
    ) -> Result<(VersionPage, VersionsQuery), Error> {
        access.require(args.dataset, None)?;
        page_size(args.page_size)?;
        if args.adjustment.is_some() && args.dataset != Dataset::Daily {
            return Err(invalid("adjustment is supported only for market.daily"));
        }
        if args.contract_version == ReadContractVersion::V1 && args.dataset.is_v2_only() {
            return Err(invalid(&format!(
                "{} requires contract_version v2",
                args.dataset.name()
            )));
        }
        // The versions list accepted `universe` in v1; the M3 scope is a v2
        // field, so a v1 request carrying it is rejected rather than served
        // with a member set its contract version cannot describe.
        if args.contract_version == ReadContractVersion::V1
            && matches!(&args.selection, Selection::Universe { scope: Some(_), .. })
        {
            return Err(invalid("selection.scope requires contract_version v2"));
        }
        // M3 #446 §10.3: the versions list resolves the same member set the
        // data query would, so a scope-only universe must be validated here too
        // (this path does not build a `QueryPlan`).
        universe_scope(args.dataset, &args.selection)?;
        if let Selection::Universe {
            catalog_version,
            scope,
        } = &mut args.selection
        {
            let (resolved, members) = self.repository.universe_members(
                args.dataset,
                catalog_version.as_deref(),
                *scope,
                None,
                None,
            )?;
            *catalog_version = (!resolved.is_empty()).then_some(resolved);
            checked_members(args.dataset, members, access)?;
        } else {
            entities(args.dataset, &args.selection, access)?;
        }
        let cursor = args.cursor.take();
        let key = format!("versions:{}", serde_json::to_string(&args).unwrap());
        let mut saved = if let Some(cursor) = cursor {
            self.resume(&cursor, &key, access)?
        } else {
            *self.inflight.borrow_mut() = Some(self.budget.reserve_build()?);
            let data = self.repository.versions(&args, access)?;
            let bytes = serde_json::to_vec(&data).unwrap().len();
            if data.len() > MAX_ROWS || bytes > MAX_BYTES {
                return Err(invalid("snapshot budget exceeded"));
            }
            Saved {
                token: String::new(),
                key,
                access: access.clone(),
                time: Utc::now(),
                id: self.id(),
                offset: 0,
                content: Content::Versions(Arc::new(data)),
                bytes,
            }
        };
        let Content::Versions(data) = &saved.content else {
            return Err(invalid("cursor category mismatch"));
        };
        self.reserve_pages(saved.bytes)?;
        self.repository.verify(
            &data
                .iter()
                .map(|v| v.version_ref.clone())
                .collect::<Vec<_>>(),
        )?;
        let end = (saved.offset + args.page_size).min(data.len());
        let items = data[saved.offset..end].to_vec();
        let more = end < data.len();
        saved.offset = end;
        let mut result = VersionPage {
            snapshot_id: saved.id.clone(),
            snapshot_time: saved.time,
            items,
            next_cursor: None,
            page_complete: !more,
        };
        if more {
            result.next_cursor = Some(self.save(saved));
        }
        Ok((result, args))
    }
}
impl<R: ReadRepository + crate::data_supply::presence::PresenceRepository> Queries<R> {
    /// `get_yearly_base_years` (generic-data-v4): exact in-window year sets
    /// for pinned published daily versions under one bounded snapshot.
    pub fn year_presence(
        &self,
        args: YearPresenceQuery,
        access: &Access,
    ) -> Result<YearPresenceReply, Error> {
        crate::data_supply::presence::year_presence(&self.repository, self.id(), args, access)
    }
}
/// Names the request fields that differ from the snapshot a cursor was issued
/// for. `contract_version`/`fields` are part of the key, so a v1 cursor reused
/// on a v2 request reports `contract_version` instead of a generic mismatch
/// (v8 PR-3, #387 §5.3).
pub(crate) fn cursor_mismatch(stored: &str, requested: &str) -> String {
    let changed = key_diff(stored, requested);
    if changed.is_empty() {
        "cursor conditions/authorization changed".into()
    } else {
        format!(
            "cursor conditions changed: {}; start a new query",
            changed.join(", ")
        )
    }
}

fn key_diff(stored: &str, requested: &str) -> Vec<String> {
    let (Some((stored_kind, stored_body)), Some((requested_kind, requested_body))) =
        (stored.split_once(':'), requested.split_once(':'))
    else {
        return vec![];
    };
    if stored_kind != requested_kind {
        return vec![stored_kind.to_owned()];
    }
    let (Ok(serde_json::Value::Object(stored)), Ok(serde_json::Value::Object(requested))) = (
        serde_json::from_str(stored_body),
        serde_json::from_str(requested_body),
    ) else {
        return vec![];
    };
    let keys: BTreeSet<&String> = stored.keys().chain(requested.keys()).collect();
    keys.into_iter()
        .filter(|key| stored.get(*key) != requested.get(*key))
        .cloned()
        .collect()
}

/// Deduplicates verification work without changing query selection semantics.
pub fn unique_refs(refs: &[VersionRef]) -> Vec<VersionRef> {
    let mut seen = BTreeSet::new();
    refs.iter()
        .filter(|r| seen.insert((r.dataset, r.version_id.clone())))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etf_universe_scopes_are_daily_only_and_not_a_catalog() {
        let selection = Selection::Universe {
            catalog_version: None,
            scope: Some(CatalogScope::ShSzEtfCurrent),
        };
        assert!(universe_scope(Dataset::Daily, &selection).is_ok());
        let error = universe_scope(Dataset::Index, &selection).unwrap_err();
        assert!(error.to_string().contains("sh_sz_etf_current"));

        let with_catalog = Selection::Universe {
            catalog_version: Some("catalog-1".into()),
            scope: Some(CatalogScope::ShSzEtfListedEver),
        };
        assert!(universe_scope(Dataset::Daily, &with_catalog).is_err());
    }
}
