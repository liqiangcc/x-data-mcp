//! Explicit legacy compound day/year contract; no new public dataset variants.
use super::*;
use serde_json::json;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload {
    symbols: Vec<String>,
    periods: Vec<String>,
    #[serde(default = "qfq")]
    adjustment: String,
}
fn qfq() -> String {
    "qfq".into()
}
pub struct LegacyCollection;
impl Contract for LegacyCollection {
    fn key(&self) -> CommandKey {
        CommandKey {
            operation: "start_collection".into(),
            dataset: "legacy_market_compound".into(),
            contract_version: "task-acceptance-v1".into(),
        }
    }
    fn normalize(&self, payload: Value) -> Result<Value, Error> {
        let mut p: Payload =
            serde_json::from_value(payload).map_err(|e| Error::InvalidCommand(e.to_string()))?;
        if p.symbols.is_empty()
            || p.periods.is_empty()
            || p.adjustment != "qfq"
            || p.symbols.iter().any(|s| s.trim().is_empty())
            || p.periods
                .iter()
                .any(|p| !matches!(p.as_str(), "day" | "year"))
        {
            return Err(Error::InvalidCommand(
                "invalid legacy selection/adjustment".into(),
            ));
        }
        p.symbols.sort();
        p.symbols.dedup();
        p.periods.sort();
        p.periods.dedup();
        Ok(json!({"symbols":p.symbols,"periods":p.periods,"adjustment":p.adjustment}))
    }
    fn resolve(&self, intent: &Value) -> Result<FrozenWork, Error> {
        let p: Payload = serde_json::from_value(intent.clone())
            .map_err(|e| Error::InvalidCommand(e.to_string()))?;
        let mut items = Vec::new();
        let mut scopes = BTreeSet::new();
        for symbol in p.symbols {
            for period in &p.periods {
                let id = scope_id(&symbol, period);
                scopes.insert(id.clone());
                items.push(ItemSpec {
                    item_id: id.clone(),
                    scope_keys: BTreeSet::from([id]),
                    target: json!({"symbol":symbol,"period":period}),
                    progress_unit: "security_period".into(),
                    frozen_inputs: json!({}),
                });
            }
        }
        Ok(FrozenWork { items, scopes })
    }
}
pub fn scope_id(symbol: &str, period: &str) -> String {
    serde_json::to_string(&json!(["market", symbol, period])).unwrap()
}
