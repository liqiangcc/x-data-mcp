//! `#607` PR-E: ETF 前复权 in the research snapshot (design §7.1/§7.2/§7.3).
//!
//! The gate is the read surface's own object
//! ([`crate::source_store::etf_factor_read_gate::ForwardGate`]) over a fixture
//! source store built through the real importer, so "admitted" here means
//! exactly what `market.daily` would answer.
//!
//! What these tests hold the export to:
//!
//! * an admitted ETF's `daily_qfq` rows are the stored derived rows: `price_scale=6`,
//!   volume/amount 不复权, and the same bytes the MCP read returns;
//! * a blocked ETF keeps its `daily_raw` rows, gets no `daily_qfq` row, and the
//!   manifest names it with the read face's reason code (§7.1: never silently
//!   missing);
//! * `factors.parquet` carries `basis_status`: the ETF event's check verdict,
//!   `unverified` without a record, and the fixed A-share value; the file's own
//!   schema version moves, the daily files' does not (§7.2 (a), §7.3);
//! * two exports of one database are byte-identical, and the ETF block enters
//!   the snapshot identity so a different check set is a different snapshot.

use super::*;
use crate::research::export::{Compression, ExportOptions, export, export_with_gate};
use crate::research::{FACTORS_SCHEMA_VERSION, SnapshotPlan};
use crate::source_store::etf_factor_checks::{CheckStatus, EtfFactorCheck};
use crate::source_store::etf_factor_read_gate::ForwardGate;
use parquet::file::reader::{FileReader as _, SerializedFileReader};
use parquet::record::Field;

fn options(root: &Path) -> ExportOptions {
    ExportOptions {
        out_root: root.to_path_buf(),
        compression: Compression::Snappy,
        row_group_rows: 2,
    }
}

/// The shared fixture helpers: [`etf_check_record`] reads the digest the
/// fixture actually wrote, [`etf_gate_with`] imports records through the real
/// importer.
fn check_record(fixture: &Fixture, entity: &str, status: CheckStatus) -> EtfFactorCheck {
    etf_check_record(fixture, entity, status)
}

fn gate_with(
    dir: &tempfile::TempDir,
    records: &[EtfFactorCheck],
) -> std::sync::Arc<ForwardGate> {
    etf_gate_with(dir.path(), records)
}

type Row = Vec<(String, Field)>;

fn rows_of(directory: &Path, name: &str) -> Vec<Row> {
    let reader =
        SerializedFileReader::new(std::fs::File::open(directory.join(name)).unwrap()).unwrap();
    reader
        .get_row_iter(None)
        .unwrap()
        .map(|row| {
            row.unwrap()
                .get_column_iter()
                .map(|(name, field)| (name.clone(), field.clone()))
                .collect()
        })
        .collect()
}

fn cell<'a>(row: &'a Row, column: &str) -> &'a Field {
    &row.iter().find(|(name, _)| name == column).unwrap().1
}

/// The cell as plain text: `Field`'s `Display` quotes strings, so unquote.
fn text(row: &Row, column: &str) -> String {
    match cell(row, column) {
        Field::Str(value) => value.clone(),
        other => other.to_string(),
    }
}

fn manifest(directory: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(directory.join("manifest.json")).unwrap()).unwrap()
}

/// The manifest's `etf_forward` block.
fn etf_forward(directory: &Path) -> serde_json::Value {
    manifest(directory)["etf_forward"].clone()
}

fn blocked_entities(directory: &Path) -> Vec<String> {
    etf_forward(directory)["blocked"]
        .as_array()
        .expect("blocked list")
        .iter()
        .map(|entry| entry["entity"].as_str().unwrap().to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// admission
// ---------------------------------------------------------------------------

#[test]
fn an_admitted_etf_export_carries_the_read_face_qfq_bytes() {
    let fixture = Fixture::with_etf();
    let store = tempdir();
    let gate = gate_with(
        &store,
        &[check_record(&fixture, "sh.510050", CheckStatus::VerifiedOfficial)],
    );
    let out = tempdir();
    let request = ExportRequest::default();
    let outcome = export_with_gate(fixture.path(), &request, &options(out.path()), gate.clone())
        .unwrap();
    let directory = outcome.directory.clone();

    // `sz.159919` has no event, so nothing to verify and nothing to block; the
    // admitted ETF is the one whose event the store vouches for.
    let forward = etf_forward(&directory);
    assert_eq!(
        forward["admitted"],
        serde_json::json!(["sh.510050", "sz.159919"]),
        "{forward}"
    );
    assert_eq!(forward["blocked"], serde_json::json!([]));
    assert!(forward["checks_digest"].is_string());

    // The 前复权 rows are the stored derived rows: class 2, 6dp prices, and the
    // 不复权 volume/amount carried through (design §4.1).
    let qfq = rows_of(&directory, crate::research::DAILY_QFQ_FILE);
    let etf_rows: Vec<&Row> = qfq
        .iter()
        .filter(|row| text(row, "instrument_class") == "2")
        .collect();
    assert_eq!(etf_rows.len(), 6, "3 bars × 2 ETFs");
    let raw = rows_of(&directory, crate::research::DAILY_RAW_FILE);
    let raw_etf: Vec<&Row> = raw
        .iter()
        .filter(|row| text(row, "instrument_class") == "2")
        .collect();
    assert_eq!(raw_etf.len(), 6);
    for (qfq_row, raw_row) in etf_rows.iter().zip(&raw_etf) {
        assert_eq!(text(qfq_row, "entity"), text(raw_row, "entity"));
        assert_eq!(text(qfq_row, "price_scale"), "6");
        assert_eq!(text(raw_row, "price_scale"), "3");
        // volume/amount are not adjusted: the 前复权 file repeats the 不复权
        // bytes rather than rescaling them.
        assert_eq!(text(qfq_row, "volume"), text(raw_row, "volume"));
        assert_eq!(text(qfq_row, "amount"), text(raw_row, "amount"));
        // `no_trade` is an ETF series fact, so the 前复权 block repeats it.
        assert_eq!(text(qfq_row, "no_trade"), text(raw_row, "no_trade"));
    }
    // `sh.510050`'s first bar is 2.501 元 不复权; the fixture's one cat11 event
    // (÷2.5) applies to the bars strictly before its 除权日, so the first bar's
    // 前复权 close is 2.501 / 2.5 = 1.000400.
    let first = etf_rows
        .iter()
        .find(|row| text(row, "entity") == "sh.510050")
        .unwrap();
    assert_eq!(text(first, "close"), "1.000400");

    // The identity entry records which check set admitted the block.
    let entries = manifest(&directory)["series"].as_array().unwrap().clone();
    let entry = entries
        .iter()
        .find(|entry| entry["entity"] == "sh.510050" && entry["adjustment"] == "qfq")
        .expect("ETF 前复权 series entry");
    assert_eq!(entry["price_scale"], 6);
    assert_eq!(entry["instrument_class"], "etf");
    assert_eq!(entry["etf_checks_digest"], forward["checks_digest"]);
    // No other series entry carries the key.
    for other in &entries {
        if other["adjustment"] != "qfq" || other["instrument_class"] != "etf" {
            assert!(other.get("etf_checks_digest").is_none(), "{other}");
        }
    }
}

#[test]
fn a_blocked_etf_is_absent_from_daily_qfq_and_named_in_the_manifest() {
    // The store holds a record for `sh.510050`'s event but its verdict is
    // `unverified`: the read face blocks the entity, so the export must too.
    let fixture = Fixture::with_etf();
    let store = tempdir();
    let gate = gate_with(
        &store,
        &[check_record(&fixture, "sh.510050", CheckStatus::Unverified)],
    );
    let out = tempdir();
    let outcome =
        export_with_gate(fixture.path(), &ExportRequest::default(), &options(out.path()), gate)
            .unwrap();
    let directory = outcome.directory.clone();

    let qfq = rows_of(&directory, crate::research::DAILY_QFQ_FILE);
    assert!(
        qfq.iter()
            .all(|row| text(row, "entity") != "sh.510050"),
        "a blocked ETF must not reach daily_qfq"
    );
    // Its 不复权 rows stay: blocking is about 前复权, never about dropping data.
    let raw = rows_of(&directory, crate::research::DAILY_RAW_FILE);
    assert_eq!(
        raw.iter()
            .filter(|row| text(row, "instrument_class") == "2")
            .count(),
        6
    );

    let forward = etf_forward(&directory);
    let blocked = forward["blocked"].as_array().unwrap();
    assert_eq!(blocked.len(), 1, "{forward}");
    assert_eq!(blocked[0]["entity"], "sh.510050");
    // The read face's wire reason code, not a second vocabulary.
    assert_eq!(blocked[0]["reason"], "etf_factor_unverified");
    assert_eq!(blocked[0]["blocking_events"], 1);
    // `sz.159919` has no ETF event, so there is nothing to verify and it is
    // admitted; blocking is per entity and per event, never blanket.
    assert_eq!(forward["admitted"], serde_json::json!(["sz.159919"]));
}

#[test]
fn a_missing_source_store_blocks_every_etf_without_failing_the_export() {
    // Design §7.1 requirement 3: the store being unset is not an export
    // failure. The default (env) gate has no store under nextest, so this is
    // the production-misconfiguration path.
    let fixture = Fixture::with_etf();
    let out = tempdir();
    let outcome = export(fixture.path(), &ExportRequest::default(), &options(out.path())).unwrap();
    let directory = outcome.directory.clone();

    let blocked = blocked_entities(&directory);
    assert_eq!(blocked, ["sh.510050", "sz.159919"]);
    let forward = etf_forward(&directory);
    assert!(forward["checks_digest"].is_null());
    for entry in forward["blocked"].as_array().unwrap() {
        assert_eq!(entry["reason"], "etf_factor_unverified", "{entry}");
    }
    // The export is complete and readable: every file is there.
    for name in [
        crate::research::DAILY_RAW_FILE,
        crate::research::DAILY_QFQ_FILE,
        crate::research::FACTORS_FILE,
    ] {
        assert!(directory.join(name).is_file(), "missing {name}");
    }
}

#[test]
fn a_raw_only_export_writes_no_etf_qfq_row_and_its_daily_bytes_are_store_independent() {
    // `--adjustment raw` keeps its old daily behavior exactly: no ETF 前复权
    // row, and the daily files do not depend on the source store.
    //
    // `factors.parquet` does depend on it, by design (§7.3): an ETF event's
    // `basis_status` is a property of the event, not of which daily files were
    // written. So the snapshot id moves with the check set — the id has to
    // cover bytes that can differ.
    let fixture = Fixture::with_etf();
    let request = ExportRequest {
        adjustment: AdjustmentSelection::Raw,
        ..ExportRequest::default()
    };
    let bare = tempdir();
    let plain = export(fixture.path(), &request, &options(bare.path())).unwrap();

    let store = tempdir();
    let gate = gate_with(
        &store,
        &[check_record(&fixture, "sh.510050", CheckStatus::VerifiedOfficial)],
    );
    let stored = tempdir();
    let with_store =
        export_with_gate(fixture.path(), &request, &options(stored.path()), gate).unwrap();

    let sha = |outcome: &crate::research::export::ExportOutcome, name: &str| {
        outcome
            .files
            .iter()
            .find(|file| file.path == name)
            .unwrap()
            .sha256
            .clone()
    };
    assert_eq!(
        sha(&plain, crate::research::DAILY_RAW_FILE),
        sha(&with_store, crate::research::DAILY_RAW_FILE),
        "daily_raw must not depend on the source store for a raw export"
    );
    // A raw-only export writes no 前复权 file at all (unchanged behavior).
    assert!(
        !with_store
            .directory
            .join(crate::research::DAILY_QFQ_FILE)
            .exists()
    );
    let forward = etf_forward(&with_store.directory);
    assert_eq!(forward["admitted"], serde_json::json!([]));
    assert_eq!(forward["blocked"], serde_json::json!([]));
    assert!(
        forward["checks_digest"].is_string(),
        "the factor file's basis_status was judged against this set: {forward}"
    );
    // Without a store the same export says so and produces different factor
    // bytes, hence a different snapshot id.
    assert!(etf_forward(&plain.directory)["checks_digest"].is_null());
    assert_ne!(plain.snapshot_id, with_store.snapshot_id);
}

// ---------------------------------------------------------------------------
// factors.parquet basis_status
// ---------------------------------------------------------------------------

#[test]
fn factors_basis_status_reports_the_check_verdict_and_the_fixed_a_share_value() {
    let fixture = Fixture::with_etf();
    let store = tempdir();
    let gate = gate_with(
        &store,
        &[check_record(&fixture, "sh.510050", CheckStatus::VerifiedAnnouncement)],
    );
    let out = tempdir();
    let outcome =
        export_with_gate(fixture.path(), &ExportRequest::default(), &options(out.path()), gate)
            .unwrap();
    let rows = rows_of(&outcome.directory, crate::research::FACTORS_FILE);
    assert!(!rows.is_empty());
    let mut etf_rows = 0;
    for row in &rows {
        let entity = text(row, "entity");
        let status = text(row, "basis_status");
        if entity.starts_with("sh.51") || entity.starts_with("sz.159") {
            etf_rows += 1;
            // `sh.510050`'s event is verified; no other ETF has an event, so
            // any ETF row here must be that one.
            assert_eq!(entity, "sh.510050");
            assert_eq!(status, "verified_announcement");
        } else {
            // A shares carry one fixed value (design §7.3): the export never
            // re-derives an A-share verdict from the ETF check store.
            assert_eq!(status, crate::research::read::FactorBasisStatus::STOCK);
        }
    }
    assert_eq!(etf_rows, 1, "one ETF factor event in the fixture");

    // The file's own schema version moved; the daily files did not.
    let manifest = manifest(&outcome.directory);
    let factor_file = manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == crate::research::FACTORS_FILE)
        .unwrap();
    assert_eq!(factor_file["schema_version"], FACTORS_SCHEMA_VERSION);
    for name in [
        crate::research::DAILY_RAW_FILE,
        crate::research::DAILY_QFQ_FILE,
    ] {
        let file = manifest["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|file| file["path"] == name)
            .unwrap();
        assert!(
            file.get("schema_version").is_none() || file["schema_version"].is_null(),
            "the daily files have no version of their own: {file}"
        );
    }
    assert_eq!(
        crate::research::FACTOR_COLUMNS.last().copied(),
        Some("basis_status")
    );
}

#[test]
fn an_etf_factor_event_without_a_check_record_is_unverified() {
    // No store: nothing can be shown to be verified, so the column says so
    // rather than borrowing the A-share value.
    let fixture = Fixture::with_etf();
    let out = tempdir();
    let outcome = export(fixture.path(), &ExportRequest::default(), &options(out.path())).unwrap();
    let rows = rows_of(&outcome.directory, crate::research::FACTORS_FILE);
    let etf = rows
        .iter()
        .find(|row| text(row, "entity") == "sh.510050")
        .expect("the ETF event is exported");
    assert_eq!(
        text(etf, "basis_status"),
        crate::research::read::FactorBasisStatus::UNVERIFIED
    );
}

// ---------------------------------------------------------------------------
// identity / reproducibility
// ---------------------------------------------------------------------------

#[test]
fn two_exports_of_one_database_are_byte_identical() {
    let fixture = Fixture::with_etf();
    let store = tempdir();
    let records = [
        check_record(&fixture, "sh.510050", CheckStatus::VerifiedOfficial),
        // A newer record for the same event: `latest_by_key` wins, so the
        // export must be stable under an unchanged newest record and must not
        // depend on store iteration order.
        EtfFactorCheck {
            checked_at: "2026-11-10".into(),
            ..check_record(&fixture, "sh.510050", CheckStatus::VerifiedOfficial)
        },
    ];
    let request = ExportRequest::default();
    let first = tempdir();
    let second = tempdir();
    let a = export_with_gate(
        fixture.path(),
        &request,
        &options(first.path()),
        gate_with(&store, &records),
    )
    .unwrap();
    let b = export_with_gate(
        fixture.path(),
        &request,
        &options(second.path()),
        gate_with(&store, &records),
    )
    .unwrap();
    assert_eq!(a.snapshot_id, b.snapshot_id);
    for (left, right) in a.files.iter().zip(&b.files) {
        assert_eq!(left.path, right.path);
        assert_eq!(left.rows, right.rows);
        assert_eq!(left.sha256, right.sha256, "{}", left.path);
    }
}

#[test]
fn a_different_check_set_is_a_different_snapshot() {
    // The admission is an input (design §7.1): two exports of the same
    // database that differ only in which ETFs the gate let through must not
    // share a snapshot id.
    let fixture = Fixture::with_etf();
    let request = ExportRequest::default();
    let admitted_store = tempdir();
    let admitted = tempdir();
    let with = export_with_gate(
        fixture.path(),
        &request,
        &options(admitted.path()),
        gate_with(
            &admitted_store,
            &[check_record(&fixture, "sh.510050", CheckStatus::VerifiedOfficial)],
        ),
    )
    .unwrap();
    let blocked_store = tempdir();
    let blocked = tempdir();
    let without = export_with_gate(
        fixture.path(),
        &request,
        &options(blocked.path()),
        gate_with(&blocked_store, &[]),
    )
    .unwrap();
    assert_ne!(with.snapshot_id, without.snapshot_id);
    let qfq = |dir: &Path| {
        rows_of(dir, crate::research::DAILY_QFQ_FILE)
            .iter()
            .filter(|row| text(row, "instrument_class") == "2")
            .count()
    };
    assert_eq!(qfq(&with.directory), 6, "both ETFs admitted");
    // `sz.159919` has no event to verify, so it is admitted either way; only
    // `sh.510050` moves between the two exports.
    assert_eq!(qfq(&without.directory), 3);
    // The digest is part of the identity, so a consumer can tell which check
    // set a snapshot was admitted under.
    assert_eq!(
        manifest(&with.directory)["etf_forward"]["checks_digest"],
        etf_forward(&with.directory)["checks_digest"]
    );
    assert_ne!(
        etf_forward(&with.directory)["checks_digest"],
        etf_forward(&without.directory)["checks_digest"]
    );
}

#[test]
fn a_share_bytes_are_unchanged_by_etf_qfq() {
    // §7.1 requirement 1: the A-share block is byte-identical to the one
    // before PR-E. Compared against an export of the plain fixture (no ETFs at
    // all), so "unchanged" is executable rather than asserted.
    let plain_out = tempdir();
    let etf_out = tempdir();
    let plain = export(
        Fixture::new().path(),
        &ExportRequest::default(),
        &options(plain_out.path()),
    )
    .unwrap();
    let store = tempdir();
    let with = export_with_gate(
        Fixture::with_etf().path(),
        &ExportRequest::default(),
        &options(etf_out.path()),
        gate_with(
            &store,
            &[check_record(
                &Fixture::with_etf(),
                "sh.510050",
                CheckStatus::VerifiedOfficial,
            )],
        ),
    )
    .unwrap();

    let stock_rows = |directory: &Path, name: &str| -> Vec<Row> {
        rows_of(directory, name)
            .into_iter()
            .filter(|row| text(row, "instrument_class") != "2")
            .collect()
    };
    for name in [
        crate::research::DAILY_RAW_FILE,
        crate::research::DAILY_QFQ_FILE,
    ] {
        assert_eq!(
            stock_rows(&plain.directory, name),
            stock_rows(&with.directory, name),
            "{name}: the A-share rows changed"
        );
    }
    // The ETF 前复权 rows were appended after the A-share block, never mixed in.
    let qfq = rows_of(&with.directory, crate::research::DAILY_QFQ_FILE);
    let first_etf = qfq
        .iter()
        .position(|row| text(row, "instrument_class") == "2")
        .unwrap();
    assert!(
        qfq[first_etf..]
            .iter()
            .all(|row| text(row, "instrument_class") == "2"),
        "the ETF block is a suffix of daily_qfq"
    );
}

#[test]
fn the_plan_holds_the_same_admission_the_read_face_answers() {
    // The plan's ETF block is not a second opinion: `Sz159919` reads back from
    // the MCP surface at exactly the value the export wrote.
    let fixture = Fixture::with_etf();
    let store = tempdir();
    let gate = gate_with(
        &store,
        &[check_record(&fixture, "sh.510050", CheckStatus::VerifiedOfficial)],
    );
    let out = tempdir();
    let outcome =
        export_with_gate(fixture.path(), &ExportRequest::default(), &options(out.path()), gate)
            .unwrap();
    let rows = rows_of(&outcome.directory, crate::research::DAILY_QFQ_FILE);
    let close_of = |entity: &str, day: &str| -> String {
        rows.iter()
            .find(|row| text(row, "entity") == entity && text(row, "trade_day") == day)
            .map(|row| text(row, "close"))
            .unwrap_or_else(|| panic!("no {entity} row for {day}"))
    };
    // 不复权 sz.159919 closes are 4.123/4.124/4.125 with no factor, so 前复权
    // repeats them at 6dp.
    assert_eq!(close_of("sz.159919", &days()[0].to_string()), "4.123000");
    // sh.510050's cat11 (÷2.5) applies strictly before the event day: the first
    // bar is adjusted, the second (the event day) is not.
    assert_eq!(close_of("sh.510050", &days()[0].to_string()), "1.000400");
    assert_eq!(close_of("sh.510050", &days()[1].to_string()), "2.502000");
}

#[test]
fn the_plan_carries_the_per_entity_verdicts() {
    let fixture = Fixture::with_etf();
    // An empty but readable store: the gate reaches the stored facts and can
    // say how many events it refused, which a missing store cannot.
    let store = tempdir();
    let gate = gate_with(&store, &[]);
    let plan: SnapshotPlan =
        crate::research::plan_with_gate(&fixture.read(), &ExportRequest::default(), gate.as_ref())
            .unwrap();
    // `sz.159919` has no ETF event, so an empty store blocks nothing for it.
    assert_eq!(plan.etf_forward.blocked.len(), 1);
    for entry in &plan.etf_forward.blocked {
        assert_eq!(entry.reason, "etf_factor_unverified");
        // `sh.510050` has one stored event; `sz.159919` has none, so the gate
        // refuses it for the missing store rather than for a specific event.
        let (events, categories) = if entry.entity == "sh.510050" {
            (1, vec![11])
        } else {
            (0, Vec::new())
        };
        assert_eq!(entry.blocking_events, Some(events), "{}", entry.entity);
        assert_eq!(entry.categories, Some(categories), "{}", entry.entity);
    }
}


// ---------------------------------------------------------------------------
// cost
// ---------------------------------------------------------------------------

/// The cost of the ETF 前复权 block, measured rather than guessed.
///
/// `#607` PR-E adds roughly 1.65M rows to `daily_qfq` in production (the ETFs
/// that carry a stored `Forward` series, each mirroring its 不复权 bar count).
/// This fixture replaces the two fixture ETFs' 前复权 series with
/// `2 × BARS_PER_ETF` rows written through the production writer and exports
/// them, so the row count, the file size and the wall clock quoted in the PR
/// are measurements of this code, not extrapolations.
///
/// Ignored by default: it is a benchmark, not a contract. Run it with
/// `cargo nextest run --features export -E 'test(etf_qfq_export_cost)' \
///   --run-ignored only --no-capture`.
#[test]
#[ignore = "cost fixture: exports ~1.65M ETF 前复权 rows"]
fn etf_qfq_export_cost() {
    const BARS_PER_ETF: usize = 825_000;
    let fixture = Fixture::with_etf();
    {
        let c = fixture.write();
        let tx = c.unchecked_transaction().unwrap();
        for entity in ["sh.510050", "sz.159919"] {
            let series_key: i64 = tx
                .query_row(
                    "SELECT series_key FROM market_series \
                     WHERE entity=?1 AND period=1 AND adjustment=?2",
                    params![entity, Adjustment::Forward.code()],
                    |row| row.get(0),
                )
                .unwrap();
            let removed = tx
                .execute("DELETE FROM series_bars WHERE series_key=?1", params![series_key])
                .unwrap();
            assert_eq!(removed, 3, "{entity}: the fixture's 3 stored bars");
            // 1.65M rows in total: the production-scale block this release adds.
            let bars: Vec<CompactBar> = (0..BARS_PER_ETF)
                .map(|offset| CompactBar {
                    trade_day: 10_000 + offset as i64,
                    open_units: 1_000_000 + offset as i64,
                    high_units: 1_010_000 + offset as i64,
                    low_units: 990_000 + offset as i64,
                    close_units: 1_005_000 + offset as i64,
                    volume: Some(100_000),
                    amount: Some("100500.00".into()),
                    r7_raw: None,
                })
                .collect();
            crate::sqlite_market::series::insert_bars(&tx, series_key, &bars).unwrap();
            // The export compares each series' row count with the manifest, and
            // `record_count` is the series fact it compares against.
            tx.execute(
                "UPDATE market_series SET record_count=?2, first_day=?3, last_day=?4 \
                 WHERE series_key=?1",
                params![
                    series_key,
                    BARS_PER_ETF as i64,
                    10_000i64,
                    10_000 + BARS_PER_ETF as i64 - 1,
                ],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }

    let store = tempdir();
    let gate = gate_with(
        &store,
        &[check_record(&fixture, "sh.510050", CheckStatus::VerifiedOfficial)],
    );
    let out = tempdir();
    let started = std::time::Instant::now();
    let outcome = export_with_gate(
        fixture.path(),
        // Only the two ETFs: the file then holds nothing but the block under
        // measurement, so bytes/row is the ETF 前复权 row's cost and not a mix.
        &ExportRequest {
            dataset: DatasetSelection::Daily,
            adjustment: AdjustmentSelection::Qfq,
            entities: EntitySelection::Explicit(vec!["sh.510050".into(), "sz.159919".into()]),
            ..ExportRequest::default()
        },
        // The production row-group size, not the 2-row fixture default: the
        // file layout (and so bytes/row) must match what an export writes.
        &ExportOptions {
            out_root: out.path().to_path_buf(),
            compression: Compression::Snappy,
            row_group_rows: 65_536,
        },
        gate,
    )
    .unwrap();
    let elapsed = started.elapsed();
    let qfq = outcome
        .files
        .iter()
        .find(|file| file.path == crate::research::DAILY_QFQ_FILE)
        .unwrap();
    eprintln!(
        "etf_qfq_export_cost: {} ETF qfq rows, {} bytes ({:.1} bytes/row), {:?} total ({:.0} rows/s)",
        qfq.rows,
        qfq.bytes,
        qfq.bytes as f64 / qfq.rows as f64,
        elapsed,
        qfq.rows as f64 / elapsed.as_secs_f64(),
    );
    assert_eq!(qfq.rows as usize, 2 * BARS_PER_ETF);
}
