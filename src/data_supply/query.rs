//! Query selection and pagination decisions, independent of SQLite and MCP.
use super::{Access, wire::*};
use crate::market_storage::Error;
use chrono::{NaiveDate, Utc};
use std::{
    cell::RefCell,
    collections::{BTreeSet, VecDeque},
};
pub const MAX_ROWS: usize = 100_000;
pub const MAX_BYTES: usize = 16 * 1024 * 1024;
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
                && *start_date >= NaiveDate::from_ymd_opt(1, 1, 1).unwrap() => {}
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
#[derive(Clone)]
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
}
#[derive(Clone)]
enum Content {
    Data(QuerySnapshot),
    Versions(Vec<VersionInfo>),
}
pub struct Queries<R> {
    pub repository: R,
    cache: RefCell<VecDeque<Saved>>,
    instance: String,
    sequence: RefCell<u64>,
}
impl<R: ReadRepository> Queries<R> {
    pub fn new(repository: R) -> Self {
        Self {
            repository,
            cache: RefCell::new(VecDeque::new()),
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
    pub fn query(&self, args: Query, access: &Access) -> Result<DataPage, Error> {
        let plan = QueryPlan::new(args, access)?;
        let cursor = plan.args.cursor.clone();
        let mut keyargs = plan.args.clone();
        keyargs.cursor = None;
        let key = format!("query:{}", serde_json::to_string(&keyargs).unwrap());
        let mut saved = if let Some(cursor) = cursor {
            self.resume(&cursor, &key, access)?
        } else {
            let data = self.repository.query(&plan, access)?;
            if data.rows.len() > MAX_ROWS
                || serde_json::to_vec(&data.rows).unwrap().len() > MAX_BYTES
            {
                return Err(invalid("snapshot budget exceeded"));
            }
            Saved {
                token: String::new(),
                key,
                access: access.clone(),
                time: Utc::now(),
                id: self.id(),
                offset: 0,
                content: Content::Data(data),
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
        self.repository.verify(&refs)?;
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
                "source production gates remain blocked; coverage is not inferred from dates"
                    .into(),
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
            if data.len() > MAX_ROWS || serde_json::to_vec(&data).unwrap().len() > MAX_BYTES {
                return Err(invalid("snapshot budget exceeded"));
            }
            Saved {
                token: String::new(),
                key,
                access: access.clone(),
                time: Utc::now(),
                id: self.id(),
                offset: 0,
                content: Content::Versions(data),
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
/// Deduplicates verification work without changing query selection semantics.
pub fn unique_refs(refs: &[VersionRef]) -> Vec<VersionRef> {
    let mut seen = BTreeSet::new();
    refs.iter()
        .filter(|r| seen.insert((r.dataset, r.version_id.clone())))
        .cloned()
        .collect()
}
