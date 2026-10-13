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
}
impl QueryPlan {
    pub fn new(mut args: Query, access: &Access) -> Result<Self, Error> {
        access.require(args.dataset, None)?;
        page_size(args.page_size)?;
        let entities = entities(args.dataset, &args.selection, access)?;
        match (&args.filter, args.dataset) {
            (
                Filter::Daily {
                    start_date,
                    end_date,
                },
                Dataset::Daily,
            ) if start_date <= end_date
                && *start_date >= NaiveDate::from_ymd_opt(1, 1, 1).unwrap()
                && *end_date <= NaiveDate::from_ymd_opt(9999, 12, 31).unwrap() => {}
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
            VersionChoice::LatestPerEntity => {}
            VersionChoice::CatalogVersion { version_id }
                if args.dataset == Dataset::Catalog && !version_id.is_empty() => {}
            VersionChoice::ExplicitVersionMap { versions } if args.dataset != Dataset::Catalog => {
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
        Ok(Self { args, entities })
    }
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
        (Dataset::Daily | Dataset::Yearly, Selection::Single { entity }) => vec![entity.clone()],
        (Dataset::Daily | Dataset::Yearly, Selection::Batch { entities }) => entities.clone(),
        _ => return Err(invalid("invalid selection for dataset")),
    };
    if dataset != Dataset::Catalog && (e.is_empty() || e.len() > 100) {
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
            return Err(invalid("cursor conditions/authorization changed"));
        }
        Ok(s)
    }
    pub fn query(&self, args: Query, access: &Access) -> Result<DataPage, Error> {
        let plan = QueryPlan::new(args, access)?;
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
        Ok(result)
    }
    pub fn versions(&self, mut args: VersionsQuery, access: &Access) -> Result<VersionPage, Error> {
        access.require(args.dataset, None)?;
        page_size(args.page_size)?;
        entities(args.dataset, &args.selection, access)?;
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
        Ok(result)
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
/// Deduplicates verification work without changing query selection semantics.
pub fn unique_refs(refs: &[VersionRef]) -> Vec<VersionRef> {
    let mut seen = BTreeSet::new();
    refs.iter()
        .filter(|r| seen.insert((r.dataset, r.version_id.clone())))
        .cloned()
        .collect()
}
