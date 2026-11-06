//! Delisted codes that are not in the catalog (survivor-bias fix, Refs #446
//! §10.4 / #383).
//!
//! The fixture is [`Fixture::with_delisted_off_catalog`]: the default two-stock
//! fixture plus `sh.600001` and `sz.000003`, both delisted, both absent from
//! the catalog head, and both carrying a series, a factor set and a row in
//! every whole-market file. The older fixtures use a delisted code that is
//! also a catalog member, which is why the catalog-only selection was never
//! caught.

use super::*;
use crate::research::SnapshotPlan;

const LISTED: [&str; 2] = ["sh.600000", "sz.000001"];

fn entities(snapshot: &SnapshotPlan, adjustment: Adjustment) -> Vec<&str> {
    snapshot
        .series
        .iter()
        .filter(|entry| {
            entry.instrument_class == crate::market_data::INSTRUMENT_CLASS_STOCK
                && entry.adjustment == adjustment
        })
        .map(|entry| entry.entity.as_str())
        .collect()
}

fn factor_keys(snapshot: &SnapshotPlan) -> Vec<&str> {
    snapshot
        .derived
        .iter()
        .filter(|entry| entry.dataset == crate::research::DerivedDataset::AdjustmentEvents)
        .map(|entry| entry.key.as_str())
        .collect()
}

#[test]
fn the_fixture_delisted_codes_are_not_catalog_members() {
    let fixture = Fixture::with_delisted_off_catalog();
    let c = fixture.read();
    let in_catalog: i64 = c
        .query_row(
            "SELECT count(*) FROM catalog_members WHERE code IN ('600001','000003')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(in_catalog, 0, "the premise of every test in this file");
}

#[test]
fn the_default_selection_carries_delisted_series_and_factor_sets() {
    let fixture = Fixture::with_delisted_off_catalog();
    let snapshot = plan(&fixture.read(), &ExportRequest::default()).unwrap();

    // Both 口径 for all four stocks, in entity order; the index stays last.
    let expected = ["sh.600000", "sh.600001", "sz.000001", "sz.000003"];
    assert_eq!(entities(&snapshot, Adjustment::Raw), expected);
    assert_eq!(entities(&snapshot, Adjustment::Forward), expected);
    assert_eq!(factor_keys(&snapshot), expected);
    assert_eq!(snapshot.series.len(), 9, "4 stocks × 2 口径 + 1 index");

    // `catalog.member_count` keeps its meaning (catalog members); the manifest
    // schema is unchanged.
    assert_eq!(snapshot.catalog.selection, "catalog");
    assert_eq!(snapshot.catalog.member_count, 2);
    // 1 delisted catalog member + 2 delisted codes outside the catalog.
    assert_eq!(snapshot.coverage.delisted_series, 3);
    assert_eq!(
        snapshot.identity["coverage"]["delisted_series"],
        json!(3),
        "the identity carries the corrected count"
    );
}

#[test]
fn listed_series_are_unchanged_and_the_snapshot_id_moves() {
    let plain = plan(&Fixture::new().read(), &ExportRequest::default()).unwrap();
    let wide = plan(
        &Fixture::with_delisted_off_catalog().read(),
        &ExportRequest::default(),
    )
    .unwrap();
    let keep = |snapshot: &SnapshotPlan| -> Vec<crate::research::SeriesEntry> {
        snapshot
            .series
            .iter()
            .filter(|entry| LISTED.contains(&entry.entity.as_str()))
            .cloned()
            .collect()
    };
    let listed = keep(&plain);
    assert_eq!(listed.len(), 4);
    assert_eq!(
        listed,
        keep(&wide),
        "current members' entries do not change"
    );
    assert_ne!(
        plain.snapshot_id, wide.snapshot_id,
        "the series list is part of the identity, so the id changes with it"
    );
}

#[test]
fn explicit_entities_behave_as_before() {
    let fixture = Fixture::with_delisted_off_catalog();
    let explicit = |list: &[&str]| ExportRequest {
        entities: EntitySelection::Explicit(list.iter().map(|e| e.to_string()).collect()),
        ..ExportRequest::default()
    };
    // Naming a delisted code works (it always did) and selects only that code.
    let one = plan(&fixture.read(), &explicit(&["sh.600001"])).unwrap();
    assert_eq!(entities(&one, Adjustment::Raw), ["sh.600001"]);
    assert_eq!(factor_keys(&one), ["sh.600001"]);
    assert_eq!(one.catalog.selection, "explicit");
    assert_eq!(one.coverage.delisted_series, 1);
    // A listed code alone is not widened.
    let listed = plan(&fixture.read(), &explicit(&["sh.600000"])).unwrap();
    assert_eq!(entities(&listed, Adjustment::Raw), ["sh.600000"]);
    assert_eq!(listed.coverage.delisted_series, 0);
}

#[test]
fn a_pinned_catalog_version_is_exactly_that_catalog() {
    let fixture = Fixture::with_delisted_off_catalog();
    let pinned = plan(
        &fixture.read(),
        &ExportRequest {
            entities: EntitySelection::CatalogVersion("catalog-1".into()),
            ..ExportRequest::default()
        },
    )
    .unwrap();
    // The pin is a request for that stored catalog, so it is survivor-biased
    // by construction and the manifest names it (`catalog_version`).
    assert_eq!(entities(&pinned, Adjustment::Raw), LISTED);
    assert_eq!(factor_keys(&pinned), LISTED);
    assert_eq!(pinned.catalog.selection, "catalog_version");
    assert_eq!(pinned.catalog.member_count, 2);
    assert_eq!(pinned.coverage.delisted_series, 1);
}

#[cfg(feature = "export")]
mod export_files {
    use super::*;
    use crate::research::export::{Compression, ExportOptions, dry_run, export};
    use parquet::file::reader::{FileReader as _, SerializedFileReader};
    use parquet::record::Field;
    use std::collections::BTreeSet;

    fn options(root: &Path) -> ExportOptions {
        ExportOptions {
            out_root: root.to_path_buf(),
            compression: Compression::Snappy,
            row_group_rows: 2,
        }
    }

    /// The text of `column` in every row of a snapshot file.
    fn column(directory: &Path, name: &str, column: &str) -> Vec<String> {
        let reader =
            SerializedFileReader::new(std::fs::File::open(directory.join(name)).unwrap()).unwrap();
        reader
            .get_row_iter(None)
            .unwrap()
            .map(|row| {
                let row = row.unwrap();
                let (_, field) = row
                    .get_column_iter()
                    .find(|(name, _)| name.as_str() == column)
                    .unwrap_or_else(|| panic!("{name} has no column {column}"));
                match field {
                    Field::Str(value) => value.clone(),
                    other => other.to_string(),
                }
            })
            .collect()
    }

    /// The entity a row of a split-key file (`exchange`, `code`) belongs to.
    fn split_entities(directory: &Path, name: &str) -> BTreeSet<String> {
        column(directory, name, "exchange")
            .into_iter()
            .zip(column(directory, name, "code"))
            .map(|(exchange, code)| format!("{exchange}.{code}"))
            .collect()
    }

    fn set(values: impl IntoIterator<Item = String>) -> BTreeSet<String> {
        values.into_iter().collect()
    }

    #[test]
    fn dry_run_lists_the_delisted_series_and_counts_them() {
        let fixture = Fixture::with_delisted_off_catalog();
        let out = tempdir();
        let (snapshot, manifest) = dry_run(
            fixture.path(),
            &ExportRequest::default(),
            &options(out.path()),
        )
        .unwrap();
        assert_eq!(manifest["snapshot_id"], snapshot.snapshot_id);
        assert_eq!(manifest["coverage"]["delisted_series"], json!(3));
        assert_eq!(manifest["catalog"]["member_count"], json!(2));
        let listed: BTreeSet<String> = manifest["series"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["adjustment"] == "raw")
            .map(|entry| entry["entity"].as_str().unwrap().to_string())
            .collect();
        for entity in OFF_CATALOG_DELISTED {
            assert!(listed.contains(entity), "{entity} is missing from series");
        }
        assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 0);
    }

    #[test]
    fn every_per_entity_file_covers_the_delisted_codes() {
        let fixture = Fixture::with_delisted_off_catalog();
        let out = tempdir();
        let outcome = export(
            fixture.path(),
            &ExportRequest::default(),
            &options(out.path()),
        )
        .unwrap();
        let dir = &outcome.directory;
        let daily = |file: &str| set(column(dir, file, "entity"));
        let stocks = || set(["sh.600000", "sh.600001", "sz.000001", "sz.000003"].map(String::from));

        // Series-keyed files: the delisted codes have rows, 3 bars each.
        let raw = column(dir, crate::research::DAILY_RAW_FILE, "entity");
        for entity in OFF_CATALOG_DELISTED {
            assert_eq!(
                raw.iter().filter(|e| e.as_str() == entity).count(),
                3,
                "{entity} bars in daily_raw"
            );
        }
        assert_eq!(raw.len(), 12, "4 stocks × 3 bars");
        assert_eq!(set(raw.iter().cloned()), stocks());
        assert_eq!(daily(crate::research::DAILY_QFQ_FILE), stocks());
        assert_eq!(daily(crate::research::FACTORS_FILE), stocks());

        // Whole-table files carry every entity the tables hold; each must
        // contain every exported series entity (series ⊆ file).
        let in_file = |file: &str| -> BTreeSet<String> {
            if file == crate::research::UNIVERSE_INTERVALS_FILE
                || file == crate::research::INDUSTRY_ASSIGNMENTS_FILE
            {
                set(column(dir, file, "entity"))
            } else {
                split_entities(dir, file)
            }
        };
        let must_have = |file: &str, expected: &[&str]| {
            let found = in_file(file);
            for entity in expected {
                assert!(found.contains(*entity), "{file} lacks {entity}: {found:?}");
            }
        };
        must_have(
            crate::research::SECURITY_MASTER_FILE,
            &["sh.600001", "sz.000003"],
        );
        must_have(
            crate::research::UNIVERSE_INTERVALS_FILE,
            &["sh.600001", "sz.000003"],
        );
        must_have(
            crate::research::CAPITAL_EVENTS_FILE,
            &["sh.600001", "sz.000003"],
        );
        must_have(
            crate::research::RISK_WARNINGS_FILE,
            &["sh.600001", "sz.000003"],
        );
        must_have(
            crate::research::SUSPENSIONS_FILE,
            &["sh.600001", "sz.000003"],
        );
        must_have(
            crate::research::INDUSTRY_ASSIGNMENTS_FILE,
            &["sh.600001", "sz.000003"],
        );
        // Names exist for the one SZ code the fixture gives a dated history
        // (SH has none, D-61): an absent row is "not observed", not a drop.
        must_have(crate::research::SECURITY_NAMES_FILE, &["sz.000003"]);
        assert!(
            !in_file(crate::research::SECURITY_NAMES_FILE).contains("sh.600001"),
            "SH names stay NOT_AVAILABLE"
        );
    }

    #[test]
    fn the_delisted_universe_and_master_rows_are_closed() {
        let fixture = Fixture::with_delisted_off_catalog();
        let out = tempdir();
        let outcome = export(
            fixture.path(),
            &ExportRequest::default(),
            &options(out.path()),
        )
        .unwrap();
        let dir = &outcome.directory;
        let entities = column(dir, crate::research::UNIVERSE_INTERVALS_FILE, "entity");
        let ends = column(dir, crate::research::UNIVERSE_INTERVALS_FILE, "member_to");
        for entity in OFF_CATALOG_DELISTED {
            let index = entities.iter().position(|e| e == entity).unwrap();
            assert_ne!(ends[index], "null", "{entity} has a closed interval");
        }
    }

    #[test]
    fn the_export_is_byte_reproducible_with_delisted_series() {
        let fixture = Fixture::with_delisted_off_catalog();
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
        assert_eq!(first.snapshot_id, second.snapshot_id);
        assert_eq!(first.files, second.files);
        for file in &first.files {
            let a = std::fs::read(first.directory.join(&file.path)).unwrap();
            let b = std::fs::read(second.directory.join(&file.path)).unwrap();
            assert_eq!(a, b, "{} differs between two exports", file.path);
        }
        let a = std::fs::read(first.directory.join("manifest.json")).unwrap();
        let b = std::fs::read(second.directory.join("manifest.json")).unwrap();
        assert_eq!(a, b, "manifest.json differs");
    }
}
