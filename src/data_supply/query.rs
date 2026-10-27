//! Query selection and pagination decisions, independent of SQLite and MCP.
use super::{Access, wire::*};
use crate::market_storage::Error;
use chrono::{NaiveDate, Utc};
use std::{
    cell::RefCell,
    collections::{BTreeSet, HashMap, VecDeque},
    rc::Rc,
};
pub const MAX_ROWS: usize = 100_000;
pub const MAX_BYTES: usize = 16 * 1024 * 1024;
const CACHE_ENTRIES: usize = 32;
const CACHE_BYTES: usize = 64 * 1024 * 1024;
#[derive(Clone)]
pub struct QueryPlan {
    pub args: Query,
    pub entities: Vec<Security>,
    /// Concrete catalog version a `universe` selection resolved to; `None`
    /// for entity selections and for `market.index` (static registry).
    pub catalog_version: Option<String>,
    /// Trading days of a `cross_section` filter, resolved from the calendar.
    pub trade_days: Vec<NaiveDate>,
}
impl QueryPlan {
    pub fn new(mut args: Query, access: &Access) -> Result<Self, Error> {
        access.require(args.dataset, None)?;
        page_size(args.page_size)?;
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
        let entities = entities(args.dataset, &args.selection, access)?;
        match (&args.filter, args.dataset) {
            (
                Filter::Daily {
                    start_date,
                    end_date,
                },
                dataset,
            ) if matches!(
                dataset,
                Dataset::Daily | Dataset::Index | Dataset::AdjustmentEvents | Dataset::Calendar
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
                    Dataset::Daily | Dataset::Yearly | Dataset::Index | Dataset::AdjustmentEvents
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
        Ok(Self {
            args,
            entities,
            catalog_version: None,
            trade_days: Vec::new(),
        })
    }
    /// Resolve the parts that need storage: a universe's members and the
    /// trading days of a cross-section. Runs before the cursor key is built so
    /// the key contains the concrete catalog version.
    pub fn resolve<R: ReadRepository>(&mut self, repo: &R, access: &Access) -> Result<(), Error> {
        if let Selection::Universe { catalog_version } = &mut self.args.selection {
            let (resolved, members) =
                repo.universe_members(self.args.dataset, catalog_version.as_deref())?;
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
fn valid_range(start_date: NaiveDate, end_date: NaiveDate) -> bool {
    start_date <= end_date
        && start_date >= NaiveDate::from_ymd_opt(1, 1, 1).unwrap()
        && end_date <= NaiveDate::from_ymd_opt(9999, 12, 31).unwrap()
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
    let mut e = match (dataset, s) {
        (Dataset::Catalog, Selection::Catalog { .. }) => vec![],
        (Dataset::Calendar, Selection::Market { .. }) => vec![],
        (_, Selection::Universe { .. }) => vec![],
        (
            Dataset::Daily | Dataset::Yearly | Dataset::Index | Dataset::AdjustmentEvents,
            Selection::Single { entity },
        ) => vec![entity.clone()],
        (
            Dataset::Daily | Dataset::Yearly | Dataset::Index | Dataset::AdjustmentEvents,
            Selection::Batch { entities },
        ) => entities.clone(),
        _ => return Err(invalid("invalid selection for dataset")),
    };
    if !matches!(
        (dataset, s),
        (Dataset::Catalog, Selection::Catalog { .. })
            | (Dataset::Calendar, Selection::Market { .. })
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
#[derive(Clone, serde::Serialize)]
pub struct QuerySnapshot {
    pub manifest: Vec<ManifestEntry>,
    pub rows: Vec<Row>,
}
pub trait ReadRepository {
    fn query(&self, plan: &QueryPlan, access: &Access) -> Result<QuerySnapshot, Error>;
    fn verify(&self, versions: &[VersionRef]) -> Result<(), Error>;
    fn versions(&self, args: &VersionsQuery, access: &Access) -> Result<Vec<VersionInfo>, Error>;
    /// Members of `selection.kind=universe`, plus the concrete catalog
    /// version echoed back. `market.index` returns its static registry and an
    /// empty label (no catalog is involved).
    fn universe_members(
        &self,
        dataset: Dataset,
        catalog_version: Option<&str>,
    ) -> Result<(String, Vec<Security>), Error>;
    /// Trading days in `[start, end]`, from the derived calendar.
    fn calendar_days(&self, start: NaiveDate, end: NaiveDate) -> Result<Vec<NaiveDate>, Error>;
}
#[derive(Clone)]
struct Saved {
    token: String,
    key: String,
    access: Access,
    time: chrono::DateTime<Utc>,
    id: String,
    offset: usize,
    content: Content,
    /// Serialized size recorded at the snapshot-budget check, so resumed
    /// entries never re-serialize. Entries sharing `id` point at the same
    /// Rc'd snapshot and are accounted once in `CursorCache::bytes`.
    bytes: usize,
}
#[derive(Clone)]
enum Content {
    Data(Rc<QuerySnapshot>),
    Versions(Rc<Vec<VersionInfo>>),
}
/// LRU cursor cache. Entries are cheap handles (Rc + metadata); capacity is
/// bounded by entry count AND by the summed bytes of distinct snapshots.
struct CursorCache {
    entries: VecDeque<Saved>,
    /// Live entry count per snapshot id (page cursors of one query share
    /// `id` — and the snapshot — via resume/save).
    refs: HashMap<String, u32>,
    bytes: usize,
    entry_cap: usize,
    byte_cap: usize,
}
impl Default for CursorCache {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
            refs: HashMap::new(),
            bytes: 0,
            entry_cap: CACHE_ENTRIES,
            byte_cap: CACHE_BYTES,
        }
    }
}
impl CursorCache {
    fn release(&mut self, s: &Saved) {
        if let Some(n) = self.refs.get_mut(&s.id) {
            *n -= 1;
            if *n == 0 {
                self.refs.remove(&s.id);
                self.bytes -= s.bytes;
            }
        }
    }
    fn push(&mut self, s: Saved) {
        let new_bytes = if self.refs.contains_key(&s.id) {
            0
        } else {
            s.bytes
        };
        while self.entries.len() >= self.entry_cap || self.bytes + new_bytes > self.byte_cap {
            match self.entries.pop_front() {
                Some(old) => self.release(&old),
                None => break,
            }
        }
        *self.refs.entry(s.id.clone()).or_insert(0) += 1;
        self.bytes += new_bytes;
        self.entries.push_back(s);
    }
    /// LRU: resuming a cursor marks it most-recently-used.
    fn resume(&mut self, token: &str) -> Option<Saved> {
        let pos = self.entries.iter().position(|s| s.token == token)?;
        let s = self.entries.remove(pos)?;
        self.entries.push_back(s.clone());
        Some(s)
    }
    /// (entries, distinct-snapshot bytes, distinct snapshots)
    fn stats(&self) -> (usize, usize, usize) {
        (self.entries.len(), self.bytes, self.refs.len())
    }
}
pub struct Queries<R> {
    pub repository: R,
    cache: RefCell<CursorCache>,
    instance: String,
    sequence: RefCell<u64>,
}
impl<R: ReadRepository> Queries<R> {
    pub fn new(repository: R) -> Self {
        Self {
            repository,
            cache: RefCell::new(CursorCache::default()),
            instance: format!(
                "{}-{}",
                std::process::id(),
                Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ),
            sequence: RefCell::new(0),
        }
    }
    fn id(&self) -> String {
        let mut n = self.sequence.borrow_mut();
        *n += 1;
        format!("{}-{n}", self.instance)
    }
    /// Test seam: shrink the cache caps so eviction behavior is exercisable
    /// without multi-MB fixtures. Production uses `new`.
    #[doc(hidden)]
    pub fn with_cache_limits(repository: R, entry_cap: usize, byte_cap: usize) -> Self {
        let q = Self::new(repository);
        let mut c = q.cache.borrow_mut();
        c.entry_cap = entry_cap;
        c.byte_cap = byte_cap;
        drop(c);
        q
    }
    /// Test seam: (entries, distinct-snapshot bytes, distinct snapshots).
    #[doc(hidden)]
    pub fn cache_stats(&self) -> (usize, usize, usize) {
        self.cache.borrow().stats()
    }
    fn save(&self, mut s: Saved) -> String {
        let token = self.id();
        s.token = token.clone();
        self.cache.borrow_mut().push(s);
        token
    }
    fn resume(&self, token: &str, key: &str, access: &Access) -> Result<Saved, Error> {
        let s = self
            .cache
            .borrow_mut()
            .resume(token)
            .ok_or_else(|| invalid("invalid/expired cursor"))?;
        if s.key != key || &s.access != access {
            return Err(invalid(&cursor_mismatch(&s.key, key)));
        }
        Ok(s)
    }
    pub fn query(&self, args: Query, access: &Access) -> Result<DataPage, Error> {
        Ok(self.query_page(args, access)?.0)
    }
    /// v2 columnar read (v8 PR-3, #387 §5): the same snapshot, page slice and
    /// version verification as v1; only the projection differs.
    pub fn query_v2(&self, args: Query, access: &Access) -> Result<serde_json::Value, Error> {
        let (allowed, default) = super::columnar::query_fields(args.dataset);
        let fields = super::columnar::select(
            args.fields.as_deref(),
            allowed,
            default,
            args.dataset.name(),
        )?;
        let (page, plan) = self.query_page(args, access)?;
        let meta = super::columnar::PageMeta {
            snapshot_id: &page.snapshot_id,
            snapshot_time: page.query_time,
            next_cursor: page.next_cursor.as_deref(),
            page_complete: page.page_complete,
            limitations: &page.limitations,
        };
        let mut page = match plan.args.dataset {
            Dataset::Catalog => super::columnar::catalog_table(
                &page.version_manifest,
                &page.records,
                &fields,
                &meta,
            ),
            Dataset::AdjustmentEvents | Dataset::Calendar => super::columnar::row_table(
                plan.args.dataset,
                &page.version_manifest,
                &page.records,
                &fields,
                &meta,
            ),
            Dataset::Daily | Dataset::Index
                if matches!(&plan.args.filter, Filter::CrossSection { .. }) =>
            {
                super::columnar::cross_section(
                    plan.args.dataset,
                    &page.version_manifest,
                    &plan.trade_days,
                    &plan.entities,
                    &page.records,
                    &fields,
                    &meta,
                )
            }
            dataset => super::columnar::panel(
                dataset,
                &page.version_manifest,
                &plan.entities,
                &page.records,
                &fields,
                &meta,
            ),
        };
        if let Some(catalog_version) = &plan.catalog_version
            && let Some(object) = page.as_object_mut()
        {
            object.insert("catalog_version".into(), serde_json::json!(catalog_version));
        }
        Ok(page)
    }
    fn query_page(&self, args: Query, access: &Access) -> Result<(DataPage, QueryPlan), Error> {
        let mut plan = QueryPlan::new(args, access)?;
        plan.resolve(&self.repository, access)?;
        let cursor = plan.args.cursor.clone();
        let mut keyargs = plan.args.clone();
        keyargs.cursor = None;
        let key = format!("query:{}", serde_json::to_string(&keyargs).unwrap());
        let mut saved = if let Some(cursor) = cursor {
            self.resume(&cursor, &key, access)?
        } else {
            let data = crate::stage_trace::timed("query.repository", || {
                self.repository.query(&plan, access)
            })?;
            let bytes = crate::stage_trace::timed("query.budget", || {
                serde_json::to_vec(&data).unwrap().len()
            });
            if data.rows.len() > MAX_ROWS || bytes > MAX_BYTES {
                return Err(invalid("snapshot budget exceeded"));
            }
            Saved {
                token: String::new(),
                key,
                access: access.clone(),
                time: Utc::now(),
                id: self.id(),
                offset: 0,
                content: Content::Data(Rc::new(data)),
                bytes,
            }
        };
        let Content::Data(data) = &saved.content else {
            return Err(invalid("cursor category mismatch"));
        };
        let refs = data
            .manifest
            .iter()
            .filter_map(|m| m.version_ref.clone())
            .collect::<Vec<_>>();
        crate::stage_trace::timed("query.verify", || self.repository.verify(&refs))?;
        let end = (saved.offset + plan.args.page_size).min(data.rows.len());
        let records = data.rows[saved.offset..end].to_vec();
        let manifest = data.manifest.clone();
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
            limitations: vec![
                "fixed version selection is not a shared source acquisition time".into(),
                "declared scope: owner personal research, local-only (#71 comment-5749934485); coverage is not inferred from dates".into(),
            ],
        };
        if more {
            result.next_cursor = Some(self.save(saved));
        }
        Ok((result, plan))
    }
    pub fn versions(&self, args: VersionsQuery, access: &Access) -> Result<VersionPage, Error> {
        Ok(self.versions_page(args, access)?.0)
    }
    /// v2 version table (v8 PR-3, #387 §5.1): one row per version, the list
    /// appears once and `integrity_sha256` is gone.
    pub fn versions_v2(
        &self,
        args: VersionsQuery,
        access: &Access,
    ) -> Result<serde_json::Value, Error> {
        let (page, args) = self.versions_page(args, access)?;
        let meta = super::columnar::PageMeta {
            snapshot_id: &page.snapshot_id,
            snapshot_time: page.snapshot_time,
            next_cursor: page.next_cursor.as_deref(),
            page_complete: page.page_complete,
            limitations: &[],
        };
        let mut out = super::columnar::versions_table(args.dataset, &page.items, &meta);
        if let Selection::Universe {
            catalog_version: Some(catalog_version),
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
        if args.contract_version == ReadContractVersion::V1 && args.dataset.is_v2_only() {
            return Err(invalid(&format!(
                "{} requires contract_version v2",
                args.dataset.name()
            )));
        }
        if let Selection::Universe { catalog_version } = &mut args.selection {
            let (resolved, members) = self
                .repository
                .universe_members(args.dataset, catalog_version.as_deref())?;
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
                content: Content::Versions(Rc::new(data)),
                bytes,
            }
        };
        let Content::Versions(data) = &saved.content else {
            return Err(invalid("cursor category mismatch"));
        };
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
