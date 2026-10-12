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
        open_cents: cents,
        high_cents: cents + 10,
        low_cents: cents - 10,
        close_cents: cents,
        volume: Some(100),
        amount: Some("1000".into()),
        r7_raw: Some(7),
    }
    .to_bar(Period::Day)
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
        .map(|b| b.to_bar(Period::Year).unwrap())
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
            first_trading_date: crate::compact_market::epoch_date(lineage.first_trade_day),
            last_trading_date: crate::compact_market::epoch_date(lineage.last_trade_day),
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
        .map(|b| b.to_bar(Period::Day))
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

    // A rederived yearly clears the invalid state.
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
