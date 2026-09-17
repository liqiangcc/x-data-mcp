//! `securities.catalog` collection executor for inspected official-file
//! snapshots (design: docs/tasks/issue-6-catalog-impl.md §2/S1–S7).
//!
//! The executor consumes a `SnapshotManifest` that already passes
//! `snapshot::inspect` byte verification, extracts the three anchored
//! members (base.zip!{tdxhy.cfg, infoharbor_ex.code},
//! gbbq.zip!zhb.zip!incon.dat), parses, applies the frozen scope rule,
//! composes the candidate, and publishes through
//! `SqliteCatalog::publish_official`. Every failure — missing members,
//! malformed lines, rejected candidates — maps to an item error while the
//! previous catalog stays published; nothing is skipped, repaired or
//! guessed. Nameless scope members are governed `reject_candidate`
//! exclusions (P1 ruling, #6 comment 5710586446): they are enumerated
//! verbatim in the item outcome evidence and never enter the candidate.
//!
//! The executor is only installed when `X_DATA_MCP_OFFICIAL_MANIFEST` and
//! `X_DATA_MCP_OFFICIAL_DIR` are both configured; otherwise the catalog
//! item retains the `SOURCE_NOT_VERIFIED` refusal.

use std::{fs::File, io::Read, path::PathBuf};

use chrono::{DateTime, Utc};

use super::{compose, ex_code, incon, scope, tdxhy};
use crate::{
    snapshot::{self, FileRole},
    sqlite_catalog::SqliteCatalog,
    sqlite_market::SqliteMarket,
    task_control::{Executor, ItemOutcome, ItemSpec},
};

/// Largest member byte count this executor will read. All anchored members
/// are far below this bound (gen-2 largest ≈ 1 MiB); the cap exists so a
/// corrupt archive cannot force an unbounded allocation.
const MEMBER_LIMIT: u64 = 64 << 20;
const MANIFEST_LIMIT: u64 = 1 << 20;

pub struct TdxCatalogExecutor {
    catalog: SqliteCatalog,
    manifest_path: PathBuf,
    input_root: PathBuf,
}

impl TdxCatalogExecutor {
    pub fn new(
        catalog: SqliteCatalog,
        manifest_path: impl Into<PathBuf>,
        input_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            catalog,
            manifest_path: manifest_path.into(),
            input_root: input_root.into(),
        }
    }

    /// Install only when both configuration variables are present; absent
    /// configuration leaves the catalog item on the blocked-source path.
    pub fn from_env(market: &SqliteMarket) -> Option<Self> {
        let manifest = std::env::var_os("X_DATA_MCP_OFFICIAL_MANIFEST")?;
        let root = std::env::var_os("X_DATA_MCP_OFFICIAL_DIR")?;
        Some(Self::new(
            SqliteCatalog::new(market.tasks.clone()),
            manifest,
            root,
        ))
    }

    fn member(zip: &mut zip::ZipArchive<File>, name: &str) -> Result<Vec<u8>, String> {
        let entry = zip
            .by_name(name)
            .map_err(|e| format!("CATALOG_SOURCE_FAILURE: member {name} unavailable: {e}"))?;
        if entry.size() > MEMBER_LIMIT {
            return Err(format!(
                "CATALOG_SOURCE_FAILURE: member {name} exceeds the bounded size limit"
            ));
        }
        let mut bytes = Vec::new();
        entry
            .take(MEMBER_LIMIT)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("CATALOG_SOURCE_FAILURE: member {name} unreadable: {e}"))?;
        Ok(bytes)
    }

    fn run(&self, item: &ItemSpec) -> Result<(String, serde_json::Value), String> {
        if item.target.get("scope").and_then(|v| v.as_str()) != Some("current_sh_sz_a") {
            return Err(format!(
                "CATALOG_REJECTED: unsupported catalog scope {}",
                item.target
            ));
        }
        let mut manifest_bytes = Vec::new();
        File::open(&self.manifest_path)
            .and_then(|f| f.take(MANIFEST_LIMIT + 1).read_to_end(&mut manifest_bytes))
            .map_err(|e| format!("CATALOG_SOURCE_FAILURE: manifest unreadable: {e}"))?;
        let inspection = snapshot::inspect(&manifest_bytes, &self.input_root, Utc::now())
            .map_err(|e| format!("CATALOG_SOURCE_FAILURE: snapshot inspection failed: {e}"))?;
        let artifact = |role: FileRole| -> Result<PathBuf, String> {
            inspection
                .files
                .iter()
                .find(|f| f.role == role)
                .map(|f| self.input_root.join(&f.relative_path))
                .ok_or_else(|| format!("CATALOG_SOURCE_FAILURE: snapshot lacks role {role:?}"))
        };
        let acquired_at: DateTime<Utc> = inspection
            .files
            .iter()
            .map(|f| f.acquisition_completed_at)
            .max()
            .unwrap_or_else(Utc::now);

        let mut base_zip = zip::ZipArchive::new(
            File::open(artifact(FileRole::BaseIndustry)?)
                .map_err(|e| format!("CATALOG_SOURCE_FAILURE: base.zip unreadable: {e}"))?,
        )
        .map_err(|e| format!("CATALOG_SOURCE_FAILURE: base.zip invalid: {e}"))?;
        let tdxhy_rows = tdxhy::parse(&Self::member(&mut base_zip, "tdxhy.cfg")?)
            .map_err(|e| format!("CATALOG_SOURCE_FAILURE: {e}"))?;
        let ex_rows = ex_code::parse(&Self::member(&mut base_zip, "infoharbor_ex.code")?)
            .map_err(|e| format!("CATALOG_SOURCE_FAILURE: {e}"))?;

        let mut gbbq_zip = zip::ZipArchive::new(
            File::open(artifact(FileRole::Gbbq)?)
                .map_err(|e| format!("CATALOG_SOURCE_FAILURE: gbbq.zip unreadable: {e}"))?,
        )
        .map_err(|e| format!("CATALOG_SOURCE_FAILURE: gbbq.zip invalid: {e}"))?;
        let zhb_bytes = Self::member(&mut gbbq_zip, "zhb.zip")?;
        let mut zhb_zip = zip::ZipArchive::new(std::io::Cursor::new(zhb_bytes))
            .map_err(|e| format!("CATALOG_SOURCE_FAILURE: zhb.zip invalid: {e}"))?;
        let mut incon_bytes = Vec::new();
        {
            let entry = zhb_zip.by_name("incon.dat").map_err(|e| {
                format!("CATALOG_SOURCE_FAILURE: member incon.dat unavailable: {e}")
            })?;
            if entry.size() > MEMBER_LIMIT {
                return Err(
                    "CATALOG_SOURCE_FAILURE: member incon.dat exceeds the bounded size limit"
                        .into(),
                );
            }
            entry
                .take(MEMBER_LIMIT)
                .read_to_end(&mut incon_bytes)
                .map_err(|e| format!("CATALOG_SOURCE_FAILURE: member incon.dat unreadable: {e}"))?;
        }
        let sections =
            incon::parse(&incon_bytes).map_err(|e| format!("CATALOG_SOURCE_FAILURE: {e}"))?;

        let scoped = scope::apply(&tdxhy_rows);
        let counts = scoped.reconciliation(tdxhy_rows.len());
        if counts.total != counts.scope + counts.bj_market + counts.b_share + counts.anomalous {
            return Err("CATALOG_SOURCE_FAILURE: scope partition identity violated".into());
        }

        let source = format!("official-file:{:.16}", inspection.manifest_sha256);
        let outcome = compose::compose(
            &scoped.members,
            &ex_code::name_map(&ex_rows),
            &sections,
            &source,
            acquired_at,
        )
        .map_err(|e| format!("CATALOG_SOURCE_FAILURE: {e}"))?;
        let Some(snapshot) = outcome.snapshot else {
            let names = outcome
                .nameless
                .iter()
                .map(|(market, code)| format!("{market}:{code}"))
                .collect::<Vec<_>>()
                .join(",");
            return Err(format!(
                "CATALOG_REJECTED: no publishable members; nameless scope members: {names}"
            ));
        };
        let published_count = snapshot.records.len();

        let expected = self
            .catalog
            .selection_facts()
            .map_err(|e| format!("CATALOG_SOURCE_FAILURE: catalog head unreadable: {e}"))?
            .catalog_head;
        let version_id = self
            .catalog
            .publish_official(snapshot, expected.as_deref(), Utc::now())
            .map_err(|e| format!("CATALOG_REJECTED: {e}"))?;
        let evidence = serde_json::json!({
            "catalog_version_id": version_id,
            "published_members": published_count,
            "reject_candidate_exclusions": outcome
                .nameless
                .iter()
                .map(|(market, code)| serde_json::json!({
                    "member": format!("{market}:{code}"),
                    "reason": "reject_candidate: no name row in infoharbor_ex.code",
                }))
                .collect::<Vec<_>>(),
            "missing_industry": outcome
                .missing_industry
                .iter()
                .map(|(market, code)| format!("{market}:{code}"))
                .collect::<Vec<_>>(),
            "t_code_residuals": outcome
                .t_code_residuals
                .iter()
                .map(|r| serde_json::json!({
                    "member": format!("{}:{}", r.member.0, r.member.1),
                    "t_code": r.t_code,
                }))
                .collect::<Vec<_>>(),
            "t_dictionary_checked": outcome.t_dictionary_checked,
            "scope_partition": {
                "total": counts.total,
                "scope": counts.scope,
                "bj_market": counts.bj_market,
                "b_share": counts.b_share,
                "anomalous": counts.anomalous,
            },
        });
        Ok((version_id, evidence))
    }
}

impl Executor for TdxCatalogExecutor {
    fn execute(&self, _key: &crate::task_control::CommandKey, item: &ItemSpec) -> ItemOutcome {
        match self.run(item) {
            Ok((_version_id, evidence)) => ItemOutcome {
                success: true,
                error: None,
                evidence: Some(evidence),
            },
            Err(error) => ItemOutcome {
                success: false,
                error: Some(error),
                evidence: None,
            },
        }
    }
}
