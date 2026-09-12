//! Minimal catalog fixture adapter. No source acquisition and no stdio write handler.
use crate::{catalog::*, hash::sha256, market_storage::Error, sqlite_tasks::SqliteTasks};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::Value;
#[derive(Clone)]
pub struct SqliteCatalog {
    pub tasks: SqliteTasks,
}
fn storage(e: impl std::fmt::Display) -> Error {
    Error::Storage(e.to_string())
}
fn integrity(e: impl std::fmt::Display) -> Error {
    Error::Integrity(e.to_string())
}
fn raw(v: &CatalogVersion) -> Result<String, Error> {
    serde_json::to_string(v).map_err(integrity)
}
pub(crate) fn digest(v: &CatalogVersion) -> Result<String, Error> {
    Ok(sha256(raw(v)?.as_bytes()))
}
fn projection(v: &CatalogVersion) -> Vec<Vec<Option<String>>> {
    let mut rows: Vec<_> = v
        .records
        .iter()
        .map(|r| {
            let exchange = match r.exchange {
                Exchange::Shanghai => "sh",
                Exchange::Shenzhen => "sz",
                Exchange::Other(_) => "invalid",
            };
            let (status, code, name, reason) = match &r.industry {
                IndustryInfo::Assigned { code, name } => {
                    ("assigned", Some(code.clone()), Some(name.clone()), None)
                }
                IndustryInfo::Missing { reason } => ("missing", None, None, Some(reason.clone())),
                IndustryInfo::Conflicted { reason } => {
                    ("conflicted", None, None, Some(reason.clone()))
                }
            };
            vec![
                Some(exchange.into()),
                Some(r.symbol.clone()),
                Some(r.name.clone()),
                Some(r.source_security_id.clone()),
                Some(status.into()),
                code,
                name,
                reason,
            ]
        })
        .collect();
    rows.sort();
    rows
}
pub(crate) fn head(c: &Connection) -> Result<Option<String>, Error> {
    c.query_row(
        "SELECT version_id FROM catalog_head WHERE singleton=1",
        [],
        |r| r.get(0),
    )
    .optional()
    .map_err(storage)
}
pub(crate) fn read(c: &Connection, id: &str) -> Result<CatalogVersion, Error> {
    let row:Option<(String,String,String,String,String)> = c.query_row("SELECT compat_json,digest,published_at,source,acquired_at FROM catalog_versions WHERE version_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(storage)?;
    let (raw, digest, published, source, acquired) =
        row.ok_or_else(|| integrity("catalog version missing"))?;
    if sha256(raw.as_bytes()) != digest {
        return Err(integrity("catalog digest mismatch"));
    }
    let v: CatalogVersion = serde_json::from_str(&raw).map_err(integrity)?;
    if v.version_id != id
        || v.published_at.to_rfc3339() != published
        || v.source != source
        || v.acquired_at.to_rfc3339() != acquired
    {
        return Err(integrity("catalog metadata projection"));
    }
    let snapshot = CatalogSnapshot {
        source: v.source.clone(),
        acquired_at: v.acquired_at,
        completeness: CompletenessEvidence::Complete {
            expected_count: v.records.len(),
        },
        records: v.records.clone(),
    };
    validate_snapshot(&snapshot, None).map_err(|e| integrity(format!("{e:?}")))?;
    let mut q=c.prepare("SELECT exchange,code,name,source_security_id,industry_status,industry_code,industry_name,industry_reason FROM catalog_members WHERE version_id=?1 ORDER BY exchange,code").map_err(storage)?;
    let rows = q
        .query_map([id], |r| {
            (0..8)
                .map(|n| r.get::<_, Option<String>>(n))
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage)?;
    if rows != projection(&v) {
        return Err(integrity("catalog member projection"));
    }
    Ok(v)
}
impl SqliteCatalog {
    pub fn new(tasks: SqliteTasks) -> Self {
        Self { tasks }
    }
    pub fn fixed(&self, id: &str) -> Result<CatalogVersion, Error> {
        let mut c = self.tasks.connect(true)?;
        let tx = c.transaction().map_err(storage)?;
        crate::sqlite_tasks::check_schema(&tx, true)?;
        let result = read(&tx, id)?;
        tx.commit().map_err(storage)?;
        Ok(result)
    }
    /// Explicit fixture import, not a production publisher. Applies the existing catalog rules.
    pub fn publish_fixture(
        &self,
        id: &str,
        snapshot: CatalogSnapshot,
        expected: Option<&str>,
    ) -> Result<(), Error> {
        if id.trim().is_empty()
            || !snapshot.source.starts_with("fixture:")
            || snapshot.records.len() > 100_000
        {
            return Err(Error::Invalid("bounded, labelled fixture required".into()));
        }
        let mut c = self.tasks.connect(false)?;
        let tx = c
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        crate::sqlite_tasks::check_schema(&tx, true)?;
        let current = head(&tx)?;
        if current.as_deref() != expected {
            return Err(Error::Conflict("catalog head changed".into()));
        }
        let previous = current.as_deref().map(|id| read(&tx, id)).transpose()?;
        validate_snapshot(&snapshot, previous.as_ref())
            .map_err(|e| Error::Invalid(format!("{e:?}")))?;
        let v = CatalogVersion {
            version_id: id.into(),
            published_at: snapshot.acquired_at,
            source: snapshot.source,
            acquired_at: snapshot.acquired_at,
            records: snapshot.records,
        };
        let body = raw(&v)?;
        tx.execute(
            "INSERT INTO catalog_versions VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                id,
                v.published_at.to_rfc3339(),
                v.source,
                v.acquired_at.to_rfc3339(),
                body,
                digest(&v)?
            ],
        )
        .map_err(storage)?;
        for r in projection(&v) {
            tx.execute(
                "INSERT INTO catalog_members VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![id, r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7]],
            )
            .map_err(storage)?;
        }
        tx.execute("INSERT INTO catalog_head VALUES(1,?1) ON CONFLICT(singleton) DO UPDATE SET version_id=excluded.version_id",[id]).map_err(storage)?;
        read(&tx, id)?;
        tx.commit().map_err(storage)?;
        Ok(())
    }
}
/// Stable fixture digest is deliberately separate from the market legacy digest format.
pub fn fixture_value(v: &CatalogVersion) -> Result<Value, Error> {
    serde_json::to_value(v).map_err(integrity)
}
