//! Fabricated universe repository and a minimal `ToolHandler` over `Queries`,
//! shaped like the research entry's `get_market_data` path (v2 columnar read,
//! full-text `content` plus `structuredContent`). No real data, no database.
use chrono::{NaiveDate, Utc};
use serde_json::{Value, json};
use x_data_mcp::{
    data_supply::{
        Access,
        query::{Queries, QueryPlan, QuerySnapshot, ReadRepository},
        wire::*,
    },
    market_storage::Error,
    mcp::{ApiError, ToolHandler},
};

pub const DAY: &str = "2026-11-05";

#[derive(Clone)]
pub struct Fake {
    pub entities: usize,
}

pub fn security(n: usize) -> Security {
    Security {
        exchange: if n.is_multiple_of(2) {
            Exchange::Sh
        } else {
            Exchange::Sz
        },
        code: format!("{:06}", 600000 + n),
    }
}

fn version_ref(entity: &Security) -> VersionRef {
    let digest = format!("{:064x}", entity.code.parse::<u128>().unwrap() * 7919);
    VersionRef {
        dataset: Dataset::Daily,
        contract_version: ContractVersion::V1,
        entity: Some(entity.clone()),
        version_id: format!("daily-{digest}"),
        integrity_sha256: digest,
    }
}

fn evidence(field: &str) -> Evidence {
    Evidence {
        field: field.into(),
        status: "known".into(),
        value: Some("fabricated".into()),
        reason: "fabricated fixture evidence, no real source behind this text".into(),
    }
}

impl ReadRepository for Fake {
    fn query(&self, plan: &QueryPlan, _: &Access) -> Result<QuerySnapshot, Error> {
        let day: NaiveDate = DAY.parse().unwrap();
        let mut snapshot = QuerySnapshot {
            manifest: vec![],
            rows: vec![],
            temporal: None,
            limitations: vec![],
        };
        for entity in &plan.entities {
            let reference = version_ref(entity);
            snapshot.manifest.push(ManifestEntry {
                entity: Some(entity.clone()),
                version_ref: Some(reference.clone()),
                status: ManifestStatus::Present,
                metadata: Some(Metadata {
                    source: "fabricated".into(),
                    published_at: Utc::now(),
                    coverage_status: "complete".into(),
                    coverage_reason: format!("fabricated coverage for {}", entity.code),
                    units: [
                        "price_currency",
                        "price_unit",
                        "volume_unit",
                        "amount_currency",
                        "amount_unit",
                    ]
                    .map(evidence)
                    .into(),
                    adjustment: Some(evidence("price_adjustment")),
                    lineage: None,
                    index: None,
                    verified_through: None,
                    price_scale: None,
                    last_trade_date: None,
                    delisting_date: None,
                }),
                stale: None,
                as_of_day: None,
                reason: None,
            });
            snapshot.rows.push(Row::Market {
                entity: entity.clone(),
                version_ref: reference,
                date: day,
                first_trading_date: None,
                last_trading_date: None,
                year_unfinished: None,
                open: "10.01".into(),
                high: "10.50".into(),
                low: "9.90".into(),
                close: "10.20".into(),
                volume: Some("123456".into()),
                amount: Some("1259251.20".into()),
                no_trade: None,
            });
        }
        Ok(snapshot)
    }
    fn verify(&self, _: &[VersionRef]) -> Result<(), Error> {
        Ok(())
    }
    fn versions(&self, _: &VersionsQuery, _: &Access) -> Result<Vec<VersionInfo>, Error> {
        Ok(vec![])
    }
    fn universe_members(
        &self,
        _: Dataset,
        _: Option<&str>,
        _: Option<CatalogScope>,
        _: Option<NaiveDate>,
        _: Option<NaiveDate>,
    ) -> Result<(String, Vec<Security>), Error> {
        Ok((
            "catalog-fabricated".into(),
            (0..self.entities).map(security).collect(),
        ))
    }
    fn calendar_days(&self, _: NaiveDate, _: NaiveDate) -> Result<Vec<NaiveDate>, Error> {
        Ok(vec![DAY.parse().unwrap()])
    }
    fn last_trading_day_upto(&self, day: NaiveDate) -> Result<NaiveDate, Error> {
        Ok(day)
    }
}

pub struct Handler(pub Queries<Fake>);

impl ToolHandler for Handler {
    fn tools(&self) -> Value {
        json!([])
    }
    fn call(&self, _: &str, args: Value) -> Result<Value, ApiError> {
        let query: Query = serde_json::from_value(args).map_err(|e| ApiError {
            code: "invalid_arguments".into(),
            message: e.to_string(),
            scopes: None,
            outcome_certainty: None,
            details: None,
        })?;
        self.0
            .query_v2(query, &Access::local())
            .map_err(|e| ApiError {
                code: if e.to_string().contains("memory_budget_exceeded") {
                    "memory_budget_exceeded"
                } else {
                    "invalid_arguments"
                }
                .into(),
                message: e.to_string(),
                scopes: None,
                outcome_certainty: None,
                details: None,
            })
    }
}

/// `get_market_data`-shaped request: whole-universe one-day cross-section.
pub fn request(cursor: Option<&str>, page_size: usize, id: u64) -> String {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":"get_market_data","arguments":{
        "dataset":"market.daily","contract_version":"v2",
        "selection":{"kind":"universe"},
        "filter":{"kind":"cross_section","start_date":DAY,"end_date":DAY},
        "version":{"kind":"latest_per_entity"},
        "page_size":page_size,"cursor":cursor}}})
    .to_string()
}

/// `next_cursor` of a `tools/call` response, `None` on the last page.
pub fn next_cursor(response: &str) -> Option<String> {
    let value: Value = serde_json::from_str(response).unwrap();
    value["result"]["structuredContent"]["next_cursor"]
        .as_str()
        .map(str::to_owned)
}

pub fn is_error(response: &str) -> bool {
    let value: Value = serde_json::from_str(response).unwrap();
    value["result"]["isError"] == json!(true)
}

pub fn error_code(response: &str) -> String {
    let value: Value = serde_json::from_str(response).unwrap();
    value["result"]["structuredContent"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}
