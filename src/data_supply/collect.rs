//! Dataset command normalization and frozen item decisions. No MCP business rules.
use super::wire::*;
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
            Self::Daily { request_id, .. }
            | Self::Catalog { request_id, .. }
            | Self::Yearly { request_id, .. } => request_id,
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
                let v =
                    crate::sqlite_catalog::SqliteCatalog::new(self.repository.market.tasks.clone())
                        .fixed(version_id)
                        .map_err(invalid)?;
                let digest = crate::sqlite_catalog::digest(&v).map_err(invalid)?;
                let securities=v.records.iter().filter(|r|industry_code.as_ref().is_none_or(|code|matches!(&r.industry,crate::catalog::IndustryInfo::Assigned{code:c,..} if c==code))).map(|r|Security{exchange:match r.exchange{crate::catalog::Exchange::Shanghai=>Exchange::Sh,_=>Exchange::Sz},code:r.symbol.clone()}).collect();
                Ok((
                    securities,
                    json!({"catalog_version_id":version_id,"catalog_digest":digest}),
                ))
            }
            WriteSelection::Default => Err(invalid(
                "default selection is only implemented for market.daily update/backfill",
            )),
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
                    WriteSelection::Default => {
                        return Err(invalid(
                            "default selection is only implemented for market.daily update/backfill",
                        ));
                    }
                }
            }
            Collection::Catalog { .. } => {}
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
                let base = self.repository.market.fixed(id).map_err(invalid)?;
                if base.version().symbol != e.symbol()
                    || base.version().period != crate::market_data::Period::Day
                {
                    return Err(invalid("base identity/period mismatch"));
                }
                frozen["base_daily_version_id"] = json!(id);
                frozen["base_digest"] = base.raw()["integrity_sha256"].clone();
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
        Ok(FrozenWork { items, scopes })
    }
}
