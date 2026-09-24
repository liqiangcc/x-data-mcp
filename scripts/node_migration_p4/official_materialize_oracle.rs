//! NM-075 oracle: emit observed outputs of x_data_mcp::official_market
//! (TdxMarketSource::inspect / from_env / materialize_session /
//! materialize_daily) for differential verification against the Node
//! port in src_node/official/official_market.mjs.
//!
//! usage: oracle <manifest-path> <snapshot-root> <now-rfc3339> <op>...
//!   op = inspect | fromenv | mat:<symbol> | sess:<s1,s2,...>
//! Each line: {"id","observed"}; observed = {"status":"ok",...} or
//! {"status":"error","error":"{Display}"}. `batch_json` is the exact
//! serde_json::to_string(&MarketDataBatch) bytes for byte-compare.
use serde_json::{json, Value};
use std::env;
use std::path::Path;
use x_data_mcp::market_data::SourceError;
use x_data_mcp::official_market::TdxMarketSource;

fn emit(id: &str, observed: Value) {
    println!("{}", serde_json::to_string(&json!({"id": id, "observed": observed})).unwrap());
}

fn mat_outcome(r: Result<x_data_mcp::market_data::MarketDataBatch, SourceError>) -> Value {
    match r {
        Ok(b) => json!({"status": "ok", "batch_json": serde_json::to_string(&b).unwrap()}),
        Err(e) => json!({"status": "error", "error": format!("{e}")}),
    }
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let manifest = std::fs::read(&args[0]).unwrap();
    let root = args[1].clone();
    let now = chrono::DateTime::parse_from_rfc3339(&args[2])
        .unwrap()
        .with_timezone(&chrono::Utc);
    for op in &args[3..] {
        if op == "inspect" {
            match TdxMarketSource::inspect(Path::new(&root), &manifest, now) {
                Ok(s) => emit(
                    op,
                    json!({"status": "ok", "manifest_sha256": s.manifest_sha256()}),
                ),
                Err(e) => emit(op, json!({"status": "error", "error": format!("{e}")})),
            }
        } else if op == "fromenv" {
            match TdxMarketSource::from_env() {
                Some(s) => emit(
                    op,
                    json!({"status": "some", "manifest_sha256": s.manifest_sha256()}),
                ),
                None => emit(op, json!({"status": "none"})),
            }
        } else if let Some(sym) = op.strip_prefix("mat:") {
            let out = TdxMarketSource::inspect(Path::new(&root), &manifest, now)
                .and_then(|s| s.materialize_daily(sym));
            emit(op, mat_outcome(out));
        } else if let Some(list) = op.strip_prefix("sess:") {
            match TdxMarketSource::inspect(Path::new(&root), &manifest, now) {
                Ok(s) => {
                    let sess = s.materialize_session();
                    let outs: Vec<Value> = list
                        .split(',')
                        .map(|sym| mat_outcome(sess.materialize_daily(sym)))
                        .collect();
                    emit(op, json!({"status": "ok", "symbols": outs}));
                }
                Err(e) => emit(op, json!({"status": "error", "error": format!("{e}")})),
            }
        }
    }
}
