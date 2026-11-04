//! M3-2 PR-D: ETF rows in the research snapshot (D-70 Q7, D-72, AC-M3-2-2).
//!
//! The fixture is [`Fixture::with_etf`]: the default two-stock fixture plus two
//! ETF 不复权 series (3dp) and three ETF identities. Every "A-share unchanged"
//! assertion compares an export of that fixture with an export of the plain
//! fixture, so the claim is executable: adding ETFs to a database changes no
//! A-share series entry, no A-share row and no byte of a file an ETF does not
//! belong in.

use super::*;
use crate::research::{EtfBinding, SeriesEntry, SnapshotPlan};

fn etf_entries(series: &[SeriesEntry]) -> Vec<&SeriesEntry> {
    series
        .iter()
        .filter(|entry| entry.instrument_class == crate::market_data::INSTRUMENT_CLASS_ETF)
        .collect()
}

#[test]
fn plan_adds_etf_raw_series_with_the_d72_dates() {
    let fixture = Fixture::with_etf();
    let snapshot = plan(&fixture.read(), &ExportRequest::default()).unwrap();

    // 2 stocks × 2 口径 + 2 ETF raw + 1 index; the file list is unchanged.
    assert_eq!(snapshot.series.len(), 7);
    assert_eq!(snapshot.files.len(), 14);
    let etfs = etf_entries(&snapshot.series);
    assert_eq!(etfs.len(), 2, "ETFs have no 前复权 series");
    for entry in &etfs {
        assert_eq!(entry.adjustment, Adjustment::Raw);
        assert_eq!(entry.price_scale, 3);
        assert_eq!(entry.instrument_class_name(), "etf");
    }
    // Order: the A-share block first, then the ETF block, the index last.
    let order: Vec<&str> = snapshot
        .series
        .iter()
        .map(|entry| entry.entity.as_str())
        .collect();
    assert_eq!(
        order,
        [
            "sh.600000",
            "sh.600000",
            "sz.000001",
            "sz.000001",
            "sh.510050",
            "sz.159919",
            "sh.000300"
        ]
    );

    // D-72: the last bar with volume > 0 is the 21st; the zero-volume tail is
    // kept but does not move it. The delisted ETF's termination date is later.
    let live = etfs.iter().find(|e| e.entity == "sh.510050").unwrap();
    assert_eq!(
        live.etf,
        Some(EtfBinding {
            last_trade_date: Some(day(2026, 10, 21)),
            delisting_date: None,
        })
    );
    let delisted = etfs.iter().find(|e| e.entity == "sz.159919").unwrap();
    assert_eq!(
        delisted.etf,
        Some(EtfBinding {
            last_trade_date: Some(day(2026, 10, 21)),
            delisting_date: Some(day(2026, 10, 22)),
        })
    );
    assert_eq!(live.record_count, 3, "the zero-volume bar is not deleted");

    // The dates are part of the identity, on ETF entries only; `null` = unknown.
    let entries = snapshot.identity["series"].as_array().unwrap();
    let entry = |entity: &str| {
        entries
            .iter()
            .find(|e| e["entity"] == entity && e["adjustment"] == "raw")
            .unwrap()
    };
    assert_eq!(entry("sz.159919")["instrument_class"], "etf");
    assert_eq!(entry("sz.159919")["price_scale"], 3);
    assert_eq!(entry("sz.159919")["last_trade_date"], "2026-10-21");
    assert_eq!(entry("sz.159919")["delisting_date"], "2026-10-22");
    assert!(entry("sh.510050")["delisting_date"].is_null());
    assert!(entry("sh.510050").get("delisting_date").is_some());
    for stock in ["sh.600000", "sz.000001", "sh.000300"] {
        assert!(entry(stock).get("last_trade_date").is_none(), "{stock}");
        assert!(entry(stock).get("delisting_date").is_none(), "{stock}");
    }
}

#[test]
fn etf_rows_do_not_change_the_a_share_plan() {
    // AC-M3-2-2: A-share (and index) series entries, their version ids and
    // digests, and their identity values are the same with or without ETFs.
    let plain = plan(&Fixture::new().read(), &ExportRequest::default()).unwrap();
    let with = plan(&Fixture::with_etf().read(), &ExportRequest::default()).unwrap();

    let strip = |snapshot: &SnapshotPlan| -> Vec<SeriesEntry> {
        snapshot
            .series
            .iter()
            .filter(|entry| entry.instrument_class != crate::market_data::INSTRUMENT_CLASS_ETF)
            .cloned()
            .collect()
    };
    assert_eq!(strip(&plain), strip(&with));
    assert_eq!(plain.series.len(), 5);

    let non_etf_values = |snapshot: &SnapshotPlan| -> Vec<serde_json::Value> {
        snapshot.identity["series"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["instrument_class"] != "etf")
            .cloned()
            .collect()
    };
    assert_eq!(non_etf_values(&plain), non_etf_values(&with));

    // The factor sets (stock only) and the calendar are untouched; the
    // coverage count of delisted stock series does not count ETFs.
    let stable = |snapshot: &SnapshotPlan| -> Vec<crate::research::DerivedEntry> {
        snapshot
            .derived
            .iter()
            .filter(|entry| {
                matches!(
                    entry.dataset,
                    crate::research::DerivedDataset::AdjustmentEvents
                        | crate::research::DerivedDataset::TradingCalendar
                )
            })
            .cloned()
            .collect()
    };
    assert_eq!(stable(&plain), stable(&with));
    assert_eq!(plain.coverage, with.coverage);
    assert_eq!(plain.catalog, with.catalog);
}

#[test]
fn qfq_only_exports_exclude_etfs_and_an_explicit_etf_is_refused() {
    let fixture = Fixture::with_etf();
    let qfq = plan(
        &fixture.read(),
        &ExportRequest {
            adjustment: AdjustmentSelection::Qfq,
            ..ExportRequest::default()
        },
    )
    .unwrap();
    assert!(etf_entries(&qfq.series).is_empty());

    // Naming an ETF for a qfq-only export must say so, not return an empty or
    // partial file that reads as "this ETF has no data".
    let error = plan(
        &fixture.read(),
        &ExportRequest {
            dataset: DatasetSelection::Daily,
            adjustment: AdjustmentSelection::Qfq,
            entities: EntitySelection::Explicit(vec!["sh.510050".into()]),
            ..ExportRequest::default()
        },
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("adjustment_unavailable"),
        "{error}"
    );
}

#[test]
fn explicit_entities_select_stock_and_etf_series() {
    let fixture = Fixture::with_etf();
    let snapshot = plan(
        &fixture.read(),
        &ExportRequest {
            dataset: DatasetSelection::Daily,
            entities: EntitySelection::Explicit(vec!["sz.159919".into(), "sh.600000".into()]),
            ..ExportRequest::default()
        },
    )
    .unwrap();
    let order: Vec<(&str, i64)> = snapshot
        .series
        .iter()
        .map(|entry| (entry.entity.as_str(), entry.instrument_class))
        .collect();
    assert_eq!(
        order,
        [("sh.600000", 0), ("sh.600000", 0), ("sz.159919", 2)]
    );

    // A listed ETF with no series is a request error, like a stock without one.
    let error = plan(
        &fixture.read(),
        &ExportRequest {
            dataset: DatasetSelection::Daily,
            entities: EntitySelection::Explicit(vec!["sh.513330".into()]),
            ..ExportRequest::default()
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("EXPORT_NOT_FOUND"), "{error}");
    assert!(error.to_string().contains("sh.513330"), "{error}");
}

#[test]
fn an_etf_series_without_a_master_row_fails_closed() {
    let fixture = Fixture::with_etf();
    fixture
        .write()
        .execute(
            "DELETE FROM security_master WHERE exchange='sh' AND code='510050'",
            [],
        )
        .unwrap();
    let error = plan(&fixture.read(), &ExportRequest::default()).unwrap_err();
    assert!(error.to_string().contains("EXPORT_INTEGRITY"), "{error}");
    assert!(error.to_string().contains("sh.510050"), "{error}");
}

#[test]
fn universe_intervals_give_etfs_both_dates() {
    let fixture = Fixture::with_etf();
    let c = fixture.read();

    // The A-share read model is unchanged: two stock intervals, no ETF dates.
    let stocks = crate::research::universe::member_intervals(&c).unwrap();
    assert_eq!(stocks.len(), 2);
    for interval in &stocks {
        assert_eq!(interval.instrument_class, 0);
        assert_eq!(interval.last_trade_date, None);
        assert_eq!(interval.delisting_date, None);
    }

    let all = crate::research::universe::member_intervals_with_etf(&c).unwrap();
    let rows: Vec<_> = all
        .iter()
        .map(|i| {
            (
                i.entity.as_str(),
                i.instrument_class,
                i.member_from,
                i.member_to,
                i.last_trade_date,
                i.delisting_date,
            )
        })
        .collect();
    assert_eq!(
        rows[..2],
        stocks
            .iter()
            .map(|i| (i.entity.as_str(), 0, i.member_from, i.member_to, None, None))
            .collect::<Vec<_>>()[..]
    );
    assert_eq!(
        rows[2..],
        [
            // Live ETF: open interval, last traded bar is the 21st.
            (
                "sh.510050",
                2,
                day(2026, 10, 20),
                None,
                Some(day(2026, 10, 21)),
                None
            ),
            // Listed, no `.day` member: the identity is there, the dates are
            // unknown (never invented).
            ("sh.513330", 2, day(2026, 10, 20), None, None, None),
            // Delisted: existence ends on the termination date, tradable
            // window ends on the last bar with volume.
            (
                "sz.159919",
                2,
                day(2026, 10, 20),
                Some(day(2026, 10, 22)),
                Some(day(2026, 10, 21)),
                Some(day(2026, 10, 22))
            ),
        ]
    );
}

#[cfg(feature = "export")]
mod export_files {
    use super::*;
    use crate::research::export::{Compression, ExportOptions, export};
    use parquet::file::reader::{FileReader as _, SerializedFileReader};
    use parquet::record::Field;

    type Row = Vec<(String, Field)>;

    fn options(root: &Path) -> ExportOptions {
        ExportOptions {
            out_root: root.to_path_buf(),
            compression: Compression::Snappy,
            row_group_rows: 2,
        }
    }

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

    fn text(row: &Row, column: &str) -> String {
        cell(row, column).to_string()
    }

    fn date(day: NaiveDate) -> Field {
        Field::Date(crate::compact_market::epoch_day(day) as i32)
    }

    fn exported(fixture: &Fixture, out: &Path) -> crate::research::export::ExportOutcome {
        export(fixture.path(), &ExportRequest::default(), &options(out)).unwrap()
    }

    #[test]
    fn daily_raw_carries_etf_rows_with_class_and_no_trade() {
        let fixture = Fixture::with_etf();
        let out = tempdir();
        let outcome = exported(&fixture, out.path());
        let raw = rows_of(&outcome.directory, crate::research::DAILY_RAW_FILE);

        // 2 stocks × 3 bars, then 2 ETFs × 3 bars: the A-share block first.
        assert_eq!(raw.len(), 12);
        let entities: Vec<String> = raw.iter().map(|row| text(row, "entity")).collect();
        assert_eq!(
            entities[..6],
            ["sh.600000"; 3]
                .iter()
                .chain(["sz.000001"; 3].iter())
                .map(|s| s.to_string())
                .collect::<Vec<_>>()[..]
        );
        for row in &raw[..6] {
            assert_eq!(text(row, "instrument_class"), "0");
            assert_eq!(text(row, "no_trade"), "null", "no marker on stock rows");
        }
        for row in &raw[6..] {
            assert_eq!(text(row, "instrument_class"), "2");
            assert_eq!(text(row, "price_scale"), "3");
        }
        // sh.510050: traded, traded, then the zero bar kept as stored.
        let flags: Vec<String> = raw[6..9].iter().map(|row| text(row, "no_trade")).collect();
        assert_eq!(flags, ["false", "false", "true"]);
        assert_eq!(text(&raw[8], "volume"), "0");
        assert!(matches!(cell(&raw[8], "amount"), Field::Decimal(_)));
        let flags: Vec<String> = raw[9..].iter().map(|row| text(row, "no_trade")).collect();
        assert_eq!(flags, ["false", "false", "true"]);
        // 3dp round trip: 2.501 元 is 2_501_000 at the file's 6dp column.
        assert_eq!(entities[6], "sh.510050");
        assert_eq!(
            cell(&raw[6], "close"),
            &Field::Decimal(parquet::data_type::Decimal::from_i64(2_501_000, 18, 6))
        );
        assert_eq!(
            cell(&raw[6], "high"),
            &Field::Decimal(parquet::data_type::Decimal::from_i64(2_504_000, 18, 6))
        );

        // The qfq file has no ETF row and every row is class 0.
        let qfq = rows_of(&outcome.directory, crate::research::DAILY_QFQ_FILE);
        assert_eq!(qfq.len(), 6);
        for row in &qfq {
            assert_eq!(text(row, "instrument_class"), "0");
            assert_eq!(text(row, "no_trade"), "null");
        }
    }

    #[test]
    fn a_share_export_is_unchanged_by_etf_rows() {
        let plain_out = tempdir();
        let etf_out = tempdir();
        let plain = exported(&Fixture::new(), plain_out.path());
        let with = exported(&Fixture::with_etf(), etf_out.path());

        // Files an ETF does not belong in are byte-identical (sha256 equal).
        for name in [
            crate::research::DAILY_QFQ_FILE,
            crate::research::FACTORS_FILE,
            crate::research::CALENDAR_FILE,
            crate::research::INDEX_FILE,
            crate::research::SECURITY_NAMES_FILE,
            crate::research::CAPITAL_EVENTS_FILE,
            crate::research::RISK_WARNINGS_FILE,
            crate::research::SUSPENSIONS_FILE,
            crate::research::PRICE_LIMIT_RULES_FILE,
            crate::research::INDUSTRY_ASSIGNMENTS_FILE,
            crate::research::CODE_MIGRATIONS_FILE,
        ] {
            let sha = |outcome: &crate::research::export::ExportOutcome| {
                outcome
                    .files
                    .iter()
                    .find(|file| file.path == name)
                    .unwrap()
                    .sha256
                    .clone()
            };
            assert_eq!(sha(&plain), sha(&with), "{name} changed");
        }

        // The two files that gained columns keep every A-share row: the
        // stock block is a prefix with the same values in every v3 column.
        let v3_daily = &crate::research::DAILY_COLUMNS[..11];
        let plain_raw = rows_of(&plain.directory, crate::research::DAILY_RAW_FILE);
        let with_raw = rows_of(&with.directory, crate::research::DAILY_RAW_FILE);
        assert_eq!(plain_raw.len(), 6);
        for (before, after) in plain_raw.iter().zip(&with_raw) {
            for column in v3_daily {
                assert_eq!(cell(before, column), cell(after, column), "{column}");
            }
            assert_eq!(before, after, "the new columns are equal on stock rows too");
        }
        let plain_universe = rows_of(&plain.directory, crate::research::UNIVERSE_INTERVALS_FILE);
        let with_universe = rows_of(&with.directory, crate::research::UNIVERSE_INTERVALS_FILE);
        assert_eq!(plain_universe.len(), 2);
        assert_eq!(plain_universe[..], with_universe[..2]);
    }

    #[test]
    fn universe_file_has_class_and_both_etf_dates() {
        let out = tempdir();
        let outcome = exported(&Fixture::with_etf(), out.path());
        let rows = rows_of(&outcome.directory, crate::research::UNIVERSE_INTERVALS_FILE);
        assert_eq!(rows.len(), 5);
        let delisted = rows
            .iter()
            .find(|row| text(row, "entity") == "sz.159919")
            .unwrap();
        assert_eq!(text(delisted, "instrument_class"), "2");
        assert_eq!(cell(delisted, "member_to"), &date(day(2026, 10, 22)));
        assert_eq!(cell(delisted, "last_trade_date"), &date(day(2026, 10, 21)));
        assert_eq!(cell(delisted, "delisting_date"), &date(day(2026, 10, 22)));
        let absent = rows
            .iter()
            .find(|row| text(row, "entity") == "sh.513330")
            .unwrap();
        assert_eq!(cell(absent, "last_trade_date"), &Field::Null);
        assert_eq!(cell(absent, "delisting_date"), &Field::Null);
        let stock = rows
            .iter()
            .find(|row| text(row, "entity") == "sh.600000")
            .unwrap();
        assert_eq!(text(stock, "instrument_class"), "0");
        assert_eq!(cell(stock, "last_trade_date"), &Field::Null);
        assert_eq!(cell(stock, "delisting_date"), &Field::Null);
    }

    #[test]
    fn manifest_lists_the_v4_columns_and_the_etf_limitations() {
        let out = tempdir();
        let outcome = exported(&Fixture::with_etf(), out.path());
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(outcome.directory.join("manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["schema"], "x-data-mcp.research-snapshot.v4");

        let columns = |file: &str| -> Vec<String> {
            manifest["files"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["path"] == file)
                .unwrap()["columns"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c.as_str().unwrap().to_string())
                .collect()
        };
        for file in [
            crate::research::DAILY_RAW_FILE,
            crate::research::DAILY_QFQ_FILE,
        ] {
            let listed = columns(file);
            assert_eq!(listed.len(), 13, "{file}");
            assert_eq!(listed[..11], crate::research::DAILY_COLUMNS[..11]);
            assert_eq!(listed[11..], ["instrument_class", "no_trade"]);
        }
        let universe = columns(crate::research::UNIVERSE_INTERVALS_FILE);
        assert_eq!(universe.len(), 10);
        assert_eq!(
            universe[7..],
            ["instrument_class", "last_trade_date", "delisting_date"]
        );

        let limitations: Vec<&str> = manifest["limitations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        for needle in ["no_trade is derived", "no ETF 前复权", "survivor bias"] {
            assert!(
                limitations.iter().any(|text| text.contains(needle)),
                "missing ETF limitation {needle:?}"
            );
        }
    }
}
