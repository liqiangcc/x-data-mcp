//! N3: stale/invalid read surfaces exercised through the real MCP handler
//! (`DataApi::call`), not just storage functions — the wire shape is the
//! contract: `stale_version` for superseded pins, `version_not_found` for
//! never-committed ids, `stale:"behind"`+`as_of_day` marks, and
//! absent-with-reason rows for invalidated yearly content.
use std::collections::BTreeSet;

use rusqlite::Connection;
use serde_json::{Value, json};
use tempfile::TempDir;

use super::DataApi;
use crate::data_supply::Access;
use crate::market_data::{
    Adjustment, AdjustmentEvidence, Bar, CoverageEvidence, MarketDataBatch, Period, Provenance,
    UnitCurrencyEvidence,
};
use crate::market_storage::{
    MarketRepository, Publication, PublishIntent, PublishPlan, Receipt, Scope, compat,
};
use crate::mcp::ToolHandler;
use crate::publish::{EffectiveVersion, ValidationSummary};
use crate::sqlite_market::backend::MarketBackend;
use crate::sqlite_market::series;
use crate::sqlite_tasks::SqliteTasks;
use crate::task_control::{self, CommandKey, ItemSpec, TaskRepository};
use crate::yearly_publication::results::{YearlyRepository, YearlyResultPlan};
use chrono::{DateTime, NaiveDate};

const SYMBOL: &str = "sh.600006";
const RUN: &str = "run";

struct Fixture {
    tasks: SqliteTasks,
    backend: MarketBackend,
    api: DataApi,
    _dir: TempDir,
}

fn fixture() -> Fixture {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/dev/shm")
        .unwrap();
    let path = dir.path().join("m.db");
    SqliteTasks::new(&path).initialize_compact().unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let backend = MarketBackend::open(&SqliteTasks::new(&path)).unwrap();
    let MarketBackend::Compact(_) = &backend else {
        panic!("fixture must open the compact backend");
    };
    // Task operations must run on the coordinator-bound handle — a plain
    // `SqliteTasks::new` is the legacy adapter and refuses compact files by
    // design (`check_schema` stays fail-closed).
    let tasks = backend.tasks();
    let api = DataApi::new_with_backend(
        tasks.clone(),
        backend.clone(),
        RUN.into(),
        Access::local(),
        None,
    )
    .unwrap();
    Fixture {
        tasks,
        backend,
        api,
        _dir: dir,
    }
}

fn units() -> UnitCurrencyEvidence {
    UnitCurrencyEvidence {
        price_currency: crate::market_data::EvidenceValue::confirmed("CNY", "bound"),
        price_unit: crate::market_data::EvidenceValue::confirmed("share", "bound"),
        volume_unit: crate::market_data::EvidenceValue::confirmed("share", "bound"),
        amount_currency: crate::market_data::EvidenceValue::confirmed("CNY", "bound"),
        amount_unit: crate::market_data::EvidenceValue::confirmed("yuan", "bound"),
    }
}

fn bar(day: i64, cents: i64) -> Bar {
    crate::compact_market::CompactBar {
        trade_day: day,
        open_units: cents,
        high_units: cents + 10,
        low_units: cents - 10,
        close_units: cents,
        volume: Some(100),
        amount: Some("1000".into()),
        r7_raw: Some(7),
    }
    .to_bar(Period::Day, crate::compact_market::PRICE_SCALE_CENTS)
    .unwrap()
}

fn day_epoch(y: i32, m: u32, d: u32) -> i64 {
    crate::compact_market::epoch_day(NaiveDate::from_ymd_opt(y, m, d).unwrap())
}

/// Accepted-and-claimed task with `count` pending items on one scope —
/// mirrors the in-crate TaskHandle fixture without depending on it.
/// Frozen inputs the progress observer replays: daily range items carry
/// base+range, yearly items carry the daily base pin + years + intent.
fn frozen_inputs(period: &str) -> Value {
    if period == "year" {
        json!({
            "base_daily_version_id": "v1",
            "base_integrity_sha256": "ab".repeat(32),
            "years": [2020],
            "intent": {"expected_daily_head": "v1", "expected_yearly_head": null},
            "comparison": null,
        })
    } else {
        json!({
            "base_version_id": "v1",
            "base_integrity_sha256": "ab".repeat(32),
            "range": {"start": "2020-01-06", "end": "2020-02-04"},
        })
    }
}
fn contract_version(period: &str) -> &'static str {
    if period == "year" {
        crate::yearly_publication::tasks::CONTRACT_VERSION
    } else {
        crate::market_update::tasks::CONTRACT_VERSION
    }
}
fn task(fixture: &Fixture, symbol: &str, count: usize, period: &str, req: &str) -> String {
    let scope = task_control::legacy::scope_id(symbol, period);
    let items: Vec<ItemSpec> = (1..=count)
        .map(|index| ItemSpec {
            item_id: format!("item-{index}"),
            target: json!({"symbol": symbol, "period": period}),
            progress_unit: "bars".into(),
            frozen_inputs: frozen_inputs(period),
            scope_keys: BTreeSet::from([scope.clone()]),
        })
        .collect();
    let command = task_control::Command {
        key: CommandKey {
            operation: "update_data".into(),
            dataset: if period == "year" {
                "market.yearly".into()
            } else {
                "market.daily".into()
            },
            contract_version: contract_version(period).into(),
        },
        intent: json!({}),
    };
    let work = task_control::FrozenWork {
        items,
        scopes: BTreeSet::from([scope]),
        shared_evidence: None,
    };
    let acceptance = fixture
        .tasks
        .accept(req, &command, &work, RUN, None)
        .unwrap();
    let task_id = match acceptance.record.outcome {
        task_control::RequestOutcome::Task(snapshot) => snapshot.task.task_id.clone(),
        _ => panic!("expected task outcome"),
    };
    let revision = fixture
        .tasks
        .read_task(&task_id)
        .unwrap()
        .unwrap()
        .task
        .state_revision;
    fixture
        .tasks
        .transition(&task_id, revision, RUN, task_control::Change::Claim)
        .unwrap();
    task_id
}

/// One task with `count` pending items spread across `symbols` — the
/// same shape the batch availability test needs (100 entities).
fn task_many(fixture: &Fixture, symbols: &[String], period: &str, req: &str) -> String {
    let scopes: BTreeSet<String> = symbols
        .iter()
        .map(|s| task_control::legacy::scope_id(s, period))
        .collect();
    let items: Vec<ItemSpec> = symbols
        .iter()
        .enumerate()
        .map(|(i, s)| ItemSpec {
            item_id: format!("item-{}", i + 1),
            target: json!({"symbol": s, "period": period}),
            progress_unit: "bars".into(),
            frozen_inputs: frozen_inputs(period),
            scope_keys: BTreeSet::from([task_control::legacy::scope_id(s, period)]),
        })
        .collect();
    let command = task_control::Command {
        key: CommandKey {
            operation: "update_data".into(),
            dataset: if period == "year" {
                "market.yearly".into()
            } else {
                "market.daily".into()
            },
            contract_version: contract_version(period).into(),
        },
        intent: json!({}),
    };
    let work = task_control::FrozenWork {
        items,
        scopes,
        shared_evidence: None,
    };
    let acceptance = fixture
        .tasks
        .accept(req, &command, &work, RUN, None)
        .unwrap();
    let task_id = match acceptance.record.outcome {
        task_control::RequestOutcome::Task(snapshot) => snapshot.task.task_id.clone(),
        _ => panic!("expected task outcome"),
    };
    let revision = fixture
        .tasks
        .read_task(&task_id)
        .unwrap()
        .unwrap()
        .task
        .state_revision;
    fixture
        .tasks
        .transition(&task_id, revision, RUN, task_control::Change::Claim)
        .unwrap();
    task_id
}

fn start_item(fixture: &Fixture, task_id: &str, item: &str) -> u64 {
    let revision = fixture
        .tasks
        .read_task(task_id)
        .unwrap()
        .unwrap()
        .task
        .state_revision;
    fixture
        .tasks
        .transition(
            task_id,
            revision,
            RUN,
            task_control::Change::StartItem(item.into()),
        )
        .unwrap()
        .task
        .state_revision
}

fn command(task_id: &str, publication_id: &str, revision: u64, item: &str) -> Publication {
    Publication {
        publication_id: publication_id.into(),
        version_id: "v".into(),
        task_id: task_id.into(),
        item_id: item.into(),
        run_id: RUN.into(),
        expected_revision: revision,
        intent: PublishIntent::CurrentHead {
            expected_daily_head: String::new(),
            expected_yearly_head: None,
        },
    }
}

fn batch_of(version: &EffectiveVersion) -> MarketDataBatch {
    MarketDataBatch {
        symbol: version.symbol.clone(),
        period: version.period,
        adjustment: Adjustment::Forward,
        bars: version.records.clone(),
        provenance: Provenance {
            source: version.source.clone(),
            source_period: "day".into(),
            source_adjustment: "qfq".into(),
            symbol: version.symbol.clone(),
            acquired_at: version.published_at,
            adapter_version: "fixture".into(),
            yearly_derivation: version.yearly_derivation.clone(),
            snapshot_inputs: None,
        },
        coverage: CoverageEvidence::complete("full history verified"),
        unit_currency: units(),
        adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "bound"),
    }
}

fn checked_of(version: &EffectiveVersion) -> compat::CheckedRecord {
    // compat::check re-derives the batch validation and requires the
    // candidate's records/validation to equal it — run the real dedup so
    // the sealed record is consistent.
    let mut version = version.clone();
    let batch = batch_of(&version);
    let (records, validation) = crate::publish::validate_and_deduplicate(&batch).unwrap();
    version.records = records;
    version.validation = validation;
    let mut raw = serde_json::to_value(&version).unwrap();
    raw["batch"] = serde_json::to_value(batch_of(&version)).unwrap();
    raw["integrity_sha256"] = json!(compat::legacy_digest(&raw).unwrap());
    compat::check(&raw).unwrap()
}

/// Publish a daily head through the real envelope path (stage → mark
/// validated → commit); `base` is the CAS pin for non-first commits.
fn publish_daily(
    fixture: &Fixture,
    task_id: &str,
    item: &str,
    symbol: &str,
    bars: Vec<Bar>,
    base: Option<&str>,
) -> Receipt {
    let rev = start_item(fixture, task_id, item);
    let version_label = format!("v-{item}");
    let version = EffectiveVersion {
        version_id: version_label.clone(),
        symbol: symbol.into(),
        period: Period::Day,
        adjustment: Adjustment::Forward,
        source: "official".into(),
        records: bars,
        published_at: DateTime::from_timestamp(1_760_000_000, 0).unwrap(),
        validation: ValidationSummary {
            record_count: 0,
            unique_record_count: 0,
            duplicate_count: 0,
        },
        unit_currency: units(),
        adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "bound"),
        yearly_derivation: None,
    };
    let mut version = version.clone();
    let batch = batch_of(&version);
    let (records, validation) = crate::publish::validate_and_deduplicate(&batch).unwrap();
    version.records = records;
    version.validation = validation;
    let mut raw = serde_json::to_value(&version).unwrap();
    raw["batch"] = serde_json::to_value(batch_of(&version)).unwrap();
    raw["compact_source"] = json!({
        "generation_id": "gen-1",
        "generation_manifest_sha256": "0b".repeat(32),
        "day_member_sha256": "0c".repeat(32),
        "security_event_sha256": "0e".repeat(32),
        "generation_evidence": {"snapshot_inputs": []},
    });
    raw["integrity_sha256"] = json!(compat::legacy_digest(&raw).unwrap());
    fixture.backend.stage(&raw).unwrap();
    let candidate = fixture.backend.candidate(&version_label).unwrap();
    fixture.backend.mark_validated(&candidate).unwrap();
    let mut cmd = command(task_id, &format!("pub-{task_id}-{item}"), rev, item);
    cmd.version_id = version_label;
    let scope = Scope {
        symbol: symbol.into(),
        period: Period::Day,
    };
    fixture
        .backend
        .commit(&PublishPlan {
            command: cmd,
            record: candidate,
            required_heads: vec![(scope.clone(), base.map(String::from))],
            promote: Some(scope),
            base_digest: None,
        })
        .unwrap()
}

/// Commit the yearly candidate through the real `commit_yearly` plan path.
/// The candidate is DERIVED from the current daily head via
/// `crate::yearly::aggregate` — yearly bar dates are the base's per-year
/// last trading days, never hand-written.
fn commit_yearly_plan(
    fixture: &Fixture,
    task_id: &str,
    item: &str,
    symbol: &str,
    years: Option<&std::collections::BTreeSet<i32>>,
    base_id: &str,
    prior: Option<String>,
) -> Receipt {
    let rev = start_item(fixture, task_id, item);
    let base = base_checked(fixture, symbol);
    let daily = base.version().clone();
    let as_of = daily.records[daily.records.len() - 1].date;
    let first = daily.records[0].date;
    let annual = crate::yearly::aggregate(&daily, years, as_of).unwrap();
    let version = EffectiveVersion {
        version_id: format!("y-{item}"),
        symbol: symbol.into(),
        period: Period::Year,
        adjustment: Adjustment::Forward,
        source: "official".into(),
        records: annual.records.into_iter().map(|r| r.bar).collect(),
        published_at: DateTime::from_timestamp(1_760_000_000, 0).unwrap(),
        validation: ValidationSummary {
            record_count: 0,
            unique_record_count: 0,
            duplicate_count: 0,
        },
        unit_currency: units(),
        adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "bound"),
        yearly_derivation: Some(crate::market_data::YearlyDerivation {
            base_daily_version_id: base_id.into(),
            algorithm_version: "local-natural-year-v1".into(),
            first_trading_date: first,
            last_trading_date: as_of,
        }),
    };
    let plan = YearlyResultPlan {
        publication: PublishPlan {
            command: command(task_id, &format!("pub-{task_id}-{item}"), rev, item),
            record: checked_of(&version),
            required_heads: vec![],
            promote: None,
            base_digest: None,
        },
        base,
        prior: prior.map(|id| checked_of(&stored_year_version(fixture, symbol, &id))),
        no_change: false,
        audit: json!({}),
        audit_payloads: vec![],
    };
    fixture.backend.commit_yearly(&plan).unwrap()
}

/// Rebuild the stored yearly head (bars + lineage) as a candidate-shaped
/// record — the CAS prior must reflect storage, not a hand-written bar.
fn stored_year_version(fixture: &Fixture, symbol: &str, id: &str) -> EffectiveVersion {
    let c = Connection::open(fixture.tasks.path()).unwrap();
    let row = series::read_series(&c, symbol, Period::Year)
        .unwrap()
        .unwrap();
    assert_eq!(
        row.version_id, id,
        "stored yearly head must match the prior id"
    );
    let lineage = series::yearly_lineage(&c, row.series_key).unwrap().unwrap();
    let bars = series::all_bars(&c, row.series_key)
        .unwrap()
        .into_iter()
        .map(|b| b.to_bar(Period::Year, row.price_scale).unwrap())
        .collect();
    EffectiveVersion {
        version_id: id.into(),
        symbol: symbol.into(),
        period: Period::Year,
        adjustment: Adjustment::Forward,
        source: "official".into(),
        records: bars,
        published_at: DateTime::from_timestamp(1_760_000_000, 0).unwrap(),
        validation: ValidationSummary {
            record_count: 0,
            unique_record_count: 0,
            duplicate_count: 0,
        },
        unit_currency: units(),
        adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "bound"),
        yearly_derivation: Some(crate::market_data::YearlyDerivation {
            base_daily_version_id: crate::sqlite_market::compact_repo::hex_bytes(
                &lineage.base_daily_digest,
            ),
            algorithm_version: lineage.algorithm_version,
            first_trading_date: crate::compact_market::epoch_date(lineage.first_trade_day).unwrap(),
            last_trading_date: crate::compact_market::epoch_date(lineage.last_trade_day).unwrap(),
        }),
    }
}

fn base_checked(fixture: &Fixture, symbol: &str) -> compat::CheckedRecord {
    let c = Connection::open(fixture.tasks.path()).unwrap();
    let daily = series::read_series(&c, symbol, Period::Day)
        .unwrap()
        .unwrap();
    let bars = series::all_bars(&c, daily.series_key)
        .unwrap()
        .iter()
        .map(|b| b.to_bar(Period::Day, daily.price_scale))
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let version = EffectiveVersion {
        version_id: daily.version_id.clone(),
        symbol: symbol.into(),
        period: Period::Day,
        adjustment: Adjustment::Forward,
        source: "official".into(),
        records: bars,
        published_at: DateTime::from_timestamp(1_760_000_000, 0).unwrap(),
        validation: ValidationSummary {
            record_count: 0,
            unique_record_count: 0,
            duplicate_count: 0,
        },
        unit_currency: units(),
        adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "bound"),
        yearly_derivation: None,
    };
    checked_of(&version)
}

fn query_yearly(version: Value) -> Value {
    json!({
        "dataset": "market.yearly",
        "contract_version": "v1",
        "selection": {"kind": "single", "entity": {"exchange": "sh", "code": "600006"}},
        "filter": {"kind": "yearly", "start_year": 2019, "end_year": 2030},
        "version": version,
        "page_size": 100,
    })
}

#[test]
fn yearly_wire_reports_fresh_behind_and_stale_pins() {
    let f = fixture();
    let daily_task = task(&f, SYMBOL, 3, "day", "req-d");
    let daily = publish_daily(
        &f,
        &daily_task,
        "item-1",
        SYMBOL,
        vec![
            bar(day_epoch(2020, 1, 6), 100),
            bar(day_epoch(2020, 2, 4), 110),
        ],
        None,
    );

    let yearly_task = task(&f, SYMBOL, 2, "year", "req-y");
    let _yearly = commit_yearly_plan(
        &f,
        &yearly_task,
        "item-1",
        SYMBOL,
        Some(&std::collections::BTreeSet::from([2020])),
        &daily.version_id,
        None,
    );

    // Fresh yearly: latest resolves present with no stale mark.
    let page = f
        .api
        .call(
            "query_data",
            query_yearly(json!({"kind": "latest_per_entity"})),
        )
        .unwrap();
    assert_eq!(page["version_manifest"][0]["status"], "present");
    assert!(page["version_manifest"][0]["stale"].is_null());

    // Daily append -> the yearly row trails the live base (behind mark).
    let appended = publish_daily(
        &f,
        &daily_task,
        "item-2",
        SYMBOL,
        vec![
            bar(day_epoch(2020, 1, 6), 100),
            bar(day_epoch(2020, 2, 4), 110),
            bar(day_epoch(2020, 3, 5), 120),
        ],
        Some(&daily.version_id),
    );
    assert_ne!(appended.version_id, daily.version_id);
    let page = f
        .api
        .call(
            "query_data",
            query_yearly(json!({"kind": "latest_per_entity"})),
        )
        .unwrap();
    assert_eq!(page["version_manifest"][0]["status"], "present");
    assert_eq!(page["version_manifest"][0]["stale"], "behind");
    assert!(page["version_manifest"][0]["as_of_day"].is_string());
    let availability = f
        .api
        .call(
            "get_data_availability",
            query_yearly(json!({"kind": "latest_per_entity"})),
        )
        .unwrap();
    assert_eq!(availability["items"][0]["stale"], "behind");

    // The daily pin that was superseded reports stale_version; a
    // never-committed digest reports version_not_found.
    for (pin, code) in [
        (daily.version_id.clone(), "stale_version"),
        ("0".repeat(64), "version_not_found"),
    ] {
        let err = f
            .api
            .call(
                "query_data",
                json!({
                    "dataset": "market.daily",
                    "contract_version": "v1",
                    "selection": {"kind": "single", "entity": {"exchange": "sh", "code": "600006"}},
                    "filter": {"kind": "daily", "start_date": "2020-01-01", "end_date": "2030-12-31"},
                    "version": {"kind": "explicit_version_map", "versions": [{"entity": {"exchange": "sh", "code": "600006"}, "version_id": pin}]},
                    "page_size": 100,
                }),
            )
            .unwrap_err();
        assert_eq!(err.code, code, "pin {pin}: {err:?}");
    }
}

#[test]
fn yearly_wire_reports_invalid_as_stale_or_absent_reason() {
    let f = fixture();
    let daily_task = task(&f, SYMBOL, 3, "day", "req-d2");
    let daily = publish_daily(
        &f,
        &daily_task,
        "item-1",
        SYMBOL,
        vec![
            bar(day_epoch(2020, 1, 6), 100),
            bar(day_epoch(2020, 2, 4), 110),
        ],
        None,
    );
    let yearly_task = task(&f, SYMBOL, 2, "year", "req-y2");
    commit_yearly_plan(
        &f,
        &yearly_task,
        "item-1",
        SYMBOL,
        Some(&std::collections::BTreeSet::from([2020])),
        &daily.version_id,
        None,
    );

    // A daily REPLACE (non-prefix rewrite) invalidates the yearly row.
    publish_daily(
        &f,
        &daily_task,
        "item-2",
        SYMBOL,
        vec![
            bar(day_epoch(2020, 4, 2), 300),
            bar(day_epoch(2020, 6, 8), 310),
        ],
        Some(&daily.version_id),
    );

    // Single-entity yearly query: stale_version, not served content.
    let err = f
        .api
        .call(
            "query_data",
            query_yearly(json!({"kind": "latest_per_entity"})),
        )
        .unwrap_err();
    assert_eq!(err.code, "stale_version", "{err:?}");

    // Batched query: row-level absent + reason, not a failed request.
    let batch = f
        .api
        .call(
            "query_data",
            json!({
                "dataset": "market.yearly",
                "contract_version": "v1",
                "selection": {"kind": "batch", "entities": [
                    {"exchange": "sh", "code": "600006"},
                    {"exchange": "sz", "code": "000001"}
                ]},
                "filter": {"kind": "yearly", "start_year": 2019, "end_year": 2030},
                "version": {"kind": "latest_per_entity"},
                "page_size": 100,
            }),
        )
        .unwrap();
    let invalid = batch["version_manifest"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["entity"]["code"] == "600006")
        .unwrap();
    assert_eq!(invalid["status"], "absent");
    assert_eq!(invalid["reason"], "daily_replaced_since_derivation");
    // The second entity never existed -> plain absent.
    let other = batch["version_manifest"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["entity"]["code"] == "000001")
        .unwrap();
    assert_eq!(other["status"], "absent");
    assert!(other["reason"].is_null());

    // Availability marks the invalidated row absent-with-reason too.
    let availability = f
        .api
        .call(
            "get_data_availability",
            query_yearly(json!({"kind": "latest_per_entity"})),
        )
        .unwrap();
    assert_eq!(availability["items"][0]["availability"], "absent");
    assert_eq!(
        availability["items"][0]["reason"],
        "daily_replaced_since_derivation"
    );

    // A rederived yearly clears the invalid state. The new bar sits on the
    // new base's last 2020 trading day (06-08) — deriving a bar at the old
    // base's last day would be a fabricated row and fail closed at read.
    let base = base_checked(&f, SYMBOL);
    let head = {
        let c = Connection::open(f.tasks.path()).unwrap();
        series::read_series(&c, SYMBOL, Period::Year)
            .unwrap()
            .unwrap()
    };
    commit_yearly_plan(
        &f,
        &yearly_task,
        "item-2",
        SYMBOL,
        Some(&std::collections::BTreeSet::from([2020])),
        &base.version().version_id,
        Some(head.version_id),
    );
    let page = f
        .api
        .call(
            "query_data",
            query_yearly(json!({"kind": "latest_per_entity"})),
        )
        .unwrap();
    assert_eq!(page["version_manifest"][0]["status"], "present");
    assert!(page["version_manifest"][0]["stale"].is_null());
}

#[test]
fn yearly_batch_availability_reports_one_invalid_row_among_fresh() {
    // §6.2 batch degrade: 100 entities — the one whose daily was replaced
    // degrades to absent-with-reason; the other 99 stay available.
    let f = fixture();
    let symbols: Vec<String> = (0..100).map(|i| format!("sh.{:06}", 600000 + i)).collect();
    let daily_task = task_many(&f, &symbols, "day", "req-dm");
    for (i, s) in symbols.iter().enumerate() {
        publish_daily(
            &f,
            &daily_task,
            &format!("item-{}", i + 1),
            s,
            vec![
                bar(day_epoch(2020, 1, 6), 100),
                bar(day_epoch(2020, 2, 4), 110),
            ],
            None,
        );
    }
    let yearly_task = task_many(&f, &symbols, "year", "req-ym");
    for (i, s) in symbols.iter().enumerate() {
        let base = base_checked(&f, s);
        commit_yearly_plan(
            &f,
            &yearly_task,
            &format!("item-{}", i + 1),
            s,
            Some(&std::collections::BTreeSet::from([2020])),
            &base.version().version_id,
            None,
        );
    }

    // Replace entity 42's daily — its yearly row is now Invalid.
    let victim = &symbols[42];
    let victim_task = task(&f, victim, 1, "day", "req-dm2");
    let victim_head = base_checked(&f, victim).version().version_id.clone();
    publish_daily(
        &f,
        &victim_task,
        "item-1",
        victim,
        vec![
            bar(day_epoch(2020, 4, 2), 300),
            bar(day_epoch(2020, 6, 8), 310),
        ],
        Some(&victim_head),
    );

    let entities: Vec<Value> = symbols
        .iter()
        .map(|s| json!({"exchange": "sh", "code": &s[3..]}))
        .collect();
    let availability = f
        .api
        .call(
            "get_data_availability",
            json!({
                "dataset": "market.yearly",
                "contract_version": "v1",
                "selection": {"kind": "batch", "entities": entities},
                "filter": {"kind": "yearly", "start_year": 2019, "end_year": 2030},
                "version": {"kind": "latest_per_entity"},
                "page_size": 100,
            }),
        )
        .unwrap();
    let items = availability["items"].as_array().unwrap();
    assert_eq!(items.len(), 100);
    let absent: Vec<_> = items
        .iter()
        .filter(|i| i["availability"] == "absent")
        .collect();
    assert_eq!(
        absent.len(),
        1,
        "exactly the replaced entity must be absent"
    );
    assert_eq!(
        absent[0]["reason"], "daily_replaced_since_derivation",
        "invalid yearly must carry the classified reason"
    );
    assert_eq!(absent[0]["entity"]["code"], &victim[3..]);
}

#[test]
fn progress_through_handler_on_compact_schema() {
    // N3 progress: a compact task reports through the real handler with
    // the item success/failure split intact.
    let f = fixture();
    let task_id = task(&f, SYMBOL, 1, "day", "req-p");
    publish_daily(
        &f,
        &task_id,
        "item-1",
        SYMBOL,
        vec![bar(day_epoch(2020, 1, 6), 100)],
        None,
    );
    let progress = f
        .api
        .call(
            "get_task_progress",
            json!({"task_id": task_id, "page_size": 10}),
        )
        .unwrap();
    assert_eq!(progress["total"], 1);
    assert_eq!(progress["finished"], 1);
}

// ---------------------------------------------------------------------------
// S4a (TASK-LATEST-ONLY-S4.md): `verify_publication_result` per SPEC §6.6 —
// committed-fact semantics through the real admission + runner path on the
// compact schema, plus handler-level success/failure coverage for the other
// verification/read handlers.
// ---------------------------------------------------------------------------

/// One update task whose last item stays Running and marked
/// NeedsVerification — the shape `verify_publication_result` admits.
fn unverified_target(fixture: &Fixture, items: usize, req: &str) -> String {
    let task_id = task(fixture, SYMBOL, items, "day", req);
    publish_daily(
        fixture,
        &task_id,
        "item-1",
        SYMBOL,
        vec![
            bar(day_epoch(2020, 1, 6), 100),
            bar(day_epoch(2020, 2, 4), 110),
        ],
        None,
    );
    let last = format!("item-{items}");
    let rev = start_item(fixture, &task_id, &last);
    fixture
        .tasks
        .transition(
            &task_id,
            rev,
            RUN,
            task_control::Change::RequireVerification {
                item_id: last,
                reason: "owner suspected a crashed write".into(),
            },
        )
        .unwrap();
    task_id
}

fn run_publication_check(
    fixture: &Fixture,
    verify_id: &str,
) -> Result<crate::publication_verification::runner::Outcome, crate::market_storage::Error> {
    crate::publication_verification::runner::Runner {
        tasks: fixture.tasks.clone(),
        evidence: fixture.backend.clone(),
        run_id: RUN.into(),
        actor: "local-owner".into(),
    }
    .run(verify_id)
}

fn accept_publication_check(fixture: &Fixture, req: &str, target: &str) -> String {
    let reply = fixture
        .api
        .call(
            "verify_publication_result",
            json!({
                "contract_version": "v1",
                "request_id": req,
                "target_task_id": target,
            }),
        )
        .unwrap();
    reply["task_id"].as_str().unwrap().to_string()
}

#[test]
fn verify_publication_result_inspects_committed_and_missing_items() {
    // §6.6 ①: item-1's event is the current tip — `new_content_digest ==
    // series.content_digest` and its evidence bundle replays — while the
    // running item has no receipt → InsufficientEvidence, protection kept.
    let f = fixture();
    let target = unverified_target(&f, 2, "req-tv1");
    let verify_id = accept_publication_check(&f, "req-vp1", &target);
    let crate::publication_verification::runner::Outcome::Completed(receipt) =
        run_publication_check(&f, &verify_id).unwrap()
    else {
        panic!("inspection must complete");
    };
    assert_eq!(
        receipt.conclusion,
        crate::publication_verification::decision::Conclusion::InsufficientEvidence
    );
    assert_eq!(receipt.missing_receipt_item_ids, vec!["item-2".to_string()]);
}

#[test]
fn verify_publication_result_overwritten_event_still_binds_by_chain_closure() {
    // §6.6: item-1's event was overwritten by item-2's commit — its digest is
    // no longer the head, so the check verifies the event chain closes to the
    // current series digest instead of declaring the evidence missing.
    let f = fixture();
    let task_id = task(&f, SYMBOL, 3, "day", "req-tv2");
    let head = publish_daily(
        &f,
        &task_id,
        "item-1",
        SYMBOL,
        vec![
            bar(day_epoch(2020, 1, 6), 100),
            bar(day_epoch(2020, 2, 4), 110),
        ],
        None,
    );
    publish_daily(
        &f,
        &task_id,
        "item-2",
        SYMBOL,
        vec![
            bar(day_epoch(2020, 1, 6), 100),
            bar(day_epoch(2020, 2, 4), 110),
            bar(day_epoch(2020, 3, 5), 115),
        ],
        Some(&head.version_id),
    );
    let rev = start_item(&f, &task_id, "item-3");
    f.tasks
        .transition(
            &task_id,
            rev,
            RUN,
            task_control::Change::RequireVerification {
                item_id: "item-3".into(),
                reason: "owner suspected a crashed write".into(),
            },
        )
        .unwrap();
    let verify_id = accept_publication_check(&f, "req-vp2", &task_id);
    let crate::publication_verification::runner::Outcome::Completed(receipt) =
        run_publication_check(&f, &verify_id).unwrap()
    else {
        panic!("inspection must complete");
    };
    assert_eq!(
        receipt.conclusion,
        crate::publication_verification::decision::Conclusion::InsufficientEvidence
    );
    assert_eq!(receipt.missing_receipt_item_ids, vec!["item-3".to_string()]);
}

#[test]
fn verify_publication_result_forged_chain_fails_closed() {
    // §6.6: a tampered event link (old_content_digest on the second event)
    // breaks the closure — facts read returns Integrity and the inspection
    // preserves the unresolved task instead of emitting a conclusion.
    let f = fixture();
    let task_id = task(&f, SYMBOL, 3, "day", "req-tv3");
    let head = publish_daily(
        &f,
        &task_id,
        "item-1",
        SYMBOL,
        vec![bar(day_epoch(2020, 1, 6), 100)],
        None,
    );
    publish_daily(
        &f,
        &task_id,
        "item-2",
        SYMBOL,
        vec![
            bar(day_epoch(2020, 1, 6), 100),
            bar(day_epoch(2020, 2, 4), 110),
        ],
        Some(&head.version_id),
    );
    let rev = start_item(&f, &task_id, "item-3");
    f.tasks
        .transition(
            &task_id,
            rev,
            RUN,
            task_control::Change::RequireVerification {
                item_id: "item-3".into(),
                reason: "owner suspected a crashed write".into(),
            },
        )
        .unwrap();
    let c = Connection::open(f.tasks.path()).unwrap();
    c.execute(
        "UPDATE market_events SET old_content_digest=randomblob(32) WHERE seq=(SELECT MAX(seq) FROM market_events)",
        [],
    )
    .unwrap();
    let verify_id = accept_publication_check(&f, "req-vp3", &task_id);
    let error = run_publication_check(&f, &verify_id).unwrap_err();
    assert!(
        matches!(error, crate::market_storage::Error::Integrity(_)),
        "forged event chain must fail closed: {error:?}"
    );
    // The verification task is preserved unresolved — protection kept.
    let check = f.tasks.read_task(&verify_id).unwrap().unwrap();
    assert_ne!(check.task.status, task_control::Status::Succeeded);
}

#[test]
fn verify_publication_result_forge_and_applicability_errors() {
    let f = fixture();
    let err = f
        .api
        .call(
            "verify_publication_result",
            json!({
                "contract_version": "v1",
                "request_id": "req-vp4",
                "target_task_id": "no-such-task",
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, "task_not_found");
    // A succeeded task is not an unresolved execution → not applicable.
    let task_id = task(&f, SYMBOL, 1, "day", "req-tv4");
    publish_daily(
        &f,
        &task_id,
        "item-1",
        SYMBOL,
        vec![bar(day_epoch(2020, 1, 6), 100)],
        None,
    );
    let err = f
        .api
        .call(
            "verify_publication_result",
            json!({
                "contract_version": "v1",
                "request_id": "req-vp5",
                "target_task_id": task_id,
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, "invalid_arguments");
}

#[test]
fn task_read_handlers_answer_success_and_not_found_on_compact_schema() {
    let f = fixture();
    let task_id = task(&f, SYMBOL, 1, "day", "req-tr");
    publish_daily(
        &f,
        &task_id,
        "item-1",
        SYMBOL,
        vec![bar(day_epoch(2020, 1, 6), 100)],
        None,
    );
    let summary = f
        .api
        .call(
            "get_task",
            json!({"contract_version": "v1", "task_id": task_id}),
        )
        .unwrap();
    assert_eq!(summary["task_id"], task_id);
    let page = f
        .api
        .call("list_tasks", json!({"dataset": "market.daily"}))
        .unwrap();
    assert!(
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["task_id"] == task_id)
    );
    let progress = f
        .api
        .call("get_task_progress", json!({"task_id": task_id}))
        .unwrap();
    assert_eq!(progress["total"], 1);
    for name in ["get_task", "get_task_progress"] {
        let err = f
            .api
            .call(
                name,
                if name == "get_task" {
                    json!({"contract_version": "v1", "task_id": "no-such"})
                } else {
                    json!({"task_id": "no-such"})
                },
            )
            .unwrap_err();
        assert_eq!(err.code, "task_not_found", "{name}");
    }
}

#[test]
fn verify_stopped_writes_admits_and_rejects_on_compact_schema() {
    let f = fixture();
    let target = unverified_target(&f, 2, "req-tsv");
    let reply = f
        .api
        .call(
            "verify_stopped_writes",
            json!({
                "contract_version": "v1",
                "request_id": "req-vs1",
                "target_task_id": target,
            }),
        )
        .unwrap();
    assert!(reply["task_id"].as_str().is_some());
    let err = f
        .api
        .call(
            "verify_stopped_writes",
            json!({
                "contract_version": "v1",
                "request_id": "req-vs2",
                "target_task_id": "no-such-task",
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, "task_not_found");
}

#[cfg(target_os = "linux")]
#[test]
fn inspect_verification_task_binds_and_rejects_on_compact_schema() {
    let f = fixture();
    let target = unverified_target(&f, 2, "req-tiv");
    let verify_id = accept_publication_check(&f, "req-vpi", &target);
    let reply = f
        .api
        .call(
            "inspect_verification_task",
            json!({
                "contract_version": "v1",
                "request_id": "req-iv1",
                "target_task_id": verify_id,
            }),
        )
        .unwrap();
    assert_eq!(reply["bound"], true);
    let err = f
        .api
        .call(
            "inspect_verification_task",
            json!({
                "contract_version": "v1",
                "request_id": "req-iv2",
                "target_task_id": "no-such-task",
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, "task_not_found");
}

#[test]
fn verify_data_gap_accepts_frozen_subject_and_rejects_bad_target() {
    let f = fixture();
    let task_id = task(&f, SYMBOL, 1, "day", "req-tg");
    publish_daily(
        &f,
        &task_id,
        "item-1",
        SYMBOL,
        vec![
            bar(day_epoch(2020, 1, 6), 100),
            bar(day_epoch(2020, 2, 4), 110),
        ],
        None,
    );
    let reply = f
        .api
        .call(
            "verify_data_gap",
            json!({
                "contract_version": "v1",
                "request_id": "req-g1",
                "target": {
                    "symbol": SYMBOL,
                    "period": "day",
                    "start_date": "2020-01-06",
                    "end_date": "2020-02-04",
                },
            }),
        )
        .unwrap();
    assert!(reply["task_id"].as_str().is_some());
    // An unpublished symbol has no subject head to freeze → fail closed.
    let err = f
        .api
        .call(
            "verify_data_gap",
            json!({
                "contract_version": "v1",
                "request_id": "req-g2",
                "target": {
                    "symbol": "sz.999999",
                    "period": "day",
                    "start_date": "2020-01-06",
                    "end_date": "2020-02-04",
                },
            }),
        )
        .unwrap_err();
    assert!(
        matches!(
            err.code.as_str(),
            "invalid_arguments" | "version_not_found" | "storage_error"
        ),
        "unpublished subject must fail: {}",
        err.code
    );
}

/// v8 PR-1 (#387 §7): the friendly spellings (`entity` text, `YYYYMMDD`,
/// omitted `contract_version`/`version`) reach the read core as the same
/// request as the contract form — same records, same manifest.
#[test]
fn friendly_read_inputs_match_the_contract_form() {
    let f = fixture();
    let task_id = task(&f, SYMBOL, 1, "day", "req-n1");
    publish_daily(
        &f,
        &task_id,
        "item-1",
        SYMBOL,
        vec![
            bar(day_epoch(2020, 1, 6), 100),
            bar(day_epoch(2020, 2, 4), 110),
        ],
        None,
    );
    let friendly = json!({
        "dataset": "market.daily",
        "selection": {"kind": "single", "entity": SYMBOL},
        "filter": {"kind": "daily", "start_date": "20200106", "end_date": "20200204"},
        "version": {"kind": "latest"},
    });
    let contract = json!({
        "dataset": "market.daily",
        "contract_version": "v1",
        "selection": {"kind": "single", "entity": {"exchange": "sh", "code": "600006"}},
        "filter": {"kind": "daily", "start_date": "2020-01-06", "end_date": "2020-02-04"},
        "version": {"kind": "latest_per_entity"},
    });
    let friendly_page = f.api.call("query_data", friendly).unwrap();
    let contract_page = f.api.call("query_data", contract).unwrap();
    assert_eq!(friendly_page["records"].as_array().unwrap().len(), 2);
    assert_eq!(friendly_page["records"], contract_page["records"]);
    assert_eq!(
        friendly_page["version_manifest"],
        contract_page["version_manifest"]
    );
    // A date that is not a calendar day names the field and both formats.
    let err = f
        .api
        .call(
            "query_data",
            json!({
                "dataset": "market.daily",
                "selection": {"kind": "single", "entity": SYMBOL},
                "filter": {"kind": "daily", "start_date": "20200230", "end_date": "20200204"},
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, "invalid_arguments");
    assert!(
        err.message.contains("$.filter.start_date"),
        "{}",
        err.message
    );
    assert!(err.message.contains("YYYYMMDD"), "{}", err.message);
    // The display name in `code` is still rejected with routing advice.
    let err = f
        .api
        .call(
            "query_data",
            json!({
                "dataset": "market.daily",
                "selection": {"kind": "single", "entity": {"exchange": "sh", "code": SYMBOL}},
                "filter": {"kind": "daily", "start_date": "2020-01-06", "end_date": "2020-02-04"},
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, "invalid_arguments");
    assert!(err.message.contains("six digits"), "{}", err.message);
    assert!(err.message.contains(SYMBOL), "{}", err.message);
    // A cross-dataset selection is still rejected, naming the allowed kinds.
    let err = f
        .api
        .call(
            "query_data",
            json!({
                "dataset": "market.daily",
                "selection": {"kind": "catalog", "scope": "current_sh_sz_a"},
                "filter": {"kind": "daily", "start_date": "2020-01-06", "end_date": "2020-02-04"},
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, "invalid_arguments");
    assert!(err.message.contains("\"single\""), "{}", err.message);
    // M2 point-in-time parameters are reserved, not silently ignored.
    let err = f
        .api
        .call(
            "query_data",
            json!({
                "dataset": "market.daily",
                "selection": {"kind": "single", "entity": SYMBOL},
                "filter": {"kind": "daily", "start_date": "2020-01-06", "end_date": "2020-02-04"},
                "as_of": "2020-02-04",
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, "reserved_for_m2");
}

/// §2.1.1 #10: `list_tasks(operation=update_data)` filters the operation the
/// production tasks actually carry instead of failing at decode.
#[test]
fn list_tasks_filters_by_the_update_data_operation() {
    let f = fixture();
    let task_id = task(&f, SYMBOL, 1, "day", "req-lt");
    let page = f
        .api
        .call("list_tasks", json!({"operation": "update_data"}))
        .unwrap();
    let items = page["items"].as_array().unwrap();
    assert!(
        items.iter().any(|t| t["task_id"] == task_id.as_str()),
        "{page}"
    );
    assert!(items.iter().all(|t| t["operation"] == "update_data"));
    let none = f
        .api
        .call("list_tasks", json!({"operation": "republish_data"}))
        .unwrap();
    assert!(none["items"].as_array().unwrap().is_empty());
}

/// §7.2 row 10: cancelling without `expected_revision` reads the current
/// revision; an explicit revision still works.
#[test]
fn cancel_task_defaults_to_the_current_revision() {
    let f = fixture();
    let task_id = task(&f, SYMBOL, 1, "day", "req-cx");
    let cancelled = f
        .api
        .call("cancel_task", json!({"task_id": task_id}))
        .unwrap();
    assert_eq!(cancelled["task_id"], task_id.as_str());
    assert_eq!(cancelled["status"], "cancel_requested");
    let other = task(&f, "sz.000001", 1, "day", "req-cx2");
    let explicit = f
        .api
        .call(
            "cancel_task",
            json!({"task_id": other, "expected_revision": 0}),
        )
        .unwrap();
    assert_eq!(explicit["status"], "cancel_requested");
}
