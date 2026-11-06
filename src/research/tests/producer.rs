//! Manifest `producer` block and the `v5` column layout (audit T7, Refs #383).
//!
//! `producer` (binary `git_sha`, derivation labels) is recorded in
//! `manifest.json` and is not part of `snapshot_id`. `v5` names the ETF column
//! layout that `v4` shared with the shorter preview.42/43 layout.

use super::*;
use crate::research::manifest;

#[cfg(feature = "export")]
/// Keys `manifest::document` adds on top of the identity.
const DOCUMENT_ONLY: [&str; 5] = ["snapshot_id", "producer", "files", "limitations", "export"];

#[cfg(feature = "export")]
fn labels() -> serde_json::Value {
    json!({
        "price_limit": crate::trading_rules::ALGORITHM_VERSION,
        "market_cap": crate::research::market_cap::ALGORITHM_VERSION,
        "security_master": crate::security_master::assembly::ALGORITHM_VERSION,
        "security_names": crate::security_master::names::ALGORITHM_VERSION,
        "risk_warnings": crate::security_master::risk_warnings::ALGORITHM_VERSION,
        "suspensions": crate::security_master::suspensions::ALGORITHM_VERSION,
    })
}

#[test]
fn the_schema_string_is_v5() {
    assert_eq!(manifest::SCHEMA, "x-data-mcp.research-snapshot.v5");
}

#[cfg(feature = "export")]
#[test]
fn the_dry_run_manifest_names_the_binary_and_the_derivation_labels() {
    use crate::research::export::dry_run;

    let fixture = Fixture::new();
    let (snapshot, document) = dry_run(
        fixture.path(),
        &ExportRequest::default(),
        &crate::research::export::ExportOptions::default(),
    )
    .unwrap();

    let sha = document["producer"]["git_sha"].as_str().unwrap();
    assert_eq!(sha, env!("X_DATA_MCP_BUILD_SHA"));
    assert!(
        sha == "unknown" || (sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit())),
        "{sha}"
    );
    assert_eq!(document["producer"]["algorithms"], labels());
    // The literal labels, so a silent rename of a constant is a visible diff.
    assert_eq!(
        document["producer"]["algorithms"]["price_limit"],
        "limit-v1"
    );
    assert_eq!(
        document["producer"]["algorithms"]["market_cap"],
        "market-cap-v1"
    );

    // The identity block `algorithms` keeps its four stored labels and gains
    // neither the SHA nor the derivation labels.
    let identity_algorithms = document["algorithms"].as_object().unwrap();
    let mut keys: Vec<&str> = identity_algorithms.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["calendar", "factor_normalize", "qfq", "series_digest"]
    );
    assert!(snapshot.identity.get("producer").is_none());
}

#[cfg(feature = "export")]
#[test]
fn snapshot_id_does_not_depend_on_the_producer() {
    use crate::research::export::dry_run;

    let fixture = Fixture::new();
    let (snapshot, document) = dry_run(
        fixture.path(),
        &ExportRequest::default(),
        &crate::research::export::ExportOptions::default(),
    )
    .unwrap();

    // Strip everything the document adds and re-hash: the id is the hash of
    // what is left, so `producer` is not an input of it.
    let mut rebuilt = document.clone();
    for key in DOCUMENT_ONLY {
        rebuilt.as_object_mut().unwrap().remove(key);
    }
    assert_eq!(rebuilt, snapshot.identity);
    assert_eq!(
        manifest::snapshot_id(&rebuilt).unwrap(),
        document["snapshot_id"]
    );

    // A different binary (another SHA, another derivation label) leaves the
    // id as it is; a change to an identity field does not.
    let mut other_binary = document.clone();
    other_binary["producer"] =
        json!({"git_sha": "0".repeat(40), "algorithms": {"price_limit": "limit-v9"}});
    let mut stripped = other_binary;
    for key in DOCUMENT_ONLY {
        stripped.as_object_mut().unwrap().remove(key);
    }
    assert_eq!(
        manifest::snapshot_id(&stripped).unwrap(),
        snapshot.snapshot_id
    );
    let mut changed = stripped;
    changed["algorithms"]["qfq"] = json!("qfq-other");
    assert_ne!(
        manifest::snapshot_id(&changed).unwrap(),
        snapshot.snapshot_id
    );
}

#[test]
fn the_v5_layout_is_the_etf_layout_of_preview_44_and_45() {
    // What preview.44/45 wrote under the `v4` string. `v5` must not move it:
    // the string is the reader's only shortcut to "these columns".
    let daily = [
        "entity",
        "trade_day",
        "open",
        "high",
        "low",
        "close",
        "price_scale",
        "volume",
        "amount",
        "r7_raw",
        "volume_quantum",
        "instrument_class",
        "no_trade",
    ];
    assert_eq!(crate::research::DAILY_COLUMNS, daily);
    assert_eq!(
        crate::research::UNIVERSE_INTERVALS_COLUMNS,
        [
            "entity",
            "canonical_entity",
            "aliases",
            "member_from",
            "member_to",
            "basis",
            "flags",
            "instrument_class",
            "last_trade_date",
            "delisting_date",
        ]
    );
}

#[cfg(feature = "export")]
mod export_files {
    use super::*;
    use crate::research::export::{Compression, ExportOptions, export};

    fn options(root: &Path) -> ExportOptions {
        ExportOptions {
            out_root: root.to_path_buf(),
            compression: Compression::Snappy,
            row_group_rows: 2,
        }
    }

    #[test]
    fn the_written_manifest_carries_the_producer_and_is_byte_reproducible() {
        let fixture = Fixture::new();
        let first_root = tempdir();
        let second_root = tempdir();
        let first = export(
            fixture.path(),
            &ExportRequest::default(),
            &options(first_root.path()),
        )
        .unwrap();
        let second = export(
            fixture.path(),
            &ExportRequest::default(),
            &options(second_root.path()),
        )
        .unwrap();

        // Same binary, same database: the whole manifest, `producer` included,
        // is identical (AC-M1-7).
        let a = std::fs::read(first.directory.join("manifest.json")).unwrap();
        let b = std::fs::read(second.directory.join("manifest.json")).unwrap();
        assert_eq!(a, b);

        let written: serde_json::Value = serde_json::from_slice(&a).unwrap();
        assert_eq!(written["schema"], "x-data-mcp.research-snapshot.v5");
        assert_eq!(written["producer"]["git_sha"], env!("X_DATA_MCP_BUILD_SHA"));
        assert_eq!(written["producer"]["algorithms"], labels());

        // The written file re-hashes to its own id without `producer`.
        let mut rebuilt = written.clone();
        for key in DOCUMENT_ONLY {
            rebuilt.as_object_mut().unwrap().remove(key);
        }
        assert_eq!(manifest::snapshot_id(&rebuilt).unwrap(), first.snapshot_id);
    }

    #[test]
    fn the_listed_columns_are_what_a_reader_uses_to_tell_layouts_apart() {
        let fixture = Fixture::new();
        let out = tempdir();
        let outcome = export(
            fixture.path(),
            &ExportRequest::default(),
            &options(out.path()),
        )
        .unwrap();
        let written: serde_json::Value = serde_json::from_slice(
            &std::fs::read(outcome.directory.join("manifest.json")).unwrap(),
        )
        .unwrap();
        for file in [
            "daily_raw.parquet",
            "daily_qfq.parquet",
            "universe_intervals.parquet",
        ] {
            let entry = written["files"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["path"] == file)
                .unwrap();
            let columns: Vec<&str> = entry["columns"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c.as_str().unwrap())
                .collect();
            assert!(columns.contains(&"instrument_class"), "{file}: {columns:?}");
        }
    }
}
