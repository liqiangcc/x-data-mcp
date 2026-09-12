#![allow(dead_code)]
#[path = "market.rs"]
mod market;
use serde_json::{Value, json};
use x_data_mcp::{
    market_storage::{MarketRepository, PublishIntent, compat::legacy_digest},
    sqlite_market::SqliteMarket,
};
/// Independent one-observation-per-year fixture: yearly OHLC equals that year's sole daily bar.
pub fn publish(store: &SqliteMarket, coverage: bool) -> Value {
    let bars:Vec<_>=["100","90","80","70"].iter().enumerate().map(|(i,p)|json!({"date":format!("{}-12-30",2020+i),"open":p,"high":p,"low":p,"close":p,"volume":null,"amount":null})).collect();
    let mut daily = market::samples()[0]["record"].clone();
    daily["version_id"] = json!("metric-daily");
    daily["source"] = json!("fixture:metrics");
    daily["published_at"] = json!("2024-01-01T00:00:00Z");
    daily["records"] = json!(bars);
    daily["batch"]["bars"] = json!(bars);
    daily["batch"]["provenance"]["source"] = json!("fixture:metrics");
    daily["validation"] = json!({"record_count":4,"unique_record_count":4,"duplicate_count":0});
    daily["integrity_sha256"] = json!(legacy_digest(&daily).unwrap());
    market::published_fixture(store, &daily);
    let mut year = daily;
    year["version_id"] = json!("metric-yearly");
    year["period"] = json!("Year");
    year["batch"]["period"] = json!("Year");
    let lineage = json!({"base_daily_version_id":"metric-daily","algorithm_version":"local-natural-year-v1","first_trading_date":"2020-12-30","last_trading_date":"2023-12-30"});
    year["yearly_derivation"] = lineage.clone();
    year["batch"]["provenance"]["yearly_derivation"] = lineage;
    if coverage {
        year["analysis_coverage_v1"] = json!({"origin":"fixture","years":(2020..=2023).map(|year|json!({"year":year,"status":"complete","evidence_ref":"independent one-point-per-year fixture"})).collect::<Vec<_>>()});
    }
    year["integrity_sha256"] = json!(legacy_digest(&year).unwrap());
    let plan = market::prepare(store, &year, PublishIntent::HistoricalFixedBase);
    store.commit(&plan).unwrap();
    json!({"dataset":"market.yearly","contract_version":"v1","entity":{"exchange":"sh","code":"600006"},"version_id":"metric-yearly","integrity_sha256":year["integrity_sha256"]})
}
pub fn request(reference: Value) -> Value {
    json!({"contract_version":"v1","execution":"inline","inputs":[{"input_id":"prices","version_ref":reference}],"range":{"start_year":2020,"end_year":2023},"metrics":[{"metric_id":"down","name":"consecutive_direction","algorithm_version":"local-consecutive-direction-v1","input_ids":["prices"],"parameters":{"direction":"down","min_periods":3}}]})
}
