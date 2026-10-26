//! Index benchmark storage tests (issue-383 §6.3, AC-M1-6): registry ↔ stored
//! row consistency, the unit/scope evidence gate, `r7_raw` preservation,
//! alias round-trip, and the DDL guards.
use std::collections::BTreeSet;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde_json::json;

use super::*;
use crate::compact_market::PRICE_SCALE_CENTS;
use crate::market_data::{
    Adjustment, AdjustmentEvidence, Bar, CoverageEvidence, EvidenceValue, MarketDataBatch,
    Provenance, UnitCurrencyEvidence,
};
use crate::market_storage::MarketRepository;
use crate::sqlite_market::compact_repo::SqliteCompactMarket;
use crate::task_control::{self, CommandKey, ItemSpec, TaskRepository};

const RUN: &str = "run";
const ITEM: &str = "item-1";

fn tempdir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/dev/shm")
        .unwrap()
}

fn repo(dir: &tempfile::TempDir) -> (PathBuf, SqliteCompactMarket) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.path().join("market.db");
    crate::sqlite_tasks::SqliteTasks::new(&path)
        .initialize_compact()
        .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let repo = SqliteCompactMarket::open(&path).unwrap();
    (path, repo)
}

fn ts() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-26T00:00:00+00:00")
        .unwrap()
        .with_timezone(&Utc)
}

/// Index units exactly as the registry declares them (0.01 点 / 手 / 元).
fn index_units() -> UnitCurrencyEvidence {
    UnitCurrencyEvidence {
        price_currency: EvidenceValue::unknown("an index level is a point value"),
        price_unit: EvidenceValue::confirmed("index_point", "bound"),
        volume_unit: EvidenceValue::confirmed("hand", "bound"),
        amount_currency: EvidenceValue::confirmed("CNY", "bound"),
        amount_unit: EvidenceValue::confirmed("yuan", "bound"),
    }
}

/// One 0.01-点 index bar with a caller-chosen `r7_raw` (indices never
/// interpret it; the test pins "stored verbatim").
fn bar(day: i64, point_cents: i64, r7: u32) -> Bar {
    crate::compact_market::CompactBar {
        trade_day: day,
        open_units: point_cents,
        high_units: point_cents + 10,
        low_units: point_cents - 10,
        close_units: point_cents,
        volume: Some(1234),
        amount: Some("5678.9".into()),
        r7_raw: Some(r7),
    }
    .to_bar(Period::Day, PRICE_SCALE_CENTS)
    .unwrap()
}

fn index_batch(symbol: &str, bars: Vec<Bar>) -> IndexBatch {
    IndexBatch {
        batch: MarketDataBatch {
            symbol: symbol.into(),
            period: Period::Day,
            adjustment: Adjustment::Raw,
            bars,
            provenance: Provenance {
                source: "official-index".into(),
                source_period: "day".into(),
                source_adjustment: "raw".into(),
                symbol: symbol.into(),
                acquired_at: ts(),
                adapter_version: "fixture".into(),
                yearly_derivation: None,
                snapshot_inputs: None,
            },
            coverage: CoverageEvidence::complete("complete fixture history"),
            unit_currency: index_units(),
            adjustment_evidence: AdjustmentEvidence::confirmed("raw", "no corporate action"),
        },
        day_member_sha256: "0c".repeat(32),
        security_event_sha256: "0e".repeat(32),
    }
}

/// Accept + claim + start a one-item daily task bound to `symbol` — the same
/// task machinery the production runner drives, so the commit's item gates
/// are real.
fn running_task(repo: &SqliteCompactMarket, request: &str, symbol: &str) -> (String, u64) {
    let scope = task_control::legacy::scope_id(symbol, "day");
    let work = task_control::FrozenWork {
        items: vec![ItemSpec {
            item_id: ITEM.into(),
            target: json!({"symbol": symbol, "period": "day"}),
            progress_unit: "bars".into(),
            frozen_inputs: json!({}),
            scope_keys: BTreeSet::from([scope.clone()]),
        }],
        scopes: BTreeSet::from([scope]),
        shared_evidence: None,
    };
    let command = task_control::Command {
        key: CommandKey {
            operation: "update_data".into(),
            dataset: "market.daily".into(),
            contract_version: crate::market_update::tasks::CONTRACT_VERSION.into(),
        },
        intent: json!({}),
    };
    let acceptance = repo.accept(request, &command, &work, RUN, None).unwrap();
    let task_id = match acceptance.record.outcome {
        task_control::RequestOutcome::Task(snapshot) => snapshot.task.task_id.clone(),
        _ => panic!("expected task outcome"),
    };
    let revision = repo.task(&task_id).unwrap().task.state_revision;
    let revision = repo
        .transition(&task_id, revision, RUN, task_control::Change::Claim)
        .unwrap()
        .task
        .state_revision;
    let revision = repo
        .transition(
            &task_id,
            revision,
            RUN,
            task_control::Change::StartItem(ITEM.into()),
        )
        .unwrap()
        .task
        .state_revision;
    (task_id, revision)
}

/// Commit one index write inside a fresh task.
fn commit(
    repo: &SqliteCompactMarket,
    request: &str,
    index: &IndexBatch,
) -> crate::market_storage::Receipt {
    let (task_id, revision) = running_task(repo, request, &index.batch.symbol);
    let generation_evidence = json!({
        "manifest_sha256": "0b".repeat(32),
        "snapshot_inputs": [],
    });
    repo.commit_index(&IndexWrite {
        index,
        result_id: &format!("result-{request}"),
        task_id: &task_id,
        item_id: ITEM,
        run_id: RUN,
        expected_task_revision: revision,
        generation_id: "gen-1",
        generation_manifest_sha256: &"0b".repeat(32),
        generation_evidence: &generation_evidence,
        published_at: ts(),
    })
    .unwrap()
}

#[test]
fn registry_matches_the_stored_series_and_metadata() {
    let dir = tempdir();
    let (path, repo) = repo(&dir);
    for (index, registration) in crate::index_registry::INDEX_SERIES.iter().enumerate() {
        let bars = vec![bar(0, 300_000, 28_641_085), bar(1, 301_000, 28_641_086)];
        let batch = index_batch(registration.entity, bars.clone());
        commit(&repo, &format!("req-{index}"), &batch);

        let c = Connection::open(&path).unwrap();
        let series =
            series::read_series_adjusted(&c, registration.entity, Period::Day, Adjustment::Raw)
                .unwrap()
                .expect("committed index series");
        // §6.3: an index is a plain bar series with class=1, 口径=0, 0.01 点.
        assert_eq!(
            series.instrument_class,
            crate::market_data::INSTRUMENT_CLASS_INDEX
        );
        assert_eq!(series.adjustment, Adjustment::Raw);
        assert_eq!(series.price_scale, PRICE_SCALE_CENTS);
        assert_eq!(series.period, Period::Day);
        assert_eq!(series.record_count, 2);
        // No forward series and no yearly series exist for an index.
        assert!(
            series::read_series_adjusted(&c, registration.entity, Period::Day, Adjustment::Forward)
                .unwrap()
                .is_none()
        );
        assert!(
            series::read_series_adjusted(
                &c,
                registration.entity,
                Period::Year,
                Adjustment::Forward
            )
            .unwrap()
            .is_none()
        );

        let meta = read_meta_by_entity(&c, registration.entity)
            .unwrap()
            .expect("index metadata row");
        assert_eq!(meta.series_key, series.series_key);
        assert_eq!(meta.aliases, registration.aliases_text());
        assert_eq!(
            IndexRegistration::parse_aliases(&meta.aliases),
            registration.aliases
        );
        assert_eq!(meta.aggregate_scope, registration.aggregate_scope.as_str());
        assert_eq!(meta.member_count, registration.member_count);
        assert_eq!(meta.price_unit, registration.price_unit);
        assert_eq!(meta.volume_unit, registration.volume_unit);
        assert_eq!(meta.amount_unit, registration.amount_unit);
        assert_eq!(meta.evidence_note, registration.evidence_note);
        // The metadata points at a `derived` evidence bundle that records how
        // the unit/scope claim was proven (§6.3).
        let (kind, canonical) = evidence_store::read_evidence(&c, &meta.evidence_digest).unwrap();
        assert_eq!(kind, crate::compact_market::EvidenceKind::Derived);
        let evidence: serde_json::Value = serde_json::from_str(&canonical).unwrap();
        assert_eq!(evidence["entity"], registration.entity);
        assert_eq!(
            evidence["aggregate_scope"],
            registration.aggregate_scope.as_str()
        );
        assert_eq!(evidence["corporate_action_set"], "empty");
        assert_eq!(evidence["day_member_sha256"], "0c".repeat(32));
        assert_eq!(evidence["security_event_sha256"], "0e".repeat(32));
        assert_eq!(
            evidence["evidence_note"].as_str().unwrap(),
            registration.evidence_note
        );
        // The version evidence is a normal `version` bundle (the series row
        // contract), not the derived unit/scope proof.
        let (version_kind, _) = evidence_store::read_evidence(&c, &series.evidence_digest).unwrap();
        assert_eq!(version_kind, crate::compact_market::EvidenceKind::Version);
    }
    // The registry is exactly the stored set: 9 series rows, 9 metadata rows.
    let c = Connection::open(&path).unwrap();
    let (series_count, meta_count): (i64, i64) = c
        .query_row(
            "SELECT (SELECT count(*) FROM market_series WHERE instrument_class=1),\
             (SELECT count(*) FROM index_series_meta)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((series_count, meta_count), (9, 9));
}

#[test]
fn r7_raw_is_stored_verbatim() {
    let dir = tempdir();
    let (path, repo) = repo(&dir);
    let batch = index_batch("sh.000001", vec![bar(0, 300_000, 28_641_085)]);
    commit(&repo, "req-r7", &batch);
    let c = Connection::open(&path).unwrap();
    let stored = c
        .query_row(
            "SELECT b.r7_raw FROM series_bars b JOIN market_series s \
             ON s.series_key=b.series_key WHERE s.entity='sh.000001'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(stored, 28_641_085);
    // The bar reads back through the series scale (0.01 点), not 分 or 6dp.
    let series = series::read_series_adjusted(&c, "sh.000001", Period::Day, Adjustment::Raw)
        .unwrap()
        .unwrap();
    let bars = series::all_bars(&c, series.series_key).unwrap();
    assert_eq!(bars[0].r7_raw, Some(28_641_085));
    assert_eq!(bars[0].open_units, 300_000);
}

#[test]
fn aliases_round_trip_through_the_stored_text() {
    let dir = tempdir();
    let (path, repo) = repo(&dir);
    let batch = index_batch("sh.000300", vec![bar(0, 350_000, 7)]);
    commit(&repo, "req-alias", &batch);
    let c = Connection::open(&path).unwrap();
    let meta = read_meta_by_entity(&c, "sh.000300").unwrap().unwrap();
    assert_eq!(meta.aliases, "sz.399300");
    // The alternate code is not a second stored series: one content, one
    // version id (§6.1).
    assert!(
        series::read_series_adjusted(&c, "sz.399300", Period::Day, Adjustment::Raw)
            .unwrap()
            .is_none()
    );
}

#[test]
fn unregistered_entities_are_rejected() {
    let dir = tempdir();
    let (_path, repo) = repo(&dir);
    let batch = index_batch("sh.600006", vec![bar(0, 100, 7)]);
    let (task_id, revision) = running_task(&repo, "req-unregistered", "sh.600006");
    let generation_evidence = json!({"manifest_sha256": "0b".repeat(32)});
    let error = repo
        .commit_index(&IndexWrite {
            index: &batch,
            result_id: "result-unregistered",
            task_id: &task_id,
            item_id: ITEM,
            run_id: RUN,
            expected_task_revision: revision,
            generation_id: "gen-1",
            generation_manifest_sha256: &"0b".repeat(32),
            generation_evidence: &generation_evidence,
            published_at: ts(),
        })
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("not a registered index benchmark"),
        "{error}"
    );
}

#[test]
fn missing_unit_evidence_is_rejected() {
    // Each case gets its own store: a rejected commit leaves its task item
    // running and holding the scope, so reusing the entity would conflict on
    // occupancy instead of reaching the unit gate.
    for (label, unit) in [
        (
            "price_unit",
            UnitCurrencyEvidence {
                price_unit: EvidenceValue::unknown("no proof"),
                ..index_units()
            },
        ),
        (
            "volume_unit",
            UnitCurrencyEvidence {
                // The stock unit would be wrong for an index (§6.2).
                volume_unit: EvidenceValue::confirmed("share", "stock rule"),
                ..index_units()
            },
        ),
        (
            "amount_unit",
            UnitCurrencyEvidence {
                amount_unit: EvidenceValue::unknown("no proof"),
                ..index_units()
            },
        ),
    ] {
        let dir = tempdir();
        let (path, repo) = repo(&dir);
        let mut batch = index_batch("sh.000016", vec![bar(0, 200_000, 7)]);
        batch.batch.unit_currency = unit;
        let (task_id, revision) = running_task(&repo, &format!("req-unit-{label}"), "sh.000016");
        let generation_evidence = json!({"manifest_sha256": "0b".repeat(32)});
        let error = repo
            .commit_index(&IndexWrite {
                index: &batch,
                result_id: &format!("result-unit-{label}"),
                task_id: &task_id,
                item_id: ITEM,
                run_id: RUN,
                expected_task_revision: revision,
                generation_id: "gen-1",
                generation_manifest_sha256: &"0b".repeat(32),
                generation_evidence: &generation_evidence,
                published_at: ts(),
            })
            .unwrap_err();
        assert!(error.to_string().contains(label), "{label}: {error}");
        // The rejected commit wrote no series, no metadata and no event.
        let c = Connection::open(&path).unwrap();
        let counts: (i64, i64, i64) = c
            .query_row(
                "SELECT (SELECT count(*) FROM market_series),\
                 (SELECT count(*) FROM index_series_meta),\
                 (SELECT count(*) FROM market_events)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(counts, (0, 0, 0), "{label}");
    }
}

#[test]
fn second_commit_is_no_change_and_append_extends() {
    let dir = tempdir();
    let (path, repo) = repo(&dir);
    let bars = vec![bar(0, 300_000, 7), bar(1, 301_000, 8)];
    let first = commit(&repo, "req-1", &index_batch("sh.000001", bars.clone()));
    let again = commit(&repo, "req-2", &index_batch("sh.000001", bars.clone()));
    assert_eq!(again.version_id, first.version_id);
    let extended = commit(
        &repo,
        "req-3",
        &index_batch(
            "sh.000001",
            vec![bar(0, 300_000, 7), bar(1, 301_000, 8), bar(2, 302_000, 9)],
        ),
    );
    assert_ne!(extended.version_id, first.version_id);
    let c = Connection::open(&path).unwrap();
    let kinds: Vec<String> = c
        .prepare("SELECT kind FROM market_events WHERE entity='sh.000001' ORDER BY seq")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(kinds, vec!["first_publish", "no_change", "append"]);
    let series = series::read_series_adjusted(&c, "sh.000001", Period::Day, Adjustment::Raw)
        .unwrap()
        .unwrap();
    assert_eq!(series.record_count, 3);
    // The metadata row survives re-commits (created_at is preserved).
    assert!(read_meta_by_entity(&c, "sh.000001").unwrap().is_some());
}

#[test]
fn ddl_rejects_metadata_without_evidence() {
    let dir = tempdir();
    let (path, repo) = repo(&dir);
    commit(
        &repo,
        "req-ddl",
        &index_batch("sh.000001", vec![bar(0, 300_000, 7)]),
    );
    let c = Connection::open(&path).unwrap();
    let series_key: i64 = c
        .query_row(
            "SELECT series_key FROM market_series WHERE entity='sh.000001'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let evidence: Vec<u8> = c
        .query_row(
            "SELECT evidence_digest FROM index_series_meta WHERE series_key=?1",
            [series_key],
            |r| r.get(0),
        )
        .unwrap();
    let insert = |c: &Connection, key: i64, scope: &str, unit: &str, note: &str, count: i64| {
        // The series_key PRIMARY KEY would make a second row fail regardless
        // of the CHECKs, so each case starts from "no row" and must fail for
        // its own reason.
        c.execute("DELETE FROM index_series_meta WHERE series_key=?1", [key])
            .unwrap();
        c.execute(
            "INSERT INTO index_series_meta(series_key,aliases,aggregate_scope,member_count,\
             price_unit,volume_unit,amount_unit,evidence_digest,evidence_note,created_at) \
             VALUES(?1,'',?2,?3,?4,'hand','yuan',?5,?6,'now')",
            rusqlite::params![key, scope, count, unit, evidence.as_slice(), note],
        )
    };
    // A second row for the same series is the only way to exercise the DDL
    // on an existing schema; every attempt must fail.
    for (scope, unit, note, count) in [
        ("constituents", "index_point", "", 300),
        ("all_market", "index_point", "note", 300),
        ("constituents", "yuan", "note", 300),
        ("constituents", "index_point", "note", 0),
    ] {
        assert!(
            insert(&c, series_key, scope, unit, note, count).is_err(),
            "accepted scope={scope} unit={unit} note={note:?} count={count}"
        );
    }
}

/// Accept + claim + start a one-item task with an explicit item shape — the
/// internal `derived:index:*` items name their dataset instead of an
/// entity/period pair (issue-383 §9.2).
fn running_item(
    repo: &SqliteCompactMarket,
    request: &str,
    item_id: &str,
    target: serde_json::Value,
    scope: &str,
) -> (String, u64) {
    let work = task_control::FrozenWork {
        items: vec![ItemSpec {
            item_id: item_id.into(),
            target,
            progress_unit: "derived_dataset".into(),
            frozen_inputs: json!({}),
            scope_keys: BTreeSet::from([scope.to_string()]),
        }],
        scopes: BTreeSet::from([scope.to_string()]),
        shared_evidence: None,
    };
    let command = task_control::Command {
        key: CommandKey {
            operation: "update_data".into(),
            dataset: "market.daily".into(),
            contract_version: crate::market_update::tasks::CONTRACT_VERSION.into(),
        },
        intent: json!({}),
    };
    let acceptance = repo.accept(request, &command, &work, RUN, None).unwrap();
    let task_id = match acceptance.record.outcome {
        task_control::RequestOutcome::Task(snapshot) => snapshot.task.task_id.clone(),
        _ => panic!("expected task outcome"),
    };
    let revision = repo.task(&task_id).unwrap().task.state_revision;
    let revision = repo
        .transition(&task_id, revision, RUN, task_control::Change::Claim)
        .unwrap()
        .task
        .state_revision;
    let revision = repo
        .transition(
            &task_id,
            revision,
            RUN,
            task_control::Change::StartItem(item_id.into()),
        )
        .unwrap()
        .task
        .state_revision;
    (task_id, revision)
}

fn commit_derived(
    repo: &SqliteCompactMarket,
    task_id: &str,
    item_id: &str,
    revision: u64,
    index: &IndexBatch,
    result_id: &str,
) -> Result<crate::market_storage::Receipt, crate::market_storage::Error> {
    let generation_evidence = json!({
        "manifest_sha256": "0b".repeat(32),
        "snapshot_inputs": [],
    });
    repo.commit_index(&IndexWrite {
        index,
        result_id,
        task_id,
        item_id,
        run_id: RUN,
        expected_task_revision: revision,
        generation_id: "gen-1",
        generation_manifest_sha256: &"0b".repeat(32),
        generation_evidence: &generation_evidence,
        published_at: ts(),
    })
}

// issue-383 §9.2: the server-created index item carries its dataset as the
// target; the commit gate must accept exactly that shape for the entity in
// the item id, and keep rejecting every other target/series combination.
#[test]
fn derived_index_item_target_binds_its_own_entity_only() {
    let dir = tempdir();
    let (_path, repo) = repo(&dir);
    let symbol = "sh.000001";
    let item_id = task_control::derived::index(symbol);
    let (task_id, revision) = running_item(
        &repo,
        "derived-ok",
        &item_id,
        task_control::derived::target(&item_id),
        &task_control::derived::index_scope(symbol),
    );
    let batch = index_batch(symbol, vec![bar(0, 300_000, 7)]);
    commit_derived(
        &repo,
        &task_id,
        &item_id,
        revision,
        &batch,
        "result-derived-ok",
    )
    .unwrap();
    let task = repo.task(&task_id).unwrap();
    assert_eq!(task.task.status, task_control::Status::Succeeded);
    assert!(task.items[0].outcome.as_ref().is_some_and(|o| o.success));

    // The id names sh.000016 but the batch writes sh.000905: neither the
    // dataset target nor the occupancy key matches, so the commit fails
    // closed instead of writing another entity's series.
    let wrong_id = task_control::derived::index("sh.000016");
    let (task_id, revision) = running_item(
        &repo,
        "derived-wrong-entity",
        &wrong_id,
        task_control::derived::target(&wrong_id),
        &task_control::derived::index_scope("sh.000016"),
    );
    let other = index_batch("sh.000905", vec![bar(0, 300_000, 8)]);
    let error = commit_derived(
        &repo,
        &task_id,
        &wrong_id,
        revision,
        &other,
        "result-derived-wrong",
    )
    .unwrap_err();
    assert!(matches!(error, crate::market_storage::Error::Conflict(_)));

    // A security item (plain scope-id) may not borrow the dataset target.
    let security = task_control::legacy::scope_id("sh.000001", "day");
    let (task_id, revision) = running_item(
        &repo,
        "derived-on-security-item",
        &security,
        task_control::derived::target(&task_control::derived::index(symbol)),
        &security,
    );
    let batch = index_batch(symbol, vec![bar(0, 300_000, 9)]);
    let error = commit_derived(
        &repo,
        &task_id,
        &security,
        revision,
        &batch,
        "result-derived-security-item",
    )
    .unwrap_err();
    assert!(matches!(error, crate::market_storage::Error::Conflict(_)));
}
