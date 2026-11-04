//! Dataset command normalization and frozen item decisions. No MCP business rules.
use super::wire::*;
use crate::market_storage::MarketRepository;
use crate::{
    sqlite_supply::SqliteSupply,
    task_control::{self as task, CommandKey, Contract, FrozenWork, ItemSpec},
};
use serde_json::{Value, json};
use std::collections::BTreeSet;
fn invalid(s: impl ToString) -> task::Error {
    task::Error::InvalidCommand(s.to_string())
}
impl Collection {
    pub fn dataset(&self) -> Dataset {
        match self {
            Self::Daily { .. } => Dataset::Daily,
            Self::Catalog { .. } => Dataset::Catalog,
            Self::Yearly { .. } => Dataset::Yearly,
        }
    }
    pub fn request_id(&self) -> &str {
        match self {
            Collection::Daily { request_id, .. }
            | Collection::Catalog { request_id, .. }
            | Collection::Yearly { request_id, .. } => request_id,
        }
    }
    pub fn contract_version(&self) -> CollectionContractVersion {
        match self {
            Collection::Daily {
                contract_version, ..
            }
            | Collection::Catalog {
                contract_version, ..
            }
            | Collection::Yearly {
                contract_version, ..
            } => *contract_version,
        }
    }
    /// The persisted command key. `v1` stays byte-identical; `v2` exists only
    /// for the `market.daily` scope selection and is rejected before lookup
    /// for every other dataset.
    pub fn key(&self) -> CommandKey {
        CommandKey {
            operation: "start_collection".into(),
            dataset: serde_json::to_value(self.dataset())
                .unwrap()
                .as_str()
                .unwrap()
                .into(),
            contract_version: self.contract_version().as_str().into(),
        }
    }
    pub fn payload(&self) -> Value {
        let mut v = serde_json::to_value(self).unwrap();
        v.as_object_mut().unwrap().remove("request_id");
        v
    }
}
pub struct CollectionContract {
    pub dataset: Dataset,
    pub repository: SqliteSupply,
}
impl CollectionContract {
    fn decode(&self, mut v: Value) -> Result<Collection, task::Error> {
        v.as_object_mut()
            .ok_or_else(|| invalid("expected collection object"))?
            .insert("request_id".into(), json!(""));
        let c: Collection = serde_json::from_value(v).map_err(invalid)?;
        if c.dataset() != self.dataset {
            return Err(invalid("dataset contract mismatch"));
        }
        Ok(c)
    }
    fn selection(&self, s: &WriteSelection) -> Result<(Vec<Security>, Value), task::Error> {
        match s {
            WriteSelection::Explicit { securities } => Ok((securities.clone(), json!({}))),
            WriteSelection::Catalog {
                version_id,
                industry_code,
            } => {
                let v = crate::sqlite_catalog::SqliteCatalog::new(self.repository.market.tasks())
                    .fixed(version_id)
                    .map_err(invalid)?;
                let digest = crate::sqlite_catalog::digest(&v).map_err(invalid)?;
                let securities=v.records.iter().filter(|r|industry_code.as_ref().is_none_or(|code|matches!(&r.industry,crate::catalog::IndustryInfo::Assigned{code:c,..} if c==code))).map(|r|Security{exchange:match r.exchange{crate::catalog::Exchange::Shanghai=>Exchange::Sh,_=>Exchange::Sz},code:r.symbol.clone()}).collect();
                Ok((
                    securities,
                    json!({"catalog_version_id":version_id,"catalog_digest":digest}),
                ))
            }
            WriteSelection::Default { .. } => Err(invalid(
                "default selection is only implemented for market.daily update/backfill",
            )),
            WriteSelection::Scope { .. } => {
                Err(invalid("selection.kind=scope requires contract_version v2"))
            }
        }
    }
}
impl Contract for CollectionContract {
    fn key(&self) -> CommandKey {
        CommandKey {
            operation: "start_collection".into(),
            dataset: serde_json::to_value(self.dataset)
                .unwrap()
                .as_str()
                .unwrap()
                .into(),
            contract_version: "v1".into(),
        }
    }
    fn normalize(&self, v: Value) -> Result<Value, task::Error> {
        let mut c = self.decode(v)?;
        match &mut c {
            Collection::Daily { selection, .. } | Collection::Yearly { selection, .. } => {
                match selection {
                    WriteSelection::Explicit { securities } => {
                        for e in securities.iter() {
                            e.validate().map_err(invalid)?;
                        }
                        securities.sort();
                        securities.dedup();
                        if securities.is_empty() || securities.len() > 100 {
                            return Err(invalid("write selection budget 1..100"));
                        }
                    }
                    WriteSelection::Catalog { version_id, .. } => {
                        if version_id.is_empty() {
                            return Err(invalid("catalog version required"));
                        }
                    }
                    WriteSelection::Default { .. } => {
                        return Err(invalid(
                            "default selection is only implemented for market.daily update/backfill",
                        ));
                    }
                    WriteSelection::Scope { .. } => {
                        return Err(invalid("selection.kind=scope requires contract_version v2"));
                    }
                }
            }
            Collection::Catalog { selection, .. } => {
                // The listed-ever scope resolves from the security master, not
                // from a catalog snapshot: fail closed instead of degrading to
                // the current-member subset.
                if selection.scope != CatalogScope::CurrentShSzA {
                    return Err(invalid(
                        "catalog collection only accepts scope current_sh_sz_a",
                    ));
                }
            }
        }
        if let Collection::Yearly {
            years,
            base_daily_versions,
            ..
        } = &mut c
        {
            years.sort();
            years.dedup();
            if years.is_empty()
                || years.len() > 9999
                || years.iter().any(|y| !(1..=9999).contains(y))
            {
                return Err(invalid("explicit natural years required"));
            }
            base_daily_versions.sort_by(|a, b| {
                a.entity
                    .cmp(&b.entity)
                    .then(a.version_id.cmp(&b.version_id))
            });
            base_daily_versions.dedup();
            if base_daily_versions
                .windows(2)
                .any(|w| w[0].entity == w[1].entity)
            {
                return Err(invalid("conflicting base versions for same entity"));
            }
        }
        Ok(c.payload())
    }
    fn resolve(&self, v: &Value) -> Result<FrozenWork, task::Error> {
        let c = self.decode(v.clone())?;
        if matches!(c, Collection::Catalog { .. }) {
            let scope = "catalog:current_sh_sz_a".to_string();
            return Ok(FrozenWork {
                scopes: BTreeSet::from([scope.clone()]),
                items: vec![ItemSpec {
                    item_id: scope.clone(),
                    target: json!({"scope":"current_sh_sz_a"}),
                    progress_unit: "catalog_snapshot".into(),
                    frozen_inputs: json!({}),
                    scope_keys: BTreeSet::from([scope]),
                }],
                shared_evidence: None,
            });
        }
        let (selection, period) = match &c {
            Collection::Daily { selection, .. } => (selection, "day"),
            Collection::Yearly { selection, .. } => (selection, "year"),
            _ => unreachable!(),
        };
        let (mut entities, selection_evidence) = self.selection(selection)?;
        entities.sort();
        entities.dedup();
        if entities.is_empty() || entities.len() > 100 {
            return Err(invalid("resolved selection budget 1..100"));
        }
        if let Collection::Yearly {
            base_daily_versions,
            ..
        } = &c
            && base_daily_versions
                .iter()
                .map(|v| v.entity.clone())
                .collect::<Vec<_>>()
                != entities
        {
            return Err(invalid("base daily version map must match selection"));
        }
        let mut items = vec![];
        let mut scopes = BTreeSet::new();
        for e in entities {
            let scope = task::legacy::scope_id(&e.symbol(), period);
            scopes.insert(scope.clone());
            let mut frozen = selection_evidence.clone();
            if let Collection::Yearly {
                base_daily_versions,
                years,
                ..
            } = &c
            {
                let id = &base_daily_versions
                    .iter()
                    .find(|v| v.entity == e)
                    .unwrap()
                    .version_id;
                let base = self.repository.market.fixed_facts(id).map_err(invalid)?;
                if base.symbol != e.symbol() || base.period != crate::market_data::Period::Day {
                    return Err(invalid("base identity/period mismatch"));
                }
                frozen["base_daily_version_id"] = json!(id);
                frozen["base_digest"] = json!(base.integrity_sha256);
                frozen["years"] = json!(years);
            }
            items.push(ItemSpec {
                item_id: scope.clone(),
                target: json!({"symbol":e.symbol(),"period":period}),
                progress_unit: "security_period".into(),
                frozen_inputs: frozen,
                scope_keys: BTreeSet::from([scope]),
            });
        }
        Ok(FrozenWork {
            items,
            scopes,
            shared_evidence: None,
        })
    }
}

/// Budget of one server-resolved listed-ever collection (`#446` §4.4, D-62
/// Q3). The scope resolves the whole delisted tail; accepting it unbounded
/// would make acceptance and progress unpredictable, so 0 or 501+ fails
/// closed instead of silently truncating.
const LISTED_EVER_BUDGET: usize = 500;
/// One `.day` record is 32 bytes (`src/day_file.rs`); a shorter member holds
/// no bar and is skipped into the task evidence instead of failing the call.
const DAY_RECORD_BYTES: u64 = 32;

/// `v2` `start_collection` for `market.daily`: the member list comes from the
/// security master ∩ the active generation's `.day` members. The caller picks
/// a scope, never entities.
pub struct ListedEverCollectionContract {
    pub repository: SqliteSupply,
    pub generation: Option<super::generation::GenerationSource>,
}

impl ListedEverCollectionContract {
    fn decode(&self, mut v: Value) -> Result<Collection, task::Error> {
        v.as_object_mut()
            .ok_or_else(|| invalid("expected collection object"))?
            .insert("request_id".into(), json!(""));
        let c: Collection = serde_json::from_value(v).map_err(invalid)?;
        match &c {
            Collection::Daily {
                contract_version,
                selection,
                ..
            } => {
                if *contract_version != CollectionContractVersion::V2 {
                    return Err(invalid("contract_version v2 required"));
                }
                match selection {
                    WriteSelection::Scope {
                        scope:
                            scope @ (CatalogScope::ShSzAListedEver
                            | CatalogScope::ShSzEtfCurrent
                            | CatalogScope::ShSzEtfListedEver),
                    } => {
                        let _ = scope;
                    }
                    WriteSelection::Scope { .. } => {
                        return Err(invalid(
                            "v2 scope collection accepts sh_sz_a_listed_ever, \
                             sh_sz_etf_current or sh_sz_etf_listed_ever only",
                        ));
                    }
                    _ => {
                        return Err(invalid("contract_version v2 requires selection.kind=scope"));
                    }
                }
            }
            _ => {
                return Err(invalid(
                    "contract_version v2 is implemented only for market.daily",
                ));
            }
        }
        Ok(c)
    }

    /// Master entities (`sh|sz` ordinary A shares) without a published raw
    /// daily series. Intervals are collapsed: a code reused by two issuers is
    /// still one series identity.
    fn candidates(
        rows: &[crate::security_master::SecurityLifecycleFact],
        has_series: &BTreeSet<String>,
        scope: CatalogScope,
    ) -> BTreeSet<String> {
        rows.iter()
            .filter(|row| {
                let class_matches = match scope {
                    CatalogScope::ShSzAListedEver => row.instrument_class == 0,
                    CatalogScope::ShSzEtfCurrent => {
                        i64::from(row.instrument_class) == crate::market_data::INSTRUMENT_CLASS_ETF
                            && row.effective_to.is_none()
                            && row.delisting_source
                                == crate::security_master::DelistingSource::Unknown
                    }
                    CatalogScope::ShSzEtfListedEver => {
                        i64::from(row.instrument_class) == crate::market_data::INSTRUMENT_CLASS_ETF
                    }
                    CatalogScope::CurrentShSzA => false,
                };
                class_matches
                    && matches!(
                        row.exchange,
                        crate::security_master::Exchange::Sh | crate::security_master::Exchange::Sz
                    )
                    && !has_series.contains(&row.entity())
            })
            .map(|row| row.entity())
            .collect()
    }

    fn budget(count: usize) -> Result<(), task::Error> {
        if count == 0 {
            return Err(invalid(
                "resolved listed-ever selection is empty; nothing to collect",
            ));
        }
        if count > LISTED_EVER_BUDGET {
            return Err(invalid(format!(
                "resolved listed-ever selection budget 1..{LISTED_EVER_BUDGET}, got {count}"
            )));
        }
        Ok(())
    }

    /// The frozen resolution: master rows, series presence, generation member
    /// set and member sizes. Split from `resolve` so the pure filters and the
    /// budget can be unit-tested without a source.
    fn resolve_scope(&self, scope: CatalogScope) -> Result<ListedEverResolution, task::Error> {
        let (version, rows, has_series) = self
            .repository
            .market
            .read(|c| {
                let version = crate::sqlite_market::derived::read_kind_version(
                    c,
                    crate::security_master::assembly::DATASET_KIND,
                )?;
                let rows = crate::sqlite_market::security::read_security_master(c)?;
                let spans = if scope == CatalogScope::ShSzAListedEver {
                    crate::sqlite_market::series::raw_daily_spans(c)?
                } else {
                    crate::sqlite_market::series::etf_raw_daily_spans(c)?
                };
                Ok((
                    version,
                    rows,
                    spans.into_keys().collect::<BTreeSet<String>>(),
                ))
            })
            .map_err(|e| task::Error::StorageUnavailable(e.to_string()))?;
        let Some(version) = version else {
            return Err(invalid(
                "security_master has no published version; the listed-ever scope cannot resolve",
            ));
        };
        let source = self
            .generation
            .as_ref()
            .ok_or_else(|| invalid("selection.kind=scope requires a verified generation store"))?;
        let (generation_id, source) = source
            .active_source()
            .ok_or_else(|| invalid("selection.kind=scope requires a verified active generation"))?;
        let members = source
            .day_member_symbols()
            .map_err(|e| invalid(format!("listed-ever member listing failed: {e}")))?;
        let candidates = Self::candidates(&rows, &has_series, scope);
        let mut selected = Vec::new();
        let mut skipped_no_bars = Vec::new();
        let mut absent = Vec::new();
        for entity in &candidates {
            if !members.contains(entity) {
                // ETF scopes must report the list-has/day-has-not gap instead
                // of silently dropping it (M3-2 §4.2, AC-M3-2-5). A-share
                // listed-ever keeps its historical skip behavior.
                if scope != CatalogScope::ShSzAListedEver {
                    absent.push(AbsentMember {
                        entity: entity.clone(),
                        reason: "source_member_absent",
                    });
                }
                continue;
            }
            let size = source
                .day_member_size(entity)
                .map_err(|e| invalid(format!("listed-ever member lookup failed: {e}")))?
                .ok_or_else(|| invalid("listed member disappeared during resolution"))?;
            if size < DAY_RECORD_BYTES {
                if scope == CatalogScope::ShSzAListedEver {
                    skipped_no_bars.push(entity.clone());
                } else {
                    absent.push(AbsentMember {
                        entity: entity.clone(),
                        reason: "source_member_empty",
                    });
                }
            } else {
                let (exchange, code) = entity
                    .split_once('.')
                    .ok_or_else(|| invalid("entity has no exchange separator"))?;
                selected.push(Security {
                    exchange: match exchange {
                        "sh" => Exchange::Sh,
                        "sz" => Exchange::Sz,
                        _ => return Err(invalid("listed-ever scope is limited to sh/sz")),
                    },
                    code: code.into(),
                });
            }
        }
        Self::budget(selected.len())?;
        Ok(ListedEverResolution {
            entities: selected,
            skipped_no_bars,
            absent,
            scope,
            security_master_version_id: version.version_id,
            security_master_content_digest: crate::research::read::hex(&version.content_digest),
            generation_id,
            generation_manifest_sha256: source.manifest_sha256().to_string(),
        })
    }
}

#[derive(Debug, Clone)]
struct AbsentMember {
    entity: String,
    reason: &'static str,
}

struct ListedEverResolution {
    entities: Vec<Security>,
    skipped_no_bars: Vec<String>,
    absent: Vec<AbsentMember>,
    scope: CatalogScope,
    security_master_version_id: String,
    security_master_content_digest: String,
    generation_id: String,
    generation_manifest_sha256: String,
}

impl Contract for ListedEverCollectionContract {
    fn key(&self) -> CommandKey {
        CommandKey {
            operation: "start_collection".into(),
            dataset: "market.daily".into(),
            contract_version: CollectionContractVersion::V2.as_str().into(),
        }
    }
    fn normalize(&self, v: Value) -> Result<Value, task::Error> {
        Ok(self.decode(v)?.payload())
    }
    fn resolve(&self, v: &Value) -> Result<FrozenWork, task::Error> {
        let c = self.decode(v.clone())?;
        let Collection::Daily {
            selection: WriteSelection::Scope { scope },
            ..
        } = &c
        else {
            unreachable!("decode accepts market.daily scope selections only")
        };
        let resolution = self.resolve_scope(*scope)?;
        let scope_name = match resolution.scope {
            CatalogScope::ShSzAListedEver => "sh_sz_a_listed_ever",
            CatalogScope::ShSzEtfCurrent => "sh_sz_etf_current",
            CatalogScope::ShSzEtfListedEver => "sh_sz_etf_listed_ever",
            CatalogScope::CurrentShSzA => {
                unreachable!("v2 scope collection rejects the catalog default")
            }
        };
        let absent: Vec<Value> = resolution
            .absent
            .iter()
            .map(|member| json!({"entity": member.entity, "reason": member.reason}))
            .collect();
        let mut evidence = json!({
            "scope": scope_name,
            "security_master_version_id": resolution.security_master_version_id,
            "security_master_content_digest": resolution.security_master_content_digest,
            "generation_id": resolution.generation_id,
            "generation_manifest_sha256": resolution.generation_manifest_sha256,
            "source_generation_id": resolution.generation_id,
            "skipped_no_bars": resolution.skipped_no_bars,
            "skipped_no_bars_count": resolution.skipped_no_bars.len(),
        });
        // A-share listed-ever keeps its exact historical evidence shape; the
        // absent report is an ETF-scope field (M3-2 AC-M3-2-5).
        if resolution.scope != CatalogScope::ShSzAListedEver {
            let map = evidence.as_object_mut().expect("evidence is an object");
            map.insert("absent".into(), json!(absent));
            map.insert("absent_count".into(), json!(resolution.absent.len()));
        }
        let mut items = Vec::with_capacity(resolution.entities.len());
        let mut scopes = BTreeSet::new();
        for entity in resolution.entities {
            let scope = task::legacy::scope_id(&entity.symbol(), "day");
            scopes.insert(scope.clone());
            items.push(ItemSpec {
                item_id: scope.clone(),
                target: json!({"symbol":entity.symbol(),"period":"day"}),
                progress_unit: "security_period".into(),
                frozen_inputs: evidence.clone(),
                scope_keys: BTreeSet::from([scope]),
            });
        }
        Ok(FrozenWork {
            items,
            scopes,
            shared_evidence: None,
        })
    }
}

/// The `start_collection` command keys this surface knows. `v1` covers the
/// three collection datasets; `v2` exists only for the `market.daily`
/// listed-ever scope (`#446` §4.4). Verification tooling uses this instead of
/// pattern-matching the version string in three places.
pub fn is_collection_key(key: &CommandKey) -> bool {
    key.operation == "start_collection"
        && matches!(
            (key.dataset.as_str(), key.contract_version.as_str()),
            ("market.daily", "v1" | "v2") | ("securities.catalog" | "market.yearly", "v1")
        )
}

#[cfg(test)]
mod etf_update_tests;
#[cfg(test)]
mod tests;
