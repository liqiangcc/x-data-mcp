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
            raw: None,
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
            raw: None,
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

/// D-70 Q6: ETF has no yearly series. A yearly request sees it as absent —
/// never `adjustment_required_for_etf` — and a mixed batch still serves the
/// A-share member normally.
#[test]
fn yearly_requests_treat_etf_as_absent_without_breaking_a_share_members() {
    let f = fixture();
    let daily_task = task(&f, SYMBOL, 1, "day", "req-etf-yearly-daily");
    let daily = publish_daily(
        &f,
        &daily_task,
        "item-1",
        SYMBOL,
        vec![bar(day_epoch(2020, 1, 6), 100)],
        None,
    );
    let yearly_task = task(&f, SYMBOL, 1, "year", "req-etf-yearly");
    commit_yearly_plan(
        &f,
        &yearly_task,
        "item-1",
        SYMBOL,
        None,
        &daily.version_id,
        None,
    );
    seed_security_master(&f, &[("sh.600006", None), ("sh.510050", None)]);
    let MarketBackend::Compact(repo) = &f.backend else {
        panic!("fixture must open the compact backend");
    };
    repo.write(|c| {
        c.execute(
            "UPDATE security_master SET instrument_class=2 WHERE exchange='sh' AND code='510050'",
            [],
        )
        .map_err(|e| crate::market_storage::Error::Storage(e.to_string()))?;
        Ok::<(), crate::market_storage::Error>(())
    })
    .unwrap();

    let etf = json!({"kind":"single","entity":{"exchange":"sh","code":"510050"}});
    let mixed = json!({
        "kind":"batch",
        "entities":[
            {"exchange":"sh","code":"600006"},
            {"exchange":"sh","code":"510050"}
        ]
    });
    let body = |selection: Value| {
        json!({
            "dataset":"market.yearly",
            "contract_version":"v1",
            "selection":selection,
            "filter":{"kind":"yearly","start_year":2019,"end_year":2030},
            "version":{"kind":"latest_per_entity"},
            "page_size":100
        })
    };

    let single = f.api.call("query_data", body(etf.clone())).unwrap();
    assert_eq!(single["version_manifest"][0]["status"], "absent");
    assert!(single["records"].as_array().unwrap().is_empty());

    let batch = f.api.call("query_data", body(mixed.clone())).unwrap();
    let manifests = batch["version_manifest"].as_array().unwrap();
    let by_code = |code: &str| {
        manifests
            .iter()
            .find(|entry| entry["entity"]["code"] == json!(code))
            .unwrap()
    };
    assert_eq!(by_code("510050")["status"], "absent");
    assert_eq!(by_code("600006")["status"], "present");
    assert!(
        batch["records"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["entity"]["code"] == json!("600006"))
    );

    let availability = f
        .api
        .call("get_data_availability", body(etf.clone()))
        .unwrap();
    assert_eq!(availability["items"][0]["availability"], "absent");
    let mixed_availability = f
        .api
        .call("get_data_availability", body(mixed.clone()))
        .unwrap();
    let availability_by_code = |code: &str| {
        mixed_availability["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["entity"]["code"] == json!(code))
            .unwrap()
    };
    assert_eq!(availability_by_code("510050")["availability"], "absent");
    assert_ne!(
        availability_by_code("600006")["availability"],
        "absent",
        "the A-share member must not be dragged into the ETF absence"
    );

    let versions = |selection: Value| {
        json!({
            "dataset":"market.yearly",
            "contract_version":"v1",
            "selection":selection,
            "page_size":100
        })
    };
    let etf_versions = f.api.call("list_data_versions", versions(etf)).unwrap();
    assert!(etf_versions["items"].as_array().unwrap().is_empty());
    let mixed_versions = f.api.call("list_data_versions", versions(mixed)).unwrap();
    let items = mixed_versions["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["version_ref"]["entity"]["code"], json!("600006"));
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

/// VM1-41: what the `Observer` keeps per cursor. Availability is the largest
/// thing it caches: at most 100 entities (a `universe` is one row), each with
/// at most the in-flight tasks plus 5 finished ones. Measured here with 100
/// entities that each have a finished daily task; the bound is far below the
/// 16 MiB per entry the budget check would allow.
#[test]
fn a_cached_availability_snapshot_is_small() {
    let f = fixture();
    let symbols: Vec<String> = (0..100).map(|i| format!("sh.{:06}", 600000 + i)).collect();
    let daily_task = task_many(&f, &symbols, "day", "req-size");
    for (i, s) in symbols.iter().enumerate() {
        publish_daily(
            &f,
            &daily_task,
            &format!("item-{}", i + 1),
            s,
            vec![bar(day_epoch(2020, 1, 6), 100)],
            None,
        );
    }
    let entities: Vec<Value> = symbols
        .iter()
        .map(|s| json!({"exchange": "sh", "code": &s[3..]}))
        .collect();
    let page = f
        .api
        .call(
            "get_data_availability",
            json!({
                "dataset": "market.daily",
                "contract_version": "v1",
                "selection": {"kind": "batch", "entities": entities},
                "filter": {"kind": "daily", "start_date": "2020-01-01", "end_date": "2020-12-31"},
                "version": {"kind": "latest_per_entity"},
                "page_size": 10,
            }),
        )
        .unwrap();
    // The cursor holds all 100 items, not the 10 of the page.
    let all = f
        .api
        .call(
            "get_data_availability",
            json!({
                "dataset": "market.daily",
                "contract_version": "v1",
                "selection": {"kind": "batch", "entities": entities},
                "filter": {"kind": "daily", "start_date": "2020-01-01", "end_date": "2020-12-31"},
                "version": {"kind": "latest_per_entity"},
                "page_size": 100,
            }),
        )
        .unwrap();
    assert!(page["next_cursor"].is_string());
    let items = all["items"].as_array().unwrap();
    assert_eq!(items.len(), 100);
    let bytes = serde_json::to_vec(items).unwrap().len();
    let related = items[0]["related_tasks"].as_array().unwrap();
    eprintln!(
        "availability snapshot: {bytes} B for 100 items ({} B per item, {} related tasks on the first)",
        bytes / 100,
        related.len()
    );
    // 6 related tasks per entity is the cap without in-flight tasks; scale
    // the measured per-item size to that.
    let one_task = serde_json::to_vec(&related[0]).unwrap().len();
    let worst = bytes + 100 * one_task * 5;
    eprintln!("one related task {one_task} B; worst case at 6 per entity {worst} B");
    assert!(worst < 2 * 1024 * 1024, "{worst}");
    // Progress caches the whole `ProgressPage` of a task of up to 100 items
    // (larger tasks use the keyset shape, which caches only the head). Here
    // the 100 items have all finished, so ask for the unfinished ones of a
    // fresh task with 100 pending items.
    let pending = task_many(&f, &symbols, "day", "req-size-progress");
    let progress = f
        .api
        .call(
            "get_task_progress",
            json!({"task_id": pending, "page_size": 100}),
        )
        .unwrap();
    let progress_bytes = serde_json::to_vec(&progress).unwrap().len();
    eprintln!(
        "progress snapshot: {progress_bytes} B, {} unfinished items",
        progress["unfinished"].as_array().map_or(0, Vec::len)
    );
    assert!(progress_bytes < 512 * 1024, "{progress_bytes}");
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
    // AC-M2-12: point-in-time parameters are dataset-declared, not silently
    // ignored; market.daily declares neither.
    let err = f
        .api
        .call(
            "query_data",
            json!({
                "dataset": "market.daily",
                "selection": {"kind": "single", "entity": SYMBOL},
                "filter": {"kind": "daily", "start_date": "2020-01-06", "end_date": "2020-02-04"},
                "known_at": "2020-02-04",
            }),
        )
        .unwrap_err();
    assert_eq!(err.code, "invalid_arguments");
    assert!(
        err.message.contains("unsupported_for_dataset"),
        "{}",
        err.message
    );
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

// ---------------------------------------------------------------------------
// M1 PR-2 downstream constraint (x-data-mcp/32 §4.3): the MCP daily read is
// the forward 口径 only, and the manifest still reports the literal "qfq".
// ---------------------------------------------------------------------------

/// Seed one daily item that writes both 口径 in one transaction: the forward
/// series at 10.05 and the raw series at 11.00 for the same day.
fn seed_daily_pair(fixture: &Fixture) {
    use crate::compact_market::{EvidenceKind, VersionEvidence};
    use crate::market_data::{AdjustmentEvidence, CoverageStatus, RawBatch};
    use crate::sqlite_market::{evidence_store, series_commit};

    let task_id = task(fixture, SYMBOL, 1, "day", "req-pair");
    let item = "item-1";
    let revision = start_item(fixture, &task_id, item);
    let day = day_epoch(2020, 1, 6);
    let forward_bars = vec![bar(day, 1005)];
    let raw_bars = vec![bar(day, 1100)];
    let raw = RawBatch::new(
        raw_bars,
        AdjustmentEvidence::confirmed("raw", "raw .day fact"),
        crate::market_data::AdjustmentEventSet::empty(SYMBOL),
        crate::security_master::CapitalEventSet::empty(SYMBOL),
    );
    let MarketBackend::Compact(repo) = &fixture.backend else {
        panic!("fixture must open the compact backend");
    };
    let scope = Scope {
        symbol: SYMBOL.into(),
        period: Period::Day,
    };
    repo.tx(|c| {
        let forward_compact = series_commit::to_compact_bars(
            &forward_bars,
            Period::Day,
            crate::compact_market::PRICE_SCALE_QFQ,
        )?;
        let version_ev = evidence_store::put_evidence(
            c,
            EvidenceKind::Version,
            &serde_json::to_value(VersionEvidence {
                source: "official".into(),
                coverage_reason: "full history verified".into(),
                unit_currency: units(),
                adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "bound"),
                excluded_bars: None,
            })
            .unwrap(),
        )?;
        let binding = series_commit::put_generation_binding(
            c,
            "gen-1",
            &"0b".repeat(32),
            &"0c".repeat(32),
            &"0e".repeat(32),
            &json!({"snapshot_inputs": []}),
        )?;
        let continuity = series_commit::put_continuity_evidence(c, &json!({"mode": "first"}))?;
        series_commit::commit_item_tx(
            c,
            &series_commit::Commit {
                result_id: "pair-result-1",
                task_id: &task_id,
                item_id: item,
                run_id: RUN,
                occupancy_key: &scope.occupancy_key(),
                expected_task_revision: revision,
                entity: SYMBOL,
                period: Period::Day,
                adjustment: Adjustment::Forward,
                base_version_id: None,
                change: series_commit::SeriesChange::FirstPublish {
                    bars: &forward_compact,
                },
                range_start_day: day,
                range_end_day: day,
                algorithm_version: series_commit::COLLECTION_ALGO,
                window_digest: None,
                source_binding_digest: Some(binding),
                evidence_digest: Some(version_ev),
                coverage_status: Some(CoverageStatus::Complete),
                metadata_changed: false,
                event_evidence_digest: continuity,
                published_at: Some(chrono::DateTime::from_timestamp(1_760_000_000, 0).unwrap()),
                staged_version_id: None,
                defer_task_row: false,
            },
            Some(&raw),
        )
    })
    .unwrap();
}

fn daily_query() -> Value {
    json!({
        "dataset":"market.daily",
        "contract_version":"v1",
        "selection":{"kind":"single","entity":{"exchange":"sh","code":"600006"}},
        "filter":{"kind":"daily","start_date":"2020-01-06","end_date":"2020-01-06"},
        "version":{"kind":"latest_per_entity"}
    })
}

/// x-data-mcp/32 §4.3 (1): `query_data` reads `adjustment=1` only; the raw
/// series is invisible on the MCP surface (AC-M1-10).
#[test]
fn query_data_daily_reads_only_the_forward_口径() {
    let fixture = fixture();
    seed_daily_pair(&fixture);
    let page = fixture.api.call("query_data", daily_query()).unwrap();
    assert_eq!(page["records"].as_array().unwrap().len(), 1);
    assert_eq!(page["records"][0]["close"], "10.05");
    assert_ne!(page["records"][0]["close"], "11.00");
    // A-share bytes stay additive-free (M3-2 PR-C): no ETF-only row/metadata
    // fields appear just because `market.daily` learned the `adjustment` knob.
    assert!(page["records"][0].get("no_trade").is_none());
    let metadata = &page["version_manifest"][0]["metadata"];
    for field in ["price_scale", "last_trade_date", "delisting_date"] {
        assert!(
            metadata.get(field).is_none(),
            "A-share metadata grew {field}"
        );
    }
    // The manifest version is the forward series row, not the raw one.
    let MarketBackend::Compact(repo) = &fixture.backend else {
        panic!("fixture must open the compact backend");
    };
    let forward = repo
        .read(|c| {
            crate::sqlite_market::series::read_series_adjusted(
                c,
                SYMBOL,
                Period::Day,
                Adjustment::Forward,
            )
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        page["version_manifest"][0]["version_ref"]["version_id"],
        forward.version_id
    );
}

/// x-data-mcp/32 §4.3 (2): the manifest's `metadata.adjustment.value` stays
/// the literal `"qfq"` — the downstream `KlineReader` depends on it.
#[test]
fn query_data_manifest_adjustment_stays_qfq() {
    let fixture = fixture();
    seed_daily_pair(&fixture);
    let page = fixture.api.call("query_data", daily_query()).unwrap();
    let adjustment = page["version_manifest"][0]["metadata"]["adjustment"]
        .as_object()
        .expect("daily manifest carries adjustment evidence");
    assert_eq!(adjustment["field"], "price_adjustment");
    assert_eq!(adjustment["status"], "confirmed");
    assert_eq!(adjustment["value"], "qfq");
}

/// One index write through the storage commit path (M1 PR-5): the fixture's
/// task machinery plus the neutral adapter-shaped batch.
fn commit_index(
    fixture: &Fixture,
    task_id: &str,
    item: &str,
    symbol: &str,
    bars: Vec<Bar>,
) -> Receipt {
    let revision = start_item(fixture, task_id, item);
    let MarketBackend::Compact(repo) = &fixture.backend else {
        panic!("fixture must open the compact backend");
    };
    let index = crate::market_data::IndexBatch {
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
                acquired_at: DateTime::from_timestamp(1_760_000_000, 0).unwrap(),
                adapter_version: "fixture".into(),
                yearly_derivation: None,
                snapshot_inputs: None,
            },
            coverage: CoverageEvidence::complete("complete fixture history"),
            unit_currency: UnitCurrencyEvidence {
                price_currency: crate::market_data::EvidenceValue::unknown(
                    "an index level is a point value",
                ),
                price_unit: crate::market_data::EvidenceValue::confirmed("index_point", "bound"),
                volume_unit: crate::market_data::EvidenceValue::confirmed("hand", "bound"),
                amount_currency: crate::market_data::EvidenceValue::confirmed("CNY", "bound"),
                amount_unit: crate::market_data::EvidenceValue::confirmed("yuan", "bound"),
            },
            adjustment_evidence: AdjustmentEvidence::confirmed("raw", "no corporate action"),
        },
        day_member_sha256: "0c".repeat(32),
        security_event_sha256: "0e".repeat(32),
    };
    let generation_evidence = json!({"manifest_sha256": "0b".repeat(32)});
    repo.commit_index(&crate::sqlite_market::index::IndexWrite {
        index: &index,
        result_id: &format!("index-{task_id}-{item}"),
        task_id,
        item_id: item,
        run_id: RUN,
        expected_task_revision: revision,
        generation_id: "gen-1",
        generation_manifest_sha256: &"0b".repeat(32),
        generation_evidence: &generation_evidence,
        published_at: DateTime::from_timestamp(1_760_000_000, 0).unwrap(),
    })
    .unwrap()
}

fn index_query(code: &str, version: Value) -> Value {
    json!({
        "dataset": "market.daily",
        "contract_version": "v1",
        "selection": {"kind": "single", "entity": {"exchange": "sh", "code": code}},
        "filter": {"kind": "daily", "start_date": "1990-01-01", "end_date": "2030-12-31"},
        "version": version,
        "page_size": 100,
    })
}

/// AC-M1-10 (index part): a committed index series is a real bar series in
/// storage but the public MCP read surface never resolves it — neither by
/// latest head, nor by an explicitly pinned version id, nor in the version
/// list.
#[test]
fn index_series_stay_invisible_on_the_query_surface() {
    let f = fixture();
    let task_id = task(&f, "sh.000001", 1, "day", "req-index");
    let receipt = commit_index(
        &f,
        &task_id,
        "item-1",
        "sh.000001",
        vec![
            bar(day_epoch(2020, 1, 6), 300_000),
            bar(day_epoch(2020, 1, 7), 301_000),
        ],
    );

    // The series exists in storage (otherwise "absent" would be trivial).
    let MarketBackend::Compact(repo) = &f.backend else {
        panic!("fixture must open the compact backend");
    };
    let series = repo
        .read(|c| {
            crate::sqlite_market::series::read_series_adjusted(
                c,
                "sh.000001",
                Period::Day,
                Adjustment::Raw,
            )
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        series.instrument_class,
        crate::market_data::INSTRUMENT_CLASS_INDEX
    );
    assert_eq!(series.version_id, receipt.version_id);

    // Latest head: absent, no records.
    let page = f
        .api
        .call(
            "query_data",
            index_query("000001", json!({"kind": "latest_per_entity"})),
        )
        .unwrap();
    assert_eq!(page["version_manifest"][0]["status"], "absent");
    assert!(page["records"].as_array().unwrap().is_empty());

    // A pinned index version id does not become readable just because it is
    // known: the visibility boundary applies to explicit pins too.
    let page = f
        .api
        .call(
            "query_data",
            index_query(
                "000001",
                json!({
                    "kind": "explicit_version_map",
                    "versions": [{
                        "entity": {"exchange": "sh", "code": "000001"},
                        "version_id": receipt.version_id,
                    }],
                }),
            ),
        )
        .unwrap();
    assert_eq!(page["version_manifest"][0]["status"], "absent");
    assert!(page["records"].as_array().unwrap().is_empty());

    // Availability reports the same absence.
    let availability = f
        .api
        .call(
            "get_data_availability",
            index_query("000001", json!({"kind": "latest_per_entity"})),
        )
        .unwrap();
    assert_eq!(availability["items"][0]["availability"], "absent");

    // The version list does not advertise the index series either.
    let versions = f
        .api
        .call(
            "list_data_versions",
            json!({
                "dataset": "market.daily",
                "contract_version": "v1",
                "selection": {"kind": "single", "entity": {"exchange": "sh", "code": "000001"}},
                "page_size": 10,
            }),
        )
        .unwrap();
    assert!(versions["items"].as_array().unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// v8 PR-4 (#387 §4.2/§4.3): universe + cross-section and the three M1
// datasets. The fixtures below commit through the real storage paths; the
// assertions are the wire shape, not a reimplementation of it.
// ---------------------------------------------------------------------------

fn publish_catalog(fixture: &Fixture, id: &str, members: &[(&str, crate::catalog::Exchange)]) {
    use crate::catalog::{
        CatalogSnapshot, CompletenessEvidence, IndustryInfo, SecurityRecord, SecurityType,
    };
    use crate::sqlite_catalog::SqliteCatalog;
    let now = chrono::Utc::now();
    let records = members
        .iter()
        .map(|(code, exchange)| SecurityRecord {
            symbol: (*code).into(),
            name: format!("股票{code}"),
            exchange: exchange.clone(),
            security_type: SecurityType::AShare,
            source_security_id: (*code).into(),
            industry: IndustryInfo::Assigned {
                code: "I1".into(),
                name: "行业一".into(),
                scheme: None,
            },
            source: "fixture:catalog".into(),
            acquired_at: now,
        })
        .collect::<Vec<_>>();
    SqliteCatalog::new(fixture.tasks.clone())
        .publish_fixture(
            id,
            CatalogSnapshot {
                source: "fixture:catalog".into(),
                acquired_at: now,
                completeness: CompletenessEvidence::Complete {
                    expected_count: records.len(),
                },
                records,
            },
            None,
        )
        .unwrap();
}

fn commit_calendar_days(fixture: &Fixture, days: &[NaiveDate]) {
    use crate::market_data::{CalendarBatch, CalendarInputDates};
    use crate::trading_calendar::{CalendarCoverage, TradingCalendar};
    let input = |entity: &str, member: &str| CalendarInputDates {
        entity: entity.into(),
        member: member.into(),
        day_member_sha256: "0a".repeat(32),
        days: days.to_vec(),
    };
    let batch = CalendarBatch {
        inputs: vec![
            input("sh.000001", "sh000001.day"),
            input("sz.399001", "sz399001.day"),
        ],
        security_event_sha256: "0e".repeat(32),
    };
    let calendar = TradingCalendar::union(&batch).unwrap();
    let coverage = CalendarCoverage {
        total_bars: 1,
        outside_bars: 0,
        outside_days: vec![],
        outside_samples: vec![],
        days_truncated: false,
        samples_truncated: false,
        stale_bars: 0,
        stale_days: vec![],
        stale_samples: vec![],
        stale_days_truncated: false,
        stale_samples_truncated: false,
        stock_last_day: days.last().copied(),
        calendar_last_day: calendar.last_day(),
        calendar_digest: calendar.digest_hex(),
    };
    let manifest = "0b".repeat(32);
    let MarketBackend::Compact(repo) = &fixture.backend else {
        panic!("fixture must open the compact backend");
    };
    repo.commit_calendar(&crate::sqlite_market::calendar::CalendarWrite {
        batch: &batch,
        coverage: &coverage,
        task_id: "task-calendar",
        item_id: "derived:calendar",
        generation_id: "gen-1",
        generation_manifest_sha256: &manifest,
        generation_evidence: &json!({"fixture": true}),
    })
    .unwrap();
}

fn seed_adjustment_events(fixture: &Fixture, symbol: &str) {
    use crate::market_data::{AdjustmentEvent, AdjustmentEventSet};
    use rust_decimal::Decimal;
    let MarketBackend::Compact(repo) = &fixture.backend else {
        panic!("fixture must open the compact backend");
    };
    let binding = repo
        .read(|c| series::read_series_adjusted(c, symbol, Period::Day, Adjustment::Forward))
        .unwrap()
        .unwrap()
        .source_binding_digest;
    let set = AdjustmentEventSet {
        entity: symbol.into(),
        events: vec![AdjustmentEvent {
            ordinal: 1,
            effective_day: NaiveDate::from_ymd_opt(2020, 1, 6).unwrap(),
            category: 1,
            c_bits: [0; 4],
            factors: [Decimal::new(150, 2); 4],
            known_at: None,
            basis_status: crate::market_data::FactorBasisStatus::Verified,
        }],
        excluded: 0,
        precision: crate::market_data::FactorPrecision::Stock2dp,
    };
    repo.write(|c| {
        crate::sqlite_market::derived::replace_adjustment_events(
            c,
            &set,
            binding,
            "task-events",
            "item-1",
        )
    })
    .unwrap();
}

fn cross_section_query() -> Value {
    json!({
        "dataset":"market.daily",
        "contract_version":"v2",
        "selection":{"kind":"universe","catalog_version":"catalog-cs"},
        "filter":{"kind":"cross_section","start_date":"2020-01-06","end_date":"2020-01-06"},
        "version":{"kind":"latest_per_entity"},
        "fields":["close"]
    })
}

/// AC-V8-5: `universe` + `cross_section` projects `trade_days` once,
/// `entities` once, and `columns[field][day][entity]`; the concrete catalog
/// version is echoed.
#[test]
fn cross_section_universe_projects_the_column_grid() {
    let f = fixture();
    seed_daily_pair(&f);
    let second = task(&f, "sz.000001", 1, "day", "req-cs-second");
    publish_daily(
        &f,
        &second,
        "item-1",
        "sz.000001",
        vec![bar(day_epoch(2020, 1, 6), 2000)],
        None,
    );
    publish_catalog(
        &f,
        "catalog-cs",
        &[
            ("600006", crate::catalog::Exchange::Shanghai),
            ("000001", crate::catalog::Exchange::Shenzhen),
        ],
    );
    commit_calendar_days(
        &f,
        &[
            NaiveDate::from_ymd_opt(2020, 1, 6).unwrap(),
            NaiveDate::from_ymd_opt(2020, 1, 7).unwrap(),
        ],
    );
    let page = f.api.call("query_data", cross_section_query()).unwrap();
    assert_eq!(page["layout"], "cross_section");
    assert_eq!(page["catalog_version"], "catalog-cs");
    assert_eq!(page["trade_days"], json!(["2020-01-06"]));
    assert_eq!(page["entities"], json!(["sh.600006", "sz.000001"]));
    assert_eq!(page["columns"]["close"], json!([["10.05", "20"]]));
}

/// AC-V8-5: more than five trading days is rejected and the message names the
/// longer-panel alternative.
#[test]
fn cross_section_rejects_a_window_over_five_trading_days() {
    let f = fixture();
    seed_daily_pair(&f);
    publish_catalog(
        &f,
        "catalog-cs",
        &[("600006", crate::catalog::Exchange::Shanghai)],
    );
    let days = (6..=13)
        .map(|d| NaiveDate::from_ymd_opt(2020, 1, d).unwrap())
        .filter(|d| {
            use chrono::Datelike;
            !matches!(d.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun)
        })
        .collect::<Vec<_>>();
    assert_eq!(days.len(), 6);
    commit_calendar_days(&f, &days);
    let mut query = cross_section_query();
    query["filter"]["end_date"] = json!("2020-01-13");
    let error = f.api.call("query_data", query).unwrap_err();
    assert_eq!(error.code, "invalid_arguments");
    assert!(
        error.message.contains("6 trading days"),
        "{}",
        error.message
    );
    assert!(
        error.message.contains("selection.kind=batch"),
        "{}",
        error.message
    );
}

/// AC-V8-6: `market.daily` keeps the stock boundary, so the index entity is
/// absent there; `market.index` exposes the same entity with its own metadata.
#[test]
fn index_is_visible_only_through_its_own_dataset() {
    let f = fixture();
    let task_id = task(&f, "sh.000001", 1, "day", "req-index");
    commit_index(
        &f,
        &task_id,
        "item-1",
        "sh.000001",
        vec![bar(day_epoch(2020, 1, 6), 3000)],
    );
    let base = json!({
        "contract_version":"v2",
        "selection":{"kind":"single","entity":"sh.000001"},
        "filter":{"kind":"daily","start_date":"2020-01-06","end_date":"2020-01-06"},
        "version":{"kind":"latest_per_entity"}
    });
    let mut daily = base.clone();
    daily["dataset"] = json!("market.daily");
    let daily_page = f.api.call("query_data", daily).unwrap();
    assert_eq!(daily_page["versions"][0]["status"], "absent");
    assert_eq!(daily_page["row_count"], 0);

    let mut index = base;
    index["dataset"] = json!("market.index");
    let index_page = f.api.call("query_data", index).unwrap();
    assert_eq!(index_page["layout"], "panel");
    assert_eq!(index_page["versions"][0]["status"], "present");
    assert_eq!(index_page["columns"]["close"], json!([["30"]]));
    let metadata = &index_page["metadata"];
    assert_eq!(metadata["index"]["price_unit"], "index_point");
    assert_eq!(metadata["index"]["volume_unit"], "hand");
    assert_eq!(metadata["index"]["aggregate_scope"], "whole_market");
}

/// #383 owner ruling 2026-10-27: the index commit drops an impossible-OHLC
/// bar, and `market.index` metadata exposes how many were dropped.
#[test]
fn index_metadata_reports_excluded_bad_bars() {
    let f = fixture();
    let task_id = task(&f, "sh.000001", 1, "day", "req-index-bad");
    let mut bad = bar(day_epoch(2020, 1, 7), 3000);
    bad.high = bad.close - rust_decimal::Decimal::new(1, 2); // close > high
    commit_index(
        &f,
        &task_id,
        "item-1",
        "sh.000001",
        vec![bar(day_epoch(2020, 1, 6), 3000), bad],
    );
    let page = f
        .api
        .call(
            "query_data",
            json!({
                "dataset": "market.index",
                "contract_version": "v2",
                "selection": {"kind": "single", "entity": "sh.000001"},
                "filter": {"kind": "daily", "start_date": "2020-01-06", "end_date": "2020-01-07"},
                "version": {"kind": "latest_per_entity"}
            }),
        )
        .unwrap();
    assert_eq!(page["metadata"]["index"]["excluded_bar_count"], json!(1));
}

/// AC-V8-6/AC-V8-8: the calendar has no entity, its rows are `trade_day`, and
/// `selection.kind=market` is the only selection it accepts.
#[test]
fn calendar_is_market_level_and_entity_free() {
    let f = fixture();
    commit_calendar_days(
        &f,
        &[
            NaiveDate::from_ymd_opt(2020, 1, 6).unwrap(),
            NaiveDate::from_ymd_opt(2020, 1, 7).unwrap(),
        ],
    );
    let page = f
        .api
        .call(
            "query_data",
            json!({
                "dataset":"market.calendar",
                "contract_version":"v2",
                "selection":{"kind":"market"},
                "filter":{"kind":"daily","start_date":"2020-01-06","end_date":"2020-01-07"},
                "version":{"kind":"latest_per_entity"}
            }),
        )
        .unwrap();
    assert_eq!(page["layout"], "table");
    assert!(page.get("entities").is_none(), "{page}");
    assert_eq!(
        page["columns"]["trade_day"],
        json!(["2020-01-06", "2020-01-07"])
    );
    assert_eq!(page["versions"][0]["status"], "present");
    assert!(page["versions"][0].get("entity").is_none(), "{page}");
    let error = f
        .api
        .call(
            "query_data",
            json!({
                "dataset":"market.calendar",
                "contract_version":"v2",
                "selection":{"kind":"single","entity":"sh.600006"},
                "filter":{"kind":"daily","start_date":"2020-01-06","end_date":"2020-01-07"},
                "version":{"kind":"latest_per_entity"}
            }),
        )
        .unwrap_err();
    assert_eq!(error.code, "invalid_arguments");
}

/// AC-V8-6: adjustment events expose the `effective_day` column and never a
/// `known_at` column (M1 stores NULL; v8 §4.6 keeps it out of the wire).
#[test]
fn adjustment_events_have_no_known_at_and_no_entity_gate() {
    let f = fixture();
    seed_daily_pair(&f);
    seed_adjustment_events(&f, SYMBOL);
    let page = f
        .api
        .call(
            "query_data",
            json!({
                "dataset":"market.adjustment_events",
                "contract_version":"v2",
                "selection":{"kind":"single","entity":SYMBOL},
                "filter":{"kind":"daily","start_date":"2020-01-06","end_date":"2020-01-06"},
                "version":{"kind":"latest_per_entity"}
            }),
        )
        .unwrap();
    assert_eq!(page["layout"], "table");
    assert_eq!(page["entities"], json!([SYMBOL]));
    assert_eq!(page["columns"]["effective_day"], json!(["2020-01-06"]));
    assert_eq!(page["columns"]["category"], json!([1]));
    assert_eq!(page["columns"]["c1"], json!(["1.50"]));
    assert!(page["columns"].get("known_at").is_none(), "{page}");
    // `fields` can still pick the date alone.
    let only_day = f
        .api
        .call(
            "query_data",
            json!({
                "dataset":"market.adjustment_events",
                "contract_version":"v2",
                "selection":{"kind":"single","entity":SYMBOL},
                "filter":{"kind":"daily","start_date":"2020-01-06","end_date":"2020-01-06"},
                "version":{"kind":"latest_per_entity"},
                "fields":["effective_day"]
            }),
        )
        .unwrap();
    assert_eq!(only_day["columns"], json!({"effective_day":["2020-01-06"]}));
}

/// AC-V8-8: a universe availability is one aggregate row, not one row per
/// member, and it stays inside the small-response budget.
#[test]
fn universe_availability_is_one_aggregate_row() {
    let f = fixture();
    seed_daily_pair(&f);
    publish_catalog(
        &f,
        "catalog-cs",
        &[
            ("600006", crate::catalog::Exchange::Shanghai),
            ("000001", crate::catalog::Exchange::Shenzhen),
        ],
    );
    let page = f
        .api
        .call(
            "get_data_availability",
            json!({
                "dataset":"market.daily",
                "contract_version":"v2",
                "selection":{"kind":"universe","catalog_version":"catalog-cs"},
                "filter":{"kind":"daily","start_date":"2020-01-06","end_date":"2020-01-06"},
                "version":{"kind":"latest_per_entity"}
            }),
        )
        .unwrap();
    assert_eq!(page["row_count"], 1);
    assert_eq!(page["columns"]["universe"][0]["entities"], 2);
    assert_eq!(page["columns"]["universe"][0]["present"], 1);
    assert_eq!(page["columns"]["universe"][0]["absent"], 1);
    assert_eq!(page["columns"]["availability"], json!(["partial"]));
    let bytes = serde_json::to_vec(&page).unwrap().len();
    assert!(bytes <= 700, "universe availability is {bytes} B: {page}");
}

/// AC-V8-6: facets are derived from the same frozen catalog version the
/// catalog query reads, so member counts agree.
#[test]
fn facets_agree_with_the_catalog_query() {
    let f = fixture();
    publish_catalog(
        &f,
        "catalog-facets",
        &[
            ("600006", crate::catalog::Exchange::Shanghai),
            ("600007", crate::catalog::Exchange::Shanghai),
        ],
    );
    let facets = f
        .api
        .call(
            "list_catalog_facets",
            json!({"dataset":"securities.catalog","catalog_version":"catalog-facets"}),
        )
        .unwrap();
    let members = facets["facets"]["industries"].as_array().unwrap();
    assert_eq!(members.len(), 1, "{facets}");
    assert_eq!(members[0]["code"], "I1");
    assert_eq!(members[0]["members"], 2);
    let catalog = f
        .api
        .call(
            "query_data",
            json!({
                "dataset":"securities.catalog",
                "contract_version":"v1",
                "selection":{"kind":"catalog","scope":"current_sh_sz_a"},
                "filter":{"kind":"catalog","industry_code":"I1"},
                "version":{"kind":"catalog_version","version_id":"catalog-facets"}
            }),
        )
        .unwrap();
    assert_eq!(catalog["records"].as_array().unwrap().len(), 2);
}

/// Internally-tagged unit variants silently ignore extra keys under serde's
/// `deny_unknown_fields`; these variants are structs so a caller cannot widen
/// `market` into a single-entity request by accident.
#[test]
fn market_selection_rejects_extra_keys() {
    let f = fixture();
    for (selection, key) in [
        (json!({"kind":"market","entity":"sh.600006"}), "entity"),
        (json!({"kind":"market","exchange":"sz"}), "exchange"),
    ] {
        let error = f
            .api
            .call(
                "query_data",
                json!({
                    "dataset":"market.calendar",
                    "contract_version":"v2",
                    "selection":selection,
                    "filter":{"kind":"daily","start_date":"2020-01-06","end_date":"2020-01-06"},
                    "version":{"kind":"latest_per_entity"}
                }),
            )
            .unwrap_err();
        assert_eq!(error.code, "invalid_arguments");
        assert!(error.message.contains(key), "{}", error.message);
    }
}

#[test]
fn latest_per_entity_rejects_extra_keys() {
    let f = fixture();
    let error = f
        .api
        .call(
            "query_data",
            json!({
                "dataset":"market.daily",
                "contract_version":"v2",
                "selection":{"kind":"single","entity":"sh.600006"},
                "filter":{"kind":"daily","start_date":"2020-01-06","end_date":"2020-01-06"},
                "version":{"kind":"latest_per_entity","entity":"sh.600006"}
            }),
        )
        .unwrap_err();
    assert_eq!(error.code, "invalid_arguments");
    assert!(error.message.contains("entity"), "{}", error.message);
}

#[test]
fn default_write_selection_rejects_extra_keys() {
    let f = fixture();
    let error = f
        .api
        .call(
            "start_collection",
            json!({
                "request_id":"r",
                "dataset":"market.daily",
                "contract_version":"v1",
                "selection":{"kind":"default","symbols":["sh.600006"]}
            }),
        )
        .unwrap_err();
    assert_eq!(error.code, "invalid_arguments");
    assert!(error.message.contains("symbols"), "{}", error.message);
}

/// M3 PR-9 (#446 §10.3): rows of the main table only — no catalog needed.
fn seed_security_master(f: &Fixture, rows: &[(&str, Option<NaiveDate>)]) {
    use crate::security_master::assembly::Assembly;
    use crate::security_master::{
        DelistingReason, DelistingSource, Exchange, IssuerResolution, ListingSource,
        SecurityLifecycleFact,
    };
    let from = NaiveDate::from_ymd_opt(2018, 1, 1).unwrap();
    let facts: Vec<SecurityLifecycleFact> = rows
        .iter()
        .map(|(entity, delisting)| {
            let (exchange, code) = entity.split_once('.').unwrap();
            SecurityLifecycleFact {
                exchange: if exchange == "sh" {
                    Exchange::Sh
                } else {
                    Exchange::Sz
                },
                code: code.into(),
                effective_from: from,
                effective_to: None,
                instrument_class: 0,
                issuer_key: None,
                issuer_resolution: IssuerResolution::CodeDate,
                listing_date: Some(from),
                listing_date_tdx: Some(from),
                listing_source: ListingSource::ExchangeList,
                b_listing_date: None,
                delisting_date: *delisting,
                delisting_date_tdx: *delisting,
                delisting_source: if delisting.is_some() {
                    DelistingSource::ExchangeList
                } else {
                    DelistingSource::Unknown
                },
                delisting_reason_class: DelistingReason::Unknown,
                arrangement_start: None,
                arrangement_end: None,
                listing_conflict: false,
                delisting_conflict: false,
                known_at: Some(from),
            }
        })
        .collect();
    let MarketBackend::Compact(repo) = &f.backend else {
        panic!("fixture must open the compact backend");
    };
    repo.write(|c| {
        // `security_master` rows carry a foreign key to `source_bindings`, so
        // the fixture has to bind one the same way a real run does (no FK
        // shortcuts: the storage adapter still enforces the production shape).
        let generation = crate::sqlite_market::evidence_store::put_evidence(
            c,
            crate::compact_market::EvidenceKind::Generation,
            &json!({"generation": "gen-scope"}),
        )
        .unwrap();
        let binding = crate::sqlite_market::evidence_store::put_source_binding(
            c,
            &crate::compact_market::SourceBinding {
                generation_id: "gen-scope".into(),
                generation_manifest_sha256: [0x0b; 32],
                day_member_sha256: [0x0c; 32],
                security_event_sha256: [0x0e; 32],
                evidence_digest: generation,
            },
        )
        .unwrap();
        crate::sqlite_market::security::replace_security_master(
            c,
            &Assembly {
                rows: facts,
                migrations: Vec::new(),
                conflicts: Vec::new(),
                pending_sources: Vec::new(),
                unresolved: Vec::new(),
                exchange_as_of: Some(from),
            },
            binding,
            "task-master",
            "derived:security_master",
        )
    })
    .unwrap();
}

/// D-80 through the public read face: an ETF listed in 2005 and first observed
/// in 2026 is answered for a 2020 `as_of`, and a `known_at` before the first
/// observation excludes the row and counts it.
#[test]
fn security_master_serves_an_etf_from_its_listing_day_and_known_at_still_filters() {
    use crate::security_master::assembly::{Assembly, MemberFact, member_rows};
    use crate::security_master::{DelistingReason, DelistingSource, Exchange, ListingSource};
    let f = fixture();
    let listed = NaiveDate::from_ymd_opt(2005, 2, 23).unwrap();
    let observed = NaiveDate::from_ymd_opt(2026, 11, 5).unwrap();
    let rows = member_rows(&[MemberFact {
        exchange: Exchange::Sh,
        code: "510050".into(),
        first_seen: observed,
        known_at: observed,
        listing_date: Some(listed),
        listing_source: ListingSource::ExchangeList,
        listing_date_tdx: Some(listed),
        effective_to: None,
        delisting_date: None,
        delisting_source: DelistingSource::Unknown,
        delisting_reason_class: DelistingReason::Unknown,
        announcement_date: None,
        announcement_url: None,
        extraction: None,
    }])
    .unwrap();
    let MarketBackend::Compact(repo) = &f.backend else {
        panic!("fixture must open the compact backend");
    };
    repo.write(|c| {
        let generation = crate::sqlite_market::evidence_store::put_evidence(
            c,
            crate::compact_market::EvidenceKind::Generation,
            &json!({"generation": "gen-etf"}),
        )
        .unwrap();
        let binding = crate::sqlite_market::evidence_store::put_source_binding(
            c,
            &crate::compact_market::SourceBinding {
                generation_id: "gen-etf".into(),
                generation_manifest_sha256: [0x0b; 32],
                day_member_sha256: [0x0c; 32],
                security_event_sha256: [0x0e; 32],
                evidence_digest: generation,
            },
        )
        .unwrap();
        crate::sqlite_market::security::replace_security_master(
            c,
            &Assembly {
                rows,
                migrations: Vec::new(),
                conflicts: Vec::new(),
                pending_sources: Vec::new(),
                unresolved: Vec::new(),
                exchange_as_of: Some(observed),
            },
            binding,
            "task-master",
            "derived:security_master",
        )
    })
    .unwrap();
    let ask = |known_at: Option<&str>| {
        let mut input = json!({
            "securities": ["sh.510050"], "fact": "security_master", "as_of": "2020-06-30"
        });
        if let Some(day) = known_at {
            input["known_at"] = json!(day);
        }
        f.api.call("get_security_state", input).unwrap()
    };
    let now = ask(None);
    assert_eq!(now["row_count"], 1, "{now}");
    // The record set is selected by `effective_from <= as_of`, so a row for
    // 2020 means the interval opened before the first observation (2026).
    assert_eq!(now["columns"]["known_at"], json!(["2026-11-05"]));
    assert_eq!(now["columns"]["listing_date"], json!(["2005-02-23"]));
    // Information set first: known by 2020 the row did not exist yet.
    let early = ask(Some("2020-06-30"));
    assert_eq!(early["row_count"], 0, "{early}");
    let late = ask(Some("2026-11-05"));
    assert_eq!(late["row_count"], 1, "{late}");
}

/// Seed `derived:security_names` the way the daily item does: one generation
/// binding, then the whole-table replace. The read test goes through the public
/// `query_data` handler, so the stored rows are exactly what a real run leaves.
fn seed_security_names(f: &Fixture) {
    use crate::security_master::Exchange;
    use crate::security_master::names::{NameCertainty, NameInterval, NameSet, NameSourceKind};
    use crate::sqlite_market::security_names::SecurityNameWrite;

    let row = |code: &str,
               from: NaiveDate,
               to: Option<NaiveDate>,
               name: &str,
               certainty: NameCertainty,
               known_at: Option<NaiveDate>,
               record: Option<&str>| NameInterval {
        exchange: Exchange::Sz,
        code: code.into(),
        effective_from: from,
        effective_to: to,
        name: name.into(),
        certainty,
        known_at,
        source_kind: NameSourceKind::SzseNameHistory,
        source_record_id: record.map(str::to_owned),
    };
    // XDM-157: the two dated rows are first known on the Shanghai capture days
    // 2026-11-01/2026-11-02; the third row predates the rule and keeps NULL.
    let set = NameSet {
        rows: vec![
            row(
                "000001",
                NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
                Some(NaiveDate::from_ymd_opt(2027, 6, 1).unwrap()),
                "ST旧名",
                NameCertainty::Interval,
                Some(NaiveDate::from_ymd_opt(2026, 11, 1).unwrap()),
                Some("record-1"),
            ),
            row(
                "000002",
                NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
                None,
                "新名",
                NameCertainty::Unbounded,
                Some(NaiveDate::from_ymd_opt(2026, 11, 2).unwrap()),
                Some("record-2"),
            ),
            row(
                "000003",
                NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                None,
                "无日期",
                NameCertainty::Unbounded,
                None,
                None,
            ),
        ],
        coverage: Vec::new(),
        pending_sources: Vec::new(),
    };
    f.backend
        .commit_security_names(&SecurityNameWrite {
            set: &set,
            etf: crate::sqlite_market::security_names::EtfPart::Keep,
            task_id: "task-names",
            item_id: "derived:security_names",
            generation_id: "gen-names",
            generation_manifest_sha256: &"41".repeat(32),
            member_sha256: &"42".repeat(32),
            security_event_sha256: &"43".repeat(32),
            generation_evidence: &json!({"generation_id": "gen-names"}),
        })
        .unwrap()
        .expect("a non-empty name set registers its version");
}

/// One complete `szse_short_name` refresh, the durable owner of the dataset's
/// `metadata.verified_through` (XDM-157, `issue-446` §5.3/§5.6).
fn seed_names_refresh_run(f: &Fixture, verified_through: &str) {
    use crate::exchange_source::SourceId;
    use crate::sqlite_market::refresh_runs::{RunFinish, RunSourceRow, RunStart};

    let run_id = "refresh-names-1".to_owned();
    let source = SourceId::SzseShortName;
    let claimed = f
        .backend
        .claim_source_refresh_run(&RunStart {
            run_id: run_id.clone(),
            mode: "full".into(),
            scope: source.as_str().into(),
            started_at: "2026-11-02T01:00:00Z".into(),
            task_id: None,
            owner_instance: "mcp-tests".into(),
        })
        .unwrap();
    assert!(claimed.is_none(), "the test refresh run opens");
    f.backend
        .finish_source_refresh_run(
            &run_id,
            &RunFinish {
                status: "succeeded".into(),
                finished_at: "2026-11-02T02:00:00Z".into(),
                requests: 1,
                reused: 0,
                changed: 0,
                failed_windows: 0,
                unobserved_windows: 0,
                verified_through: Some(verified_through.to_owned()),
                detail: None,
            },
            &[RunSourceRow {
                run_id: run_id.clone(),
                source_id: source.as_str().into(),
                class: "table".into(),
                complete: true,
                requests: 1,
                snapshots: 0,
                changed: 0,
                unobserved_windows: 0,
                failed_windows: 0,
                elapsed_ms: 1,
                verified_through: Some(verified_through.to_owned()),
            }],
            &[],
        )
        .unwrap();
}

fn seed_risk_warnings(f: &Fixture) {
    use crate::security_master::coverage::CoverageWindow;
    use crate::security_master::risk_warnings::RiskWarningSet;
    use crate::security_master::{Exchange, RiskStatus, RiskWarningInterval};
    use crate::sqlite_market::risk_warnings::RiskWarningWrite;

    let day = |y, m, d| NaiveDate::from_ymd_opt(y, m, d).unwrap();
    let set = RiskWarningSet {
        rows: vec![RiskWarningInterval {
            exchange: Exchange::Sz,
            code: "002731".into(),
            effective_from: day(2026, 1, 5),
            effective_to: None,
            status: RiskStatus::OtherRiskWarning,
            raw_status_code: "ST".into(),
            announced_at: None,
            known_at: Some(day(2026, 11, 1)),
            source_kind: "szse_name_history".into(),
            source_record_id: Some("sz|002731|2026-01-05".into()),
            source_document_sha256: Some("a".repeat(64)),
        }],
        coverage: vec![
            CoverageWindow {
                source_kind: "szse_name_history".into(),
                exchange: Exchange::Sz,
                from: day(1994, 1, 3),
                to: day(2026, 11, 1),
                observed: true,
                rows: 1,
                reason: "fixture".into(),
            },
            CoverageWindow {
                source_kind: "exchange_announcement".into(),
                exchange: Exchange::Sh,
                from: day(2000, 1, 1),
                to: day(2003, 12, 31),
                observed: false,
                rows: 0,
                reason: "pageCount=0".into(),
            },
        ],
        pending_sources: Vec::new(),
        sh_deferred: Vec::new(),
    };
    f.backend
        .commit_risk_warnings(&RiskWarningWrite {
            set: &set,
            task_id: "task-risk",
            item_id: "derived:risk_warnings",
            generation_id: "gen-risk",
            generation_manifest_sha256: &"51".repeat(32),
            member_sha256: &"52".repeat(32),
            security_event_sha256: &"53".repeat(32),
            generation_evidence: &json!({"generation_id": "gen-risk"}),
            replay_inputs_digest: None,
        })
        .unwrap()
        .expect("a non-empty set registers its version");
}

fn query_risk_warnings(f: &Fixture) -> Value {
    f.api
        .call(
            "query_data",
            json!({
                "dataset": "market.risk_warnings",
                "contract_version": "v2",
                "selection": {"kind": "single", "entity": "sz.002731"},
                "filter": {"kind": "daily", "start_date": "2026-01-01",
                           "end_date": "2026-12-31", "as_of": "2026-07-07"},
                "version": {"kind": "latest_per_entity"}
            }),
        )
        .unwrap()
}

/// A registered `market.risk_warnings` version never says the source is
/// pending (the D-58 source is admitted and written), keeps its
/// `coverage_status`, and states the real SH/BJ limits in `limitations`.
#[test]
fn registered_risk_warnings_carry_no_pending_text_and_state_real_limits() {
    let f = fixture();
    seed_risk_warnings(&f);

    let page = query_risk_warnings(&f);
    assert_eq!(page["row_count"], 1, "{page}");
    assert_eq!(page["versions"][0]["status"], "present");
    let metadata = &page["metadata"];
    assert_eq!(metadata["coverage_status"], "complete");
    let reason = metadata["coverage_reason"].as_str().unwrap();
    assert!(!reason.contains("pending"), "{reason}");
    assert!(!reason.contains("#308"), "{reason}");
    assert!(
        reason.starts_with(
            "1 SZ and 0 SH stored ST interval row(s) (SH intervals are built from announcement events); \
             1 acquisition window(s) observed, 1 not_observed; "
        ),
        "{reason}"
    );
    let limitations = page["limitations"].to_string();
    assert!(!limitations.contains("pending"), "{limitations}");
    assert!(limitations.contains("announcement events"), "{limitations}");
    assert!(limitations.contains("D-84"), "{limitations}");
    assert!(
        !limitations.contains("effective day is the announcement day"),
        "{limitations}"
    );
    assert!(limitations.contains("BJ is not available"), "{limitations}");
    assert!(
        limitations.contains("sh: 1 window(s) between 2000-01-01 and 2003-12-31"),
        "{limitations}"
    );
    // D-81: the absence wording is market-aware — SZ writes `normal` inside
    // the short-name coverage, SH never infers it.
    assert!(
        limitations.contains("SZ also stores `normal` intervals"),
        "{limitations}"
    );
    assert!(limitations.contains("never `normal`"), "{limitations}");
}

/// An unregistered dataset stays `absent` and explains the empty answer in
/// `limitations`; no metadata (so no `coverage_reason`) is invented.
#[test]
fn unregistered_risk_warnings_stay_absent_with_a_reason() {
    let f = fixture();
    let page = query_risk_warnings(&f);
    assert_eq!(page["row_count"], 0, "{page}");
    assert_eq!(page["versions"][0]["status"], "absent");
    assert!(
        page["metadata"].is_null() || page.get("metadata").is_none(),
        "{page}"
    );
    let limitations = page["limitations"].to_string();
    assert!(
        limitations.contains("market.risk_warnings has no registered version"),
        "{limitations}"
    );
}

/// Interval rows for the #587 truncation tests. The source kind follows the
/// real writer (SH announcement events, SZ short-name history); `known_at` is
/// a fixture capture day.
fn seed_risk_intervals(
    f: &Fixture,
    rows: &[(
        &str,
        NaiveDate,
        Option<NaiveDate>,
        crate::security_master::RiskStatus,
    )],
) {
    use crate::security_master::coverage::CoverageWindow;
    use crate::security_master::risk_warnings::RiskWarningSet;
    use crate::security_master::{Exchange, RiskWarningInterval};
    use crate::sqlite_market::risk_warnings::RiskWarningWrite;

    let intervals: Vec<RiskWarningInterval> = rows
        .iter()
        .map(|(entity, from, to, status)| {
            let (exchange, code) = entity.split_once('.').unwrap();
            let exchange = if exchange == "sh" {
                Exchange::Sh
            } else {
                Exchange::Sz
            };
            RiskWarningInterval {
                exchange,
                code: code.into(),
                effective_from: *from,
                effective_to: *to,
                status: *status,
                raw_status_code: status.as_str().into(),
                announced_at: None,
                known_at: Some(day(2026, 11, 1)),
                source_kind: if exchange == Exchange::Sh {
                    "exchange_announcement".into()
                } else {
                    "szse_name_history".into()
                },
                source_record_id: Some(format!("{entity}|{from}")),
                source_document_sha256: Some("a".repeat(64)),
            }
        })
        .collect();
    let set = RiskWarningSet {
        rows: intervals,
        coverage: vec![CoverageWindow {
            source_kind: "szse_name_history".into(),
            exchange: Exchange::Sz,
            from: day(1994, 1, 3),
            to: day(2026, 11, 1),
            observed: true,
            rows: rows.len(),
            reason: "fixture".into(),
        }],
        pending_sources: Vec::new(),
        sh_deferred: Vec::new(),
    };
    f.backend
        .commit_risk_warnings(&RiskWarningWrite {
            set: &set,
            task_id: "task-risk",
            item_id: "derived:risk_warnings",
            generation_id: "gen-risk",
            generation_manifest_sha256: &"51".repeat(32),
            member_sha256: &"52".repeat(32),
            security_event_sha256: &"53".repeat(32),
            generation_evidence: &json!({"generation_id": "gen-risk"}),
            replay_inputs_digest: None,
        })
        .unwrap()
        .expect("a non-empty set registers its version");
}

/// #587: an open risk-warning interval stops at the security's delisting date.
/// The delisting day itself is already outside the security's life
/// (`member_state` is `Out(AfterDelisting)` from that day on), so neither the
/// delisting day nor any later day may report a warning. Three SH and three SZ
/// codes, both entry points.
#[test]
fn risk_warnings_open_intervals_stop_at_the_delisting_date() {
    use crate::security_master::RiskStatus;

    let f = fixture();
    let delisting = day(2019, 6, 28);
    let codes = [
        "sh.600146",
        "sh.600193",
        "sh.600301",
        "sz.000013",
        "sz.000015",
        "sz.000047",
    ];
    seed_security_master(&f, &codes.map(|entity| (entity, Some(delisting))));
    seed_risk_intervals(
        &f,
        &codes.map(|entity| {
            (
                entity,
                day(2014, 12, 1),
                None,
                RiskStatus::DelistingRiskWarning,
            )
        }),
    );

    for entity in codes {
        let state = |as_of: &str| {
            f.api
                .call(
                    "get_security_state",
                    json!({"securities": [entity], "fact": "risk_warnings", "as_of": as_of}),
                )
                .unwrap()
        };
        let before = state("2019-06-27");
        assert_eq!(before["row_count"], 1, "{entity}: {before}");
        assert_eq!(
            before["columns"]["normalized_status"],
            json!(["delisting_risk_warning"]),
            "{entity}: {before}"
        );
        assert_eq!(
            before["columns"]["effective_to"],
            json!(["2019-06-28"]),
            "{entity}: the open interval is reported up to the delisting date"
        );
        for after in ["2019-06-28", "2020-06-30"] {
            let page = state(after);
            assert_eq!(page["row_count"], 0, "{entity} at {after}: {page}");
        }

        // The same record-set filter through `query_data` (single-day window).
        let query = |as_of: &str| {
            f.api
                .call(
                    "query_data",
                    json!({
                        "dataset": "market.risk_warnings",
                        "contract_version": "v2",
                        "selection": {"kind": "single", "entity": entity},
                        "filter": {"kind": "daily", "start_date": as_of,
                                   "end_date": as_of, "as_of": as_of},
                        "version": {"kind": "latest_per_entity"}
                    }),
                )
                .unwrap()
        };
        assert_eq!(query("2019-06-27")["row_count"], 1, "{entity}");
        assert_eq!(query("2019-06-28")["row_count"], 0, "{entity}");
        assert_eq!(query("2020-06-30")["row_count"], 0, "{entity}");
    }
}

/// #587: only an open interval of a security with a delisting date is
/// truncated. A closed interval that ends at or before the delisting date is
/// unchanged, one that extends past it is cut at the delisting date, a
/// security without a delisting date keeps its open interval, and a code the
/// main table does not contain is not guessed at.
#[test]
fn risk_warnings_truncate_only_open_intervals_of_delisted_securities() {
    use crate::security_master::RiskStatus;

    let f = fixture();
    let delisting = day(2019, 6, 28);
    seed_security_master(
        &f,
        &[
            ("sh.600146", Some(delisting)), // open -> truncated
            ("sh.600193", Some(delisting)), // closed before -> unchanged
            ("sh.600301", Some(delisting)), // closed after -> truncated
            ("sz.000013", None),            // no delisting date -> unchanged
        ],
    );
    seed_risk_intervals(
        &f,
        &[
            (
                "sh.600146",
                day(2014, 12, 1),
                None,
                RiskStatus::DelistingRiskWarning,
            ),
            (
                "sh.600193",
                day(2014, 12, 1),
                Some(day(2019, 6, 3)),
                RiskStatus::DelistingRiskWarning,
            ),
            (
                "sh.600301",
                day(2014, 12, 1),
                Some(day(2021, 1, 4)),
                RiskStatus::DelistingRiskWarning,
            ),
            (
                "sz.000013",
                day(2014, 12, 1),
                None,
                RiskStatus::DelistingRiskWarning,
            ),
            // Not in the main table: no delisting date to truncate with.
            (
                "sz.000015",
                day(2014, 12, 1),
                None,
                RiskStatus::DelistingRiskWarning,
            ),
        ],
    );
    let state = |entity: &str, as_of: &str| {
        f.api
            .call(
                "get_security_state",
                json!({"securities": [entity], "fact": "risk_warnings", "as_of": as_of}),
            )
            .unwrap()
    };

    let closed = state("sh.600193", "2019-06-02");
    assert_eq!(closed["row_count"], 1, "{closed}");
    assert_eq!(closed["columns"]["effective_to"], json!(["2019-06-03"]));
    assert_eq!(state("sh.600193", "2019-06-03")["row_count"], 0);

    let late = state("sh.600301", "2019-06-27");
    assert_eq!(late["row_count"], 1, "{late}");
    assert_eq!(
        late["columns"]["effective_to"],
        json!(["2019-06-28"]),
        "a closed interval that runs past the delisting date is cut there"
    );
    assert_eq!(state("sh.600301", "2019-06-28")["row_count"], 0);

    let unknown = state("sz.000013", "2020-06-30");
    assert_eq!(unknown["row_count"], 1, "{unknown}");
    assert!(
        unknown["columns"].get("effective_to").is_none(),
        "a security without a delisting date keeps its open end: {unknown}"
    );

    let absent = state("sz.000015", "2020-06-30");
    assert_eq!(absent["row_count"], 1, "{absent}");
}

/// #587: the truncation is a read-time filter, so the registered version and
/// the exported rows do not move: the export still carries the open interval
/// with `effective_to = NULL`, and the `version_id` is the same before and
/// after the truncated answer.
#[test]
fn read_time_truncation_leaves_the_export_identity_unchanged() {
    use crate::security_master::RiskStatus;

    let f = fixture();
    let delisting = day(2019, 6, 28);
    seed_security_master(&f, &[("sh.600146", Some(delisting))]);
    seed_risk_intervals(
        &f,
        &[(
            "sh.600146",
            day(2014, 12, 1),
            None,
            RiskStatus::DelistingRiskWarning,
        )],
    );

    let version = |f: &Fixture| {
        f.backend
            .read(|c| crate::sqlite_market::derived::read_kind_version(c, "risk_warnings"))
            .unwrap()
            .expect("the fixture registers a risk-warnings version")
            .version_id
    };
    // The exporter (`research::read::risk_warning_rows`) reads exactly this
    // accessor; comparing its rows is comparing the exported interval facts.
    let export = |f: &Fixture| {
        f.backend
            .read(crate::sqlite_market::security::read_risk_warning_intervals)
            .unwrap()
    };

    let before = version(&f);
    let exported_before = export(&f);
    let page = f
        .api
        .call(
            "get_security_state",
            json!({"securities": ["sh.600146"], "fact": "risk_warnings",
                   "as_of": "2020-06-30"}),
        )
        .unwrap();
    assert_eq!(page["row_count"], 0, "{page}");

    assert_eq!(
        version(&f),
        before,
        "the read path does not rewrite the version"
    );
    assert_eq!(export(&f), exported_before, "the exported rows do not move");
    assert_eq!(exported_before.len(), 1);
    assert_eq!(
        exported_before[0].effective_to, None,
        "the export keeps the stored open end"
    );
}

/// An unknown `operation` names the tools this entry serves, so the caller
/// can retry with `operation=query_data`.
#[test]
fn unknown_capability_operation_lists_the_available_tools() {
    let f = fixture();
    let err = f
        .api
        .call("get_data_capabilities", json!({"operation": "query"}))
        .unwrap_err();
    assert_eq!(err.code, "unsupported_operation");
    assert!(
        err.message.contains("operation not registered"),
        "{}",
        err.message
    );
    assert!(err.message.contains("query_data"), "{}", err.message);
    assert!(
        err.message.contains("get_data_capabilities"),
        "{}",
        err.message
    );
    // The suggested spelling really works.
    f.api
        .call("get_data_capabilities", json!({"operation": "query_data"}))
        .unwrap();
}

fn query_security_names(f: &Fixture, known_at: Option<&str>) -> Value {
    let mut request = json!({
        "dataset": "market.security_names",
        "contract_version": "v2",
        "selection": {
            "kind": "batch",
            "entities": [{"exchange": "sz", "code": "000001"},
                         {"exchange": "sz", "code": "000002"},
                         {"exchange": "sz", "code": "000003"}]
        },
        "filter": {"kind": "daily", "start_date": "2026-01-01",
                   "end_date": "2026-12-31", "as_of": "2026-12-31"},
        "version": {"kind": "latest_per_entity"}
    });
    if let Some(day) = known_at {
        request["known_at"] = json!(day);
    }
    f.api.call("query_data", request).unwrap()
}

/// The row key `effective_from` is a default column of `market.security_master`
/// (a `table` layout has no `dates`); `fields` can still drop it.
#[test]
fn security_master_default_columns_include_the_row_key() {
    let f = fixture();
    seed_security_master(&f, &[("sh.600006", None)]);
    let query = |fields: Option<Value>| {
        let mut request = json!({
            "dataset":"market.security_master","contract_version":"v2",
            "selection":{"kind":"single","entity":"sh.600006"},
            "filter":{"kind":"daily","start_date":"2020-01-06","end_date":"2020-01-06"},
            "version":{"kind":"latest_per_entity"}
        });
        if let Some(fields) = fields {
            request["fields"] = fields;
        }
        f.api.call("query_data", request).unwrap()
    };
    let page = query(None);
    assert_eq!(page["row_count"], 1, "{page}");
    assert_eq!(page["columns"]["effective_from"], json!(["2018-01-01"]));
    assert!(page["columns"].get("effective_to").is_none(), "{page}");
    let narrowed = query(Some(json!(["instrument_class"])));
    assert!(
        narrowed["columns"].get("effective_from").is_none(),
        "{narrowed}"
    );
}

/// XDM-163: `market.security_names` is no longer a placeholder. The read face
/// serves the stored intervals with the row shape and metadata the other M2
/// fact datasets use, and the `known_at` filter is a Shanghai-day information
/// set applied before the effective-window/`as_of` record set.
#[test]
fn security_names_read_face_serves_intervals_and_filters_known_at() {
    let f = fixture();
    seed_security_names(&f);
    seed_names_refresh_run(&f, "2026-11-02");

    let page = query_security_names(&f, None);
    assert_eq!(page["layout"], "table");
    assert_eq!(page["row_count"], 3, "{page}");
    assert_eq!(
        page["entities"],
        json!(["sz.000001", "sz.000002", "sz.000003"])
    );
    assert_eq!(page["columns"]["name"], json!(["ST旧名", "新名", "无日期"]));
    assert_eq!(
        page["columns"]["effective_from"],
        json!(["2026-01-05", "2026-04-01", "2026-01-01"])
    );
    assert_eq!(
        page["columns"]["effective_to"],
        json!(["2027-06-01", null, null])
    );
    assert_eq!(
        page["columns"]["certainty"],
        json!(["interval", "unbounded", "unbounded"])
    );
    assert_eq!(
        page["columns"]["known_at"],
        json!(["2026-11-01", "2026-11-02", null])
    );
    assert_eq!(
        page["columns"]["source_kind"],
        json!([
            "szse_name_history",
            "szse_name_history",
            "szse_name_history"
        ])
    );
    assert_eq!(page["versions"][0]["status"], "present");
    let metadata = &page["metadata"];
    assert_eq!(metadata["verified_through"], "2026-11-02");
    let reason = metadata["coverage_reason"].as_str().unwrap();
    assert!(reason.contains("effective-time"), "{reason}");
    assert!(reason.contains("knowledge-time"), "{reason}");
    assert!(reason.contains("Shanghai"), "{reason}");

    // A PIT query known by the first Shanghai capture day sees only the row
    // first stored that day; the next day's row and the NULL-known_at row are
    // excluded, and the NULL one is reported instead of silently dropped.
    let first = query_security_names(&f, Some("2026-11-01"));
    assert_eq!(first["row_count"], 1);
    assert_eq!(first["columns"]["name"], json!(["ST旧名"]));
    assert_eq!(first["temporal"]["known_at"], "2026-11-01");
    assert_eq!(first["temporal"]["unknown_known_at_rows"], 1);
    assert_eq!(first["temporal"]["datasets_without_known_at"], json!([]));

    let second = query_security_names(&f, Some("2026-11-02"));
    assert_eq!(second["row_count"], 2);
    assert_eq!(second["columns"]["name"], json!(["ST旧名", "新名"]));
    assert_eq!(second["temporal"]["unknown_known_at_rows"], 1);
}

/// D-76: ETF observed names share the table but have no read surface yet
/// (PR-3). A query that names an ETF code gets the answer it always got (no
/// rows), and the A-share answer is the same with or without ETF rows stored.
#[test]
fn stored_etf_name_rows_do_not_change_the_security_names_read_face() {
    use crate::security_master::Exchange;
    use crate::security_master::names::{
        EtfNameSet, NameCertainty, NameInterval, NameSet, NameSourceKind,
    };
    use crate::sqlite_market::security_names::{EtfPart, SecurityNameWrite};

    let plain = fixture();
    seed_security_names(&plain);
    seed_names_refresh_run(&plain, "2026-11-02");
    let expected = query_security_names(&plain, None);

    let with_etf = fixture();
    seed_security_names(&with_etf);
    seed_names_refresh_run(&with_etf, "2026-11-02");
    let etf_row = |exchange: Exchange, code: &str| NameInterval {
        exchange,
        code: code.into(),
        effective_from: NaiveDate::from_ymd_opt(2026, 11, 5).unwrap(),
        effective_to: None,
        name: "沪深300ETF".into(),
        certainty: NameCertainty::Unbounded,
        known_at: Some(NaiveDate::from_ymd_opt(2026, 11, 5).unwrap()),
        source_kind: NameSourceKind::ExchangeSnapshot,
        source_record_id: Some(format!("sse_etf_list:{}", "a".repeat(64))),
    };
    let etf = EtfNameSet {
        rows: vec![
            etf_row(Exchange::Sh, "510300"),
            etf_row(Exchange::Sz, "159915"),
        ],
        entities: [
            (Exchange::Sh, "510300".to_string()),
            (Exchange::Sz, "159915".to_string()),
        ]
        .into_iter()
        .collect(),
    };
    // The same A-share rows the seed wrote, plus the ETF subset.
    let a_share = NameSet {
        rows: with_etf
            .backend
            .read(crate::sqlite_market::security::read_security_names)
            .unwrap()
            .iter()
            .map(|row| row.to_interval().unwrap())
            .collect(),
        coverage: Vec::new(),
        pending_sources: Vec::new(),
    };
    with_etf
        .backend
        .commit_security_names(&SecurityNameWrite {
            set: &a_share,
            etf: EtfPart::Replace(&etf),
            task_id: "task-names",
            item_id: "derived:security_names",
            generation_id: "gen-names",
            generation_manifest_sha256: &"41".repeat(32),
            member_sha256: &"42".repeat(32),
            security_event_sha256: &"43".repeat(32),
            generation_evidence: &json!({"generation_id": "gen-names"}),
        })
        .unwrap();

    let after = query_security_names(&with_etf, None);
    assert_eq!(after["columns"], expected["columns"]);
    assert_eq!(after["entities"], expected["entities"]);
    assert_eq!(after["row_count"], expected["row_count"]);

    // Asking for the ETF itself returns no row: no read face exists yet.
    let etf_query = with_etf
        .api
        .call(
            "query_data",
            json!({
                "dataset": "market.security_names",
                "contract_version": "v2",
                "selection": {"kind": "single",
                              "entity": {"exchange": "sh", "code": "510300"}},
                "filter": {"kind": "daily", "start_date": "2026-01-01",
                           "end_date": "2026-12-31", "as_of": "2026-12-31"},
                "version": {"kind": "latest_per_entity"}
            }),
        )
        .unwrap();
    assert_eq!(etf_query["row_count"], 0, "{etf_query}");
}

/// An empty database (no registration) answers `absent` with zero rows and no
/// metadata, including when the caller passes a `known_at` cutoff: the read
/// path never panics and never invents a name or a version.
#[test]
fn security_names_read_face_is_absent_before_any_write() {
    let f = fixture();
    let page = query_security_names(&f, Some("2026-11-01"));
    assert_eq!(page["layout"], "table");
    assert_eq!(page["row_count"], 0);
    assert_eq!(page["versions"][0]["status"], "absent");
    assert!(page.get("metadata").is_none(), "{page}");
    assert_eq!(page["temporal"]["unknown_known_at_rows"], 0);
}

/// AC-M3-1/§10.3: `universe.scope=sh_sz_a_listed_ever` resolves the member set
/// from the main table, including a code the catalog does not hold, and no
/// catalog version is echoed. All three read tools agree on the members.
#[test]
fn market_daily_universe_scope_resolves_the_main_table() {
    let f = fixture();
    seed_daily_pair(&f);
    publish_catalog(
        &f,
        "catalog-scope",
        &[("600006", crate::catalog::Exchange::Shanghai)],
    );
    // 000003 is not in the catalog and has no series: it must still be a
    // member, because the scope is the historical member set (absent rows are
    // a per-series answer, not a reason to drop the entity).
    seed_security_master(
        &f,
        &[
            ("sh.600006", None),
            ("sz.000001", None),
            (
                "sz.000003",
                Some(NaiveDate::from_ymd_opt(2019, 6, 1).unwrap()),
            ),
        ],
    );
    let selection = json!({"kind":"universe","scope":"sh_sz_a_listed_ever"});
    let page = f
        .api
        .call(
            "query_data",
            json!({
                "dataset":"market.daily",
                "contract_version":"v2",
                "selection":selection,
                "filter":{"kind":"daily","start_date":"2020-01-06","end_date":"2020-01-06"},
                "version":{"kind":"latest_per_entity"}
            }),
        )
        .unwrap();
    assert_eq!(
        page["entities"],
        json!(["sh.600006", "sz.000001", "sz.000003"]),
        "main-table members, sorted"
    );
    assert!(page.get("catalog_version").is_none(), "{page}");

    let versions = f
        .api
        .call(
            "list_data_versions",
            json!({
                "dataset":"market.daily",
                "contract_version":"v2",
                "selection":selection,
            }),
        )
        .unwrap();
    // The version list is per published series, so the absent members have no
    // row (the tool resolves the same member set, then reads what exists).
    let mut listed: Vec<&str> = versions["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["entity"].as_str().unwrap())
        .collect();
    listed.sort();
    assert_eq!(listed, vec!["sh.600006"]);

    let availability = f
        .api
        .call(
            "get_data_availability",
            json!({
                "dataset":"market.daily",
                "contract_version":"v2",
                "selection":selection,
                "filter":{"kind":"daily","start_date":"2020-01-06","end_date":"2020-01-06"},
                "version":{"kind":"latest_per_entity"}
            }),
        )
        .unwrap();
    // A universe availability is one aggregate row, not one row per member.
    assert_eq!(availability["columns"]["universe"][0]["entities"], 3);
    assert_eq!(
        availability["columns"]["universe"][0]["present"], 1,
        "only the seeded series is present"
    );
    assert_eq!(availability["columns"]["universe"][0]["absent"], 2);
}

/// §10.3: the scope is `market.daily`-only, cannot be combined with a catalog
/// version, rejects the catalog default, and needs contract_version v2.
#[test]
fn universe_scope_is_rejected_where_the_design_says_so() {
    let f = fixture();
    seed_daily_pair(&f);
    publish_catalog(
        &f,
        "catalog-scope",
        &[("600006", crate::catalog::Exchange::Shanghai)],
    );
    seed_security_master(&f, &[("sh.600006", None)]);
    let query = |selection: Value, contract: &str| {
        f.api
            .call(
                "query_data",
                json!({
                    "dataset":"market.daily",
                    "contract_version":contract,
                    "selection":selection,
                    "filter":{"kind":"daily","start_date":"2020-01-06","end_date":"2020-01-06"},
                    "version":{"kind":"latest_per_entity"}
                }),
            )
            .unwrap_err()
    };
    let error = query(
        json!({"kind":"universe","catalog_version":"catalog-scope","scope":"sh_sz_a_listed_ever"}),
        "v2",
    );
    assert_eq!(error.code, "invalid_arguments", "{error:?}");
    assert!(
        error.message.contains("mutually exclusive"),
        "{}",
        error.message
    );
    let error = query(json!({"kind":"universe","scope":"current_sh_sz_a"}), "v2");
    assert!(
        error.message.contains("current_sh_sz_a"),
        "{}",
        error.message
    );
    // v1 rejects the universe selection itself, so it cannot acquire the scope.
    let error = query(
        json!({"kind":"universe","scope":"sh_sz_a_listed_ever"}),
        "v1",
    );
    assert_eq!(error.code, "invalid_arguments");
    assert!(
        error.message.contains("requires contract_version v2"),
        "{}",
        error.message
    );
    // Only market.daily may resolve the listed-ever scope.
    let error = f
        .api
        .call(
            "query_data",
            json!({
                "dataset":"market.security_master",
                "contract_version":"v2",
                "selection":{"kind":"universe","scope":"sh_sz_a_listed_ever"},
                "filter":{"kind":"daily","start_date":"2020-01-06","end_date":"2020-01-06"},
                "version":{"kind":"latest_per_entity"}
            }),
        )
        .unwrap_err();
    assert_eq!(error.code, "invalid_arguments");
    assert!(
        error.message.contains("only supported for market.daily"),
        "{}",
        error.message
    );
    // list_data_versions accepts `universe` in v1, but not the v2 scope field.
    let error = f
        .api
        .call(
            "list_data_versions",
            json!({
                "dataset":"market.daily",
                "contract_version":"v1",
                "selection":{"kind":"universe","scope":"sh_sz_a_listed_ever"},
            }),
        )
        .unwrap_err();
    assert_eq!(error.code, "invalid_arguments");
    assert!(
        error
            .message
            .contains("selection.scope requires contract_version v2"),
        "{}",
        error.message
    );
}

/// #582 AC: a refresh must not make other read tools wait. The refresh runs in
/// a background thread of the service, so the caller's session thread is free
/// the moment the run row exists.
///
/// The synchronisation point is deterministic: the fake transport blocks the
/// run's first request on a condition variable, so "the refresh is in flight"
/// is a state the test waited for, never a duration it guessed. While the run
/// is held there, the read tool must answer — and the run must still be held
/// after it answers, otherwise the measurement would prove nothing.
#[test]
fn a_read_tool_answers_while_a_refresh_run_is_in_flight() {
    use crate::exchange_source::transport::with_helper_transport;
    use crate::exchange_source::{HttpRequest, HttpResponse, Transport, TransportError};
    use crate::source_store::{PolicyEnv, TEST_ENV_LOCK};
    use std::sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::{Duration, Instant};

    /// Serves one valid SSE listing page per request, but holds every request
    /// until the test releases it.
    struct HeldTransport {
        entered: (Mutex<bool>, Condvar),
        released: (Mutex<bool>, Condvar),
        calls: AtomicUsize,
    }

    impl HeldTransport {
        fn new() -> Self {
            Self {
                entered: (Mutex::new(false), Condvar::new()),
                released: (Mutex::new(false), Condvar::new()),
                calls: AtomicUsize::new(0),
            }
        }

        /// Block until the background run is inside its first request.
        fn wait_entered(&self) {
            let mut entered = self.entered.0.lock().unwrap();
            while !*entered {
                entered = self.entered.1.wait(entered).unwrap();
            }
        }

        fn is_released(&self) -> bool {
            *self.released.0.lock().unwrap()
        }

        fn release(&self) {
            *self.released.0.lock().unwrap() = true;
            self.released.1.notify_all();
        }
    }

    impl Transport for HeldTransport {
        fn get(&self, _request: &HttpRequest) -> Result<HttpResponse, TransportError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.entered.0.lock().unwrap() = true;
            self.entered.1.notify_all();
            let mut released = self.released.0.lock().unwrap();
            while !*released {
                released = self.released.1.wait(released).unwrap();
            }
            Ok(HttpResponse {
                status: 200,
                etag: None,
                last_modified: None,
                // A one-page SSE listing envelope: `single_page_only` refuses a
                // larger `pageCount`, and one row keeps the table non-empty.
                body: br#"{"pageHelp":{"beginPage":1,"pageNo":1,"pageSize":2000,"pageCount":1,"total":1,"data":[{"SECURITY_CODE":"600000"}]}}"#
                    .to_vec(),
            })
        }
    }

    let transport = Arc::new(HeldTransport::new());
    let held = transport.clone();
    let _env_lock = TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let store = tempfile::tempdir().unwrap();
    let _env = PolicyEnv::set(Some(store.path()), false);
    let f = fixture();

    with_helper_transport(transport, || {
        let reply = f
            .api
            .call(
                "refresh_exchange_sources",
                json!({"mode": "tail", "sources": ["sse_listing"]}),
            )
            .unwrap();
        // The reply is a handle, not the result: the run continues.
        assert_eq!(reply["status"], json!("running"));
        assert!(reply["finished_at"].is_null());
        assert_eq!(reply["requests"], json!(0));
        assert!(!reply["planned"].as_array().unwrap().is_empty());
        let run_id = reply["run_id"].as_str().expect("a run id").to_string();

        // Deterministic sync point: the run is inside its first request.
        held.wait_entered();
        assert!(!held.is_released(), "the run is held before the read");

        let started = Instant::now();
        let capabilities = f.api.call("get_data_capabilities", json!({})).unwrap();
        let elapsed = started.elapsed();
        assert!(
            capabilities.is_object(),
            "the read tool must answer while the refresh runs"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "a read tool waited {elapsed:?} behind a refresh run"
        );
        // Still in flight after the read: the run was never waiting on the read
        // path, and the read was not served after the run finished.
        assert!(!held.is_released(), "the refresh was still running");

        held.release();

        // AC: after the client's own budget expires, the run id still resolves
        // to the terminal result.
        let deadline = Instant::now() + Duration::from_secs(60);
        let report = loop {
            let report = f
                .api
                .call("refresh_exchange_sources", json!({"status_of": run_id}))
                .unwrap();
            if report["status"] != json!("running") {
                break report;
            }
            assert!(
                Instant::now() < deadline,
                "refresh run {run_id} never closed"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(report["status"], json!("succeeded"), "{report}");
        assert!(report["finished_at"].is_string());
        assert!(
            report["requests"].as_u64().unwrap() > 0,
            "the durable record carries the counters the reply could not"
        );
        assert!(
            held.calls.load(Ordering::SeqCst) >= 2,
            "acquire + page-1 guard"
        );
    });
}

#[test]
fn refresh_status_of_is_a_read_and_rejects_combined_arguments() {
    let f = fixture();
    // `status_of` never starts a run, so it must not silently ignore a mode.
    let error = f
        .api
        .call(
            "refresh_exchange_sources",
            json!({"status_of": "refresh-full-1", "mode": "tail"}),
        )
        .unwrap_err();
    assert_eq!(error.code, "invalid_arguments");
    assert!(
        error.message.contains("status_of cannot be combined"),
        "{}",
        error.message
    );
    let error = f
        .api
        .call(
            "refresh_exchange_sources",
            json!({"status_of": "refresh-full-1"}),
        )
        .unwrap_err();
    assert_eq!(error.code, "refresh_run_not_found");
    assert_eq!(
        error.details.as_deref(),
        Some(&json!({"run_id": "refresh-full-1"}))
    );
    // A start without a mode is refused before touching the store.
    let error = f
        .api
        .call("refresh_exchange_sources", json!({}))
        .unwrap_err();
    assert_eq!(error.code, "invalid_arguments");
    assert!(
        error.message.contains("mode is required"),
        "{}",
        error.message
    );

    // The dry run needs neither a source store nor a run record.
    let reply = f
        .api
        .call(
            "refresh_exchange_sources",
            json!({"mode": "full", "dry_run": true}),
        )
        .unwrap();
    assert_eq!(reply["status"], json!("planned"));
    assert!(reply["run_id"].is_null());
    assert!(reply["finished_at"].is_string());
    // The plan count follows the run's date (bulletin quarters), so only that
    // a full-mode plan was listed is asserted here.
    assert!(!reply["planned"].as_array().unwrap().is_empty());
    assert!(f.backend.read_source_refresh_runs().unwrap().is_empty());
}

/// M3-2 PR-C: ETF 口径 failures are stable wire error codes, not a silent raw
/// answer and not an empty page (D-70 Q4).
#[test]
fn etf_adjustment_failures_have_stable_error_codes() {
    let missing = super::mapped(crate::market_storage::Error::Invalid(
        "adjustment_required_for_etf: specify adjustment".into(),
    ));
    assert_eq!(missing.code, "adjustment_required_for_etf");
}

/// `#607` PR-D (D-92 Q7 (a), design §3.2/§6.3): the ETF 前复权 block keeps the
/// existing code and puts the reason in `details`. One case per reason code, so
/// the envelope shape is pinned for every value a client can branch on.
#[test]
fn a_blocked_etf_qfq_carries_the_reason_in_details() {
    use crate::source_store::etf_factor_gate::BlockReason;
    let reasons = [
        BlockReason::FactorUnverified,
        BlockReason::EventOrderUnverified,
        BlockReason::EventCategoryUnexpected,
        BlockReason::FactorUnparsable,
        BlockReason::QfqNotBuilt,
    ];
    for reason in reasons {
        let error = super::mapped(crate::market_storage::Error::AdjustmentUnavailable {
            message: "ETF sh.510050 has no verified 前复权 series".into(),
            reason: reason.as_str(),
            blocking_events: 12,
            categories: vec![1, 11],
        });
        assert_eq!(error.code, "adjustment_unavailable", "{reason}");
        assert_eq!(
            error.message, "adjustment_unavailable: ETF sh.510050 has no verified 前复权 series",
            "{reason}: the code prefix is part of the message, as before"
        );
        let details = error.details.expect("details").to_string();
        assert_eq!(
            details,
            json!({
                "reason": reason.as_str(),
                "blocking_events": 12,
                "categories": [1, 11],
            })
            .to_string(),
            "{reason}: the §3.2 details shape"
        );
    }
    // The digest of the check set is the only extra key on an admitted read,
    // and it never appears on an error.
    let admitted = crate::data_supply::wire::Metadata {
        source: "s".into(),
        published_at: chrono::Utc::now(),
        coverage_status: "complete".into(),
        coverage_reason: String::new(),
        units: vec![],
        adjustment: None,
        lineage: None,
        index: None,
        verified_through: None,
        price_scale: Some(6),
        last_trade_date: None,
        delisting_date: None,
        etf_checks_digest: Some("f".repeat(64)),
    };
    let encoded = serde_json::to_value(&admitted).unwrap();
    assert_eq!(encoded["etf_checks_digest"], json!("f".repeat(64)));
    let mut without = admitted.clone();
    without.etf_checks_digest = None;
    assert!(
        serde_json::to_value(&without)
            .unwrap()
            .get("etf_checks_digest")
            .is_none(),
        "every other shape keeps its bytes"
    );
}

/// `get_data_capabilities` carries the series shapes as data: the ETF raw
/// shape is 3dp/份 with 100-share quantization, and it is never a default.
#[test]
fn capabilities_express_the_etf_raw_shape_and_units() {
    let f = fixture();
    let caps = f
        .api
        .call("get_data_capabilities", json!({"dataset": "market.daily"}))
        .unwrap();
    let etf = caps["series"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| {
            row["dataset"] == json!("market.daily") && row["instrument_class"] == json!("etf")
        })
        .expect("ETF daily shape is declared");
    assert_eq!(etf["adjustment"], json!("raw"));
    assert_eq!(etf["price_scale"], json!(3));
    assert_eq!(etf["price_unit"], json!("yuan"));
    assert_eq!(etf["volume_unit"], json!("share"));
    assert_eq!(etf["volume_quantum"], json!(100));
    assert_eq!(etf["default"], json!(false));
    // `#607` PR-D: the 前复权 shape is listed too (the capability exists; a
    // blocked entity is an entity-level fact, §6.3). It is not a default: an
    // ETF still needs an explicit `adjustment`.
    let qfq = caps["series"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| {
            row["dataset"] == json!("market.daily")
                && row["instrument_class"] == json!("etf")
                && row["adjustment"] == json!("qfq")
        })
        .expect("ETF 前复权 shape is declared");
    assert_eq!(qfq["price_scale"], json!(6));
    assert_eq!(qfq["price_unit"], json!("yuan"));
    assert_eq!(qfq["volume_unit"], json!("share"));
    assert_eq!(qfq["volume_quantum"], json!(100));
    assert_eq!(qfq["default"], json!(false));
    // The derivation labels come from code constants, not copies.
    assert_eq!(
        caps["algorithm_versions"],
        json!({
            "qfq": crate::qfq::ALGORITHM_VERSION,
            "etf_factor_checks": crate::source_store::etf_factor_checks::CHECKS_VERSION,
        })
    );
    assert_eq!(
        caps["capability_revision"],
        json!("generic-data-v12"),
        "the ETF 前复权 read surface is a wire-visible change (D-70 Q10)"
    );
    // The compact catalog keeps its bytes: neither block appears without a
    // dataset/operation request.
    let compact = f.api.call("get_data_capabilities", json!({})).unwrap();
    assert!(compact.get("series").is_none());
    assert!(compact.get("algorithm_versions").is_none());
    assert_eq!(compact["capability_revision"], json!("generic-data-v12"));
}

#[test]
fn memory_budget_refusal_has_its_own_code() {
    let refused = super::mapped(crate::market_storage::Error::Invalid(
        "memory_budget_exceeded: near the limit".into(),
    ));
    assert_eq!(refused.code, "memory_budget_exceeded");
}

// ---- D-75 narrow read tools (issue-525 PR-1) -------------------------------
//
// Every tool is a thin wrapper over `query_data` v2, so each test compares the
// tool's answer with the equivalent `query_data` call. Fixtures are small on
// purpose: a few entities and at most six calendar days.

/// The parts of a v2 answer that must be identical. `snapshot_id` and
/// `snapshot_time` differ per call by design, so they are left out.
fn same_answer(tool: &Value, query: &Value) {
    for key in [
        "layout",
        "columns",
        "entities",
        "dates",
        "trade_days",
        "row_count",
        "versions",
        "metadata",
        "temporal",
    ] {
        assert_eq!(tool.get(key), query.get(key), "{key}");
    }
}

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn mark_etf(f: &Fixture, code: &str) {
    let MarketBackend::Compact(repo) = &f.backend else {
        panic!("fixture must open the compact backend");
    };
    repo.write(|c| {
        c.execute(
            "UPDATE security_master SET instrument_class=2 WHERE exchange='sh' AND code=?1",
            [code],
        )
        .map_err(|e| crate::market_storage::Error::Storage(e.to_string()))?;
        Ok::<(), crate::market_storage::Error>(())
    })
    .unwrap();
}

#[test]
fn get_daily_bars_answers_like_query_data_v2() {
    let f = fixture();
    seed_daily_pair(&f);
    let tool = f
        .api
        .call(
            "get_daily_bars",
            json!({"securities":["sh.600006"],"start_date":"2020-01-06","end_date":"2020-01-06"}),
        )
        .unwrap();
    let query = f
        .api
        .call(
            "query_data",
            json!({
                "dataset":"market.daily","contract_version":"v2",
                "selection":{"kind":"single","entity":"sh.600006"},
                "filter":{"kind":"daily","start_date":"2020-01-06","end_date":"2020-01-06"},
                "version":{"kind":"latest_per_entity"}
            }),
        )
        .unwrap();
    assert_eq!(tool["layout"], "panel");
    assert_eq!(tool["columns"]["close"], json!([["10.05"]]));
    same_answer(&tool, &query);

    // `YYYYMMDD`, `fields` and an explicit qfq normalize like on query_data.
    let projected = f
        .api
        .call(
            "get_daily_bars",
            json!({"securities":["sh.600006"],"start_date":"20200106","end_date":"20200106",
                   "adjustment":"qfq","fields":["close"]}),
        )
        .unwrap();
    assert_eq!(projected["columns"]["close"], tool["columns"]["close"]);
    assert_eq!(projected["columns"].as_object().unwrap().len(), 1);

    // Paging goes through the same cursor.
    let page = f
        .api
        .call(
            "get_daily_bars",
            json!({"securities":["sh.600006"],"start_date":"2020-01-01","end_date":"2020-12-31","page_size":1}),
        )
        .unwrap();
    assert_eq!(page["row_count"], 1);
}

#[test]
fn get_daily_bars_points_an_index_at_get_index_bars_and_query_data_is_unchanged() {
    let f = fixture();
    let task_id = task(&f, "sh.000001", 1, "day", "req-index-redirect");
    commit_index(
        &f,
        &task_id,
        "item-1",
        "sh.000001",
        vec![bar(day_epoch(2020, 1, 6), 3000)],
    );
    for index in ["sh.000001", "sh.000300"] {
        let error = f
            .api
            .call(
                "get_daily_bars",
                json!({"securities":[index],"start_date":"2020-01-06","end_date":"2020-01-06"}),
            )
            .unwrap_err();
        assert_eq!(error.code, "invalid_arguments", "{index}");
        assert!(
            error.message.contains("get_index_bars"),
            "{}",
            error.message
        );
        assert!(error.message.contains(index), "{}", error.message);
    }
    // A mixed batch is refused as a whole rather than silently dropping the index.
    let mixed = f
        .api
        .call(
            "get_daily_bars",
            json!({"securities":["sh.600006","sh.000001"],"start_date":"2020-01-06","end_date":"2020-01-06"}),
        )
        .unwrap_err();
    assert_eq!(mixed.code, "invalid_arguments");
    // `query_data` keeps its old behaviour: absent, no error.
    let old = f
        .api
        .call(
            "query_data",
            json!({
                "dataset":"market.daily","contract_version":"v2",
                "selection":{"kind":"single","entity":"sh.000001"},
                "filter":{"kind":"daily","start_date":"2020-01-06","end_date":"2020-01-06"},
                "version":{"kind":"latest_per_entity"}
            }),
        )
        .unwrap();
    assert_eq!(old["versions"][0]["status"], "absent");
    assert_eq!(old["row_count"], 0);
}

#[test]
fn get_index_bars_answers_like_query_data_and_takes_only_registered_indices() {
    let f = fixture();
    let task_id = task(&f, "sh.000001", 1, "day", "req-index-bars");
    commit_index(
        &f,
        &task_id,
        "item-1",
        "sh.000001",
        vec![bar(day_epoch(2020, 1, 6), 3000)],
    );
    let tool = f
        .api
        .call(
            "get_index_bars",
            json!({"indices":["sh.000001"],"start_date":"2020-01-06","end_date":"2020-01-06"}),
        )
        .unwrap();
    let query = f
        .api
        .call(
            "query_data",
            json!({
                "dataset":"market.index","contract_version":"v2",
                "selection":{"kind":"single","entity":"sh.000001"},
                "filter":{"kind":"daily","start_date":"2020-01-06","end_date":"2020-01-06"},
                "version":{"kind":"latest_per_entity"}
            }),
        )
        .unwrap();
    assert_eq!(tool["columns"]["close"], json!([["30"]]));
    assert_eq!(tool["versions"][0]["status"], "present");
    same_answer(&tool, &query);

    for not_an_index in ["sh.600006", "sz.399300"] {
        let error = f
            .api
            .call(
                "get_index_bars",
                json!({"indices":[not_an_index],"start_date":"2020-01-06","end_date":"2020-01-06"}),
            )
            .unwrap_err();
        assert_eq!(error.code, "invalid_arguments", "{not_an_index}");
        assert!(
            error.message.contains("sh.000300"),
            "the message lists the registered indices: {}",
            error.message
        );
    }
}

#[test]
fn get_daily_bars_keeps_the_etf_and_batch_rules() {
    let f = fixture();
    seed_daily_pair(&f);
    seed_security_master(&f, &[("sh.600006", None), ("sh.510050", None)]);
    mark_etf(&f, "510050");
    let args = |securities: Value, extra: Value| {
        let mut input =
            json!({"securities":securities,"start_date":"2020-01-06","end_date":"2020-01-06"});
        for (key, value) in extra.as_object().into_iter().flatten() {
            input[key] = value.clone();
        }
        input
    };
    // An ETF must name its 口径 (D-70 Q3).
    let missing = f
        .api
        .call("get_daily_bars", args(json!(["sh.510050"]), json!({})))
        .unwrap_err();
    assert_eq!(missing.code, "adjustment_required_for_etf");
    let same = f
        .api
        .call(
            "query_data",
            json!({
                "dataset":"market.daily","contract_version":"v2",
                "selection":{"kind":"single","entity":"sh.510050"},
                "filter":{"kind":"daily","start_date":"2020-01-06","end_date":"2020-01-06"},
                "version":{"kind":"latest_per_entity"}
            }),
        )
        .unwrap_err();
    assert_eq!(missing.code, same.code);
    assert_eq!(missing.message, same.message);
    // qfq for an ETF is unavailable, not silently raw.
    let qfq = f
        .api
        .call(
            "get_daily_bars",
            args(json!(["sh.510050"]), json!({"adjustment":"qfq"})),
        )
        .unwrap_err();
    assert_eq!(qfq.code, "adjustment_unavailable");
    // The fixture stores no 前复权 series for this ETF, so the reason names
    // exactly that (PR-D: a distinct code from "stored but unverified").
    assert_eq!(
        qfq.details.as_ref().map(|d| d.to_string()).as_deref(),
        Some(
            json!({
                "reason": "etf_qfq_not_built",
                "blocking_events": 0,
                "categories": [],
            })
            .to_string()
            .as_str()
        ),
        "{qfq:?}"
    );
    // A mixed batch without an adjustment is refused because of its ETF member.
    let mixed = f
        .api
        .call(
            "get_daily_bars",
            args(json!(["sh.600006", "sh.510050"]), json!({})),
        )
        .unwrap_err();
    assert_eq!(mixed.code, "adjustment_required_for_etf");

    // 1..100 entities.
    let too_many: Vec<String> = (0..101).map(|n| format!("sh.{:06}", 600_000 + n)).collect();
    let error = f
        .api
        .call("get_daily_bars", args(json!(too_many), json!({})))
        .unwrap_err();
    assert_eq!(error.code, "invalid_arguments");
    assert!(error.message.contains("1..100"), "{}", error.message);
    let none = f
        .api
        .call("get_daily_bars", args(json!([]), json!({})))
        .unwrap_err();
    assert_eq!(none.code, "invalid_arguments");
}

#[test]
fn get_yearly_bars_answers_like_query_data_and_is_absent_for_an_etf() {
    let f = fixture();
    let daily_task = task(&f, SYMBOL, 1, "day", "req-yearly-tool-daily");
    let daily = publish_daily(
        &f,
        &daily_task,
        "item-1",
        SYMBOL,
        vec![bar(day_epoch(2020, 1, 6), 100)],
        None,
    );
    let yearly_task = task(&f, SYMBOL, 1, "year", "req-yearly-tool");
    commit_yearly_plan(
        &f,
        &yearly_task,
        "item-1",
        SYMBOL,
        None,
        &daily.version_id,
        None,
    );
    seed_security_master(&f, &[("sh.600006", None), ("sh.510050", None)]);
    mark_etf(&f, "510050");

    let tool = f
        .api
        .call(
            "get_yearly_bars",
            json!({"securities":["sh.600006","sh.510050"],"start_year":2019,"end_year":2030}),
        )
        .unwrap();
    let query = f
        .api
        .call(
            "query_data",
            json!({
                "dataset":"market.yearly","contract_version":"v2",
                "selection":{"kind":"batch","entities":["sh.510050","sh.600006"]},
                "filter":{"kind":"yearly","start_year":2019,"end_year":2030},
                "version":{"kind":"latest_per_entity"}
            }),
        )
        .unwrap();
    same_answer(&tool, &query);
    let status = |entity: &str| {
        tool["versions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["entity"] == json!(entity))
            .map(|row| row["status"].clone())
    };
    assert_eq!(status("sh.510050"), Some(json!("absent")));
    assert_eq!(status("sh.600006"), Some(json!("present")));

    // Daily-only fields and `adjustment` are not part of this tool.
    let error = f
        .api
        .call(
            "get_yearly_bars",
            json!({"securities":["sh.600006"],"start_year":2020,"end_year":2020,"adjustment":"qfq"}),
        )
        .unwrap_err();
    assert_eq!(error.code, "invalid_arguments");
    let error = f
        .api
        .call(
            "get_yearly_bars",
            json!({"securities":["sh.600006"],"start_year":2020,"end_year":2020,"fields":["no_trade"]}),
        )
        .unwrap_err();
    assert_eq!(error.code, "invalid_arguments");
}

#[test]
fn get_market_snapshot_answers_like_query_data_and_caps_the_window_at_five_days() {
    let f = fixture();
    seed_daily_pair(&f);
    let second = task(&f, "sz.000001", 1, "day", "req-snap-second");
    publish_daily(
        &f,
        &second,
        "item-1",
        "sz.000001",
        vec![bar(day_epoch(2020, 1, 6), 2000)],
        None,
    );
    publish_catalog(
        &f,
        "catalog-snap",
        &[
            ("600006", crate::catalog::Exchange::Shanghai),
            ("000001", crate::catalog::Exchange::Shenzhen),
        ],
    );
    let days: Vec<NaiveDate> = (6..=13)
        .map(|d| day(2020, 1, d))
        .filter(|d| {
            use chrono::Datelike;
            !matches!(d.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun)
        })
        .collect();
    assert_eq!(days.len(), 6);
    commit_calendar_days(&f, &days);

    let tool = f
        .api
        .call(
            "get_market_snapshot",
            json!({"start_date":"2020-01-06","end_date":"2020-01-06","fields":["close"]}),
        )
        .unwrap();
    let query = f
        .api
        .call(
            "query_data",
            json!({
                "dataset":"market.daily","contract_version":"v2",
                "selection":{"kind":"universe"},
                "filter":{"kind":"cross_section","start_date":"2020-01-06","end_date":"2020-01-06"},
                "version":{"kind":"latest_per_entity"},
                "fields":["close"]
            }),
        )
        .unwrap();
    assert_eq!(tool["layout"], "cross_section");
    assert_eq!(tool["entities"], json!(["sh.600006", "sz.000001"]));
    assert_eq!(tool["columns"]["close"], json!([["10.05", "20"]]));
    same_answer(&tool, &query);

    // Five trading days are allowed, six are refused by the shared check, and
    // the wrapper does not change that.
    let five = f
        .api
        .call(
            "get_market_snapshot",
            json!({"start_date":"2020-01-06","end_date":"2020-01-10","fields":["close"]}),
        )
        .unwrap();
    assert_eq!(five["trade_days"].as_array().unwrap().len(), 5);
    let six = f
        .api
        .call(
            "get_market_snapshot",
            json!({"start_date":"2020-01-06","end_date":"2020-01-13","fields":["close"]}),
        )
        .unwrap_err();
    assert_eq!(six.code, "invalid_arguments");
    assert!(six.message.contains("6 trading days"), "{}", six.message);
    assert!(six.message.contains("get_daily_bars") || six.message.contains("batch"));

    // ETF universes need an explicit raw 口径, like the daily tool.
    seed_security_master(&f, &[("sh.600006", None), ("sh.510050", None)]);
    mark_etf(&f, "510050");
    let etf = f
        .api
        .call(
            "get_market_snapshot",
            json!({"start_date":"2020-01-06","end_date":"2020-01-06","universe":"etf_current"}),
        )
        .unwrap_err();
    assert_eq!(etf.code, "adjustment_required_for_etf");
}

#[test]
fn get_trading_calendar_answers_like_query_data() {
    let f = fixture();
    commit_calendar_days(&f, &[day(2020, 1, 6), day(2020, 1, 7)]);
    let tool = f
        .api
        .call(
            "get_trading_calendar",
            json!({"start_date":"2020-01-06","end_date":"2020-01-07"}),
        )
        .unwrap();
    let query = f
        .api
        .call(
            "query_data",
            json!({
                "dataset":"market.calendar","contract_version":"v2",
                "selection":{"kind":"market"},
                "filter":{"kind":"daily","start_date":"2020-01-06","end_date":"2020-01-07"},
                "version":{"kind":"latest_per_entity"}
            }),
        )
        .unwrap();
    assert_eq!(
        tool["columns"]["trade_day"],
        json!(["2020-01-06", "2020-01-07"])
    );
    same_answer(&tool, &query);
    // The tool pages like the others, but takes no `fields`.
    let error = f
        .api
        .call(
            "get_trading_calendar",
            json!({"start_date":"2020-01-06","end_date":"2020-01-07","fields":["trade_day"]}),
        )
        .unwrap_err();
    assert_eq!(error.code, "invalid_arguments");
}

#[test]
fn get_trading_calendar_pages_through_a_long_range_like_query_data() {
    let f = fixture();
    let days: Vec<NaiveDate> = (6..=10).map(|d| day(2020, 1, d)).collect();
    commit_calendar_days(&f, &days);
    let query = f
        .api
        .call(
            "query_data",
            json!({
                "dataset":"market.calendar","contract_version":"v2",
                "selection":{"kind":"market"},
                "filter":{"kind":"daily","start_date":"2020-01-06","end_date":"2020-01-10"},
                "version":{"kind":"latest_per_entity"}
            }),
        )
        .unwrap();
    assert_eq!(query["row_count"], 5);

    let mut seen: Vec<Value> = Vec::new();
    let mut pages = 0;
    let mut cursor: Option<Value> = None;
    loop {
        let mut input = json!({"start_date":"2020-01-06","end_date":"2020-01-10","page_size":2});
        if let Some(cursor) = &cursor {
            input["cursor"] = cursor.clone();
        }
        let page = f.api.call("get_trading_calendar", input).unwrap();
        pages += 1;
        assert!(page["row_count"].as_u64().unwrap() <= 2, "{page}");
        seen.extend(page["columns"]["trade_day"].as_array().unwrap().clone());
        if page["page_complete"] == json!(true) {
            assert!(page["next_cursor"].is_null(), "{page}");
            break;
        }
        cursor = Some(page["next_cursor"].clone());
        assert!(cursor.as_ref().unwrap().is_string(), "{page}");
    }
    assert_eq!(pages, 3);
    assert_eq!(Value::Array(seen), query["columns"]["trade_day"]);

    // The default page still returns the whole short range.
    let whole = f
        .api
        .call(
            "get_trading_calendar",
            json!({"start_date":"2020-01-06","end_date":"2020-01-10"}),
        )
        .unwrap();
    assert_eq!(whole["page_complete"], true);
    same_answer(&whole, &query);
}

fn commit_rule_book(f: &Fixture) {
    let MarketBackend::Compact(repo) = &f.backend else {
        panic!("fixture must open the compact backend");
    };
    let book = crate::trading_rules::rule_book().unwrap();
    repo.commit_trading_rules(&crate::sqlite_market::rules::TradingRulesWrite {
        book: &book,
        task_id: "task-rules",
        item_id: "derived:price_limit_rules",
        generation_id: "gen-1",
        generation_manifest_sha256: &"0b".repeat(32),
        member_sha256: &"0c".repeat(32),
        security_event_sha256: &"0e".repeat(32),
        generation_evidence: &json!({"fixture": true}),
    })
    .unwrap();
}

#[test]
fn get_price_limit_rules_defaults_as_of_to_the_last_trading_day() {
    let f = fixture();
    // The seeded calendar ends in 2020, so "the last trading day up to today"
    // is its last day, whatever the host clock says.
    commit_calendar_days(&f, &[day(2020, 1, 6), day(2020, 1, 7)]);
    commit_rule_book(&f);

    let default = f.api.call("get_price_limit_rules", json!({})).unwrap();
    assert_eq!(default["temporal"]["as_of"], "2020-01-07");
    assert!(default["row_count"].as_u64().unwrap() > 0, "{default}");

    let explicit = f
        .api
        .call("get_price_limit_rules", json!({"as_of":"2020-01-07"}))
        .unwrap();
    same_answer(&default, &explicit);

    let query = f
        .api
        .call(
            "query_data",
            json!({
                "dataset":"market.price_limit_rules","contract_version":"v2",
                "selection":{"kind":"market"},
                "filter":{"kind":"daily","start_date":"2020-01-07","end_date":"2020-01-07","as_of":"2020-01-07"},
                "version":{"kind":"latest_per_entity"}
            }),
        )
        .unwrap();
    same_answer(&default, &query);

    // An earlier as_of reads the rules in effect then.
    let earlier = f
        .api
        .call("get_price_limit_rules", json!({"as_of":"2020-01-06"}))
        .unwrap();
    assert_eq!(earlier["temporal"]["as_of"], "2020-01-06");

    // `as_of` is a real field of this tool; `known_at` is not.
    let error = f
        .api
        .call(
            "get_price_limit_rules",
            json!({"as_of":"2020-01-07","known_at":"2020-01-07"}),
        )
        .unwrap_err();
    assert_eq!(error.code, "invalid_arguments");
}

#[test]
fn find_securities_answers_like_query_data_catalog() {
    let f = fixture();
    publish_catalog(
        &f,
        "catalog-find",
        &[
            ("600006", crate::catalog::Exchange::Shanghai),
            ("000001", crate::catalog::Exchange::Shenzhen),
        ],
    );
    let tool = f
        .api
        .call("find_securities", json!({"query":"600006"}))
        .unwrap();
    let query = f
        .api
        .call(
            "query_data",
            json!({
                "dataset":"securities.catalog","contract_version":"v2",
                "selection":{"kind":"catalog","scope":"current_sh_sz_a"},
                "filter":{"kind":"catalog","name_query":"600006"},
                "version":{"kind":"latest_per_entity"}
            }),
        )
        .unwrap();
    assert_eq!(tool["entities"], json!(["sh.600006"]));
    same_answer(&tool, &query);

    let by_code = f
        .api
        .call("find_securities", json!({"code":"000001"}))
        .unwrap();
    assert_eq!(by_code["entities"], json!(["sz.000001"]));
    let all = f.api.call("find_securities", json!({})).unwrap();
    assert_eq!(all["entities"], json!(["sh.600006", "sz.000001"]));
    let paged = f
        .api
        .call("find_securities", json!({"page_size":1}))
        .unwrap();
    assert_eq!(paged["row_count"], 1);
    assert!(paged["next_cursor"].is_string());
    let next = f
        .api
        .call(
            "find_securities",
            json!({"page_size":1,"cursor":paged["next_cursor"]}),
        )
        .unwrap();
    assert_eq!(next["entities"], json!(["sz.000001"]));
}

#[test]
fn every_read_tool_rejects_unknown_fields() {
    let f = fixture();
    let valid = [
        ("find_securities", json!({})),
        (
            "get_daily_bars",
            json!({"securities":["sh.600006"],"start_date":"2020-01-06","end_date":"2020-01-06"}),
        ),
        (
            "get_yearly_bars",
            json!({"securities":["sh.600006"],"start_year":2020,"end_year":2020}),
        ),
        (
            "get_index_bars",
            json!({"indices":["sh.000001"],"start_date":"2020-01-06","end_date":"2020-01-06"}),
        ),
        (
            "get_market_snapshot",
            json!({"start_date":"2020-01-06","end_date":"2020-01-06"}),
        ),
        (
            "get_trading_calendar",
            json!({"start_date":"2020-01-06","end_date":"2020-01-06"}),
        ),
        ("get_price_limit_rules", json!({"as_of":"2020-01-06"})),
        (
            "get_security_state",
            json!({"securities":["sz.000001"],"fact":"security_names","as_of":"2020-01-06"}),
        ),
        (
            "get_security_events",
            json!({"securities":["sh.600006"],"event":"capital","start_date":"2020-01-06","end_date":"2020-01-06"}),
        ),
    ];
    for (name, input) in valid {
        // Values are valid for the internal `Query`, so a rejection can only
        // come from the tool's own closed input type.
        let extras = [
            ("dataset", json!("market.daily")),
            ("contract_version", json!("v1")),
            ("selection", json!({"kind":"universe"})),
            (
                "filter",
                json!({"kind":"daily","start_date":"2020-01-06","end_date":"2020-01-06"}),
            ),
            ("version", json!({"kind":"latest_per_entity"})),
            ("bogus", json!("x")),
        ];
        for (extra, value) in extras {
            let mut input = input.clone();
            input[extra] = value;
            let error = f.api.call(name, input).unwrap_err();
            assert_eq!(error.code, "invalid_arguments", "{name} accepted {extra}");
        }
    }
    // The internal query shape is not exposed: only the tools that declare
    // `known_at` take it.
    let error = f
        .api
        .call(
            "get_daily_bars",
            json!({"securities":["sh.600006"],"start_date":"2020-01-06","end_date":"2020-01-06","known_at":"2020-01-06"}),
        )
        .unwrap_err();
    assert_eq!(error.code, "invalid_arguments");
}

#[test]
fn the_default_as_of_day_is_the_shanghai_day() {
    // `query::today()` feeds the default `as_of`; it must not follow the host
    // timezone (vm2-126 is Asia/Shanghai, aws-node is UTC).
    assert_eq!(
        crate::data_supply::query::today(),
        crate::calendar::shanghai_today()
    );
}

// ---- PR-1b: get_security_state and get_security_events ----------------------
//
// Fixtures are invented: `seed_security_names` holds three SZ name intervals,
//   sz.000001 "ST旧名"  2026-01-05 .. 2027-06-01 (end excluded), known 2026-11-01
//   sz.000002 "新名"    2026-04-01 ..             open,           known 2026-11-02
//   sz.000003 "无日期"  2026-01-01 ..             open,           known unknown

fn state_query(entities: Value, as_of: &str, known_at: Option<&str>) -> Value {
    let mut request = json!({
        "dataset": "market.security_names",
        "contract_version": "v2",
        "selection": {"kind": "batch", "entities": entities},
        "filter": {"kind": "daily", "start_date": as_of, "end_date": as_of, "as_of": as_of},
        "version": {"kind": "latest_per_entity"}
    });
    if let Some(day) = known_at {
        request["known_at"] = json!(day);
    }
    request
}

#[test]
fn get_security_state_answers_like_query_data_for_every_fact() {
    let f = fixture();
    seed_security_names(&f);
    seed_names_refresh_run(&f, "2026-11-02");
    let both = json!(["sz.000001", "sz.000002"]);

    let tool = f
        .api
        .call(
            "get_security_state",
            json!({"securities":both,"fact":"security_names","as_of":"2026-12-31"}),
        )
        .unwrap();
    let query = f
        .api
        .call("query_data", state_query(both.clone(), "2026-12-31", None))
        .unwrap();
    assert_eq!(tool["columns"]["name"], json!(["ST旧名", "新名"]));
    assert!(tool["temporal"].is_object(), "{tool}");
    assert_eq!(tool["temporal"]["as_of"], "2026-12-31");
    same_answer(&tool, &query);

    // `known_at` is a top-level field and reaches the query unchanged.
    let tool = f
        .api
        .call(
            "get_security_state",
            json!({"securities":both,"fact":"security_names","as_of":"2026-12-31",
                   "known_at":"2026-11-01"}),
        )
        .unwrap();
    let query = f
        .api
        .call(
            "query_data",
            state_query(both, "2026-12-31", Some("2026-11-01")),
        )
        .unwrap();
    same_answer(&tool, &query);
    assert_eq!(tool["temporal"]["known_at"], "2026-11-01");

    // Each `fact` value maps to its own dataset and takes the same arguments.
    for (fact, dataset) in [
        ("security_master", "market.security_master"),
        ("security_names", "market.security_names"),
        ("risk_warnings", "market.risk_warnings"),
        ("suspensions", "market.suspensions"),
        ("industry", "market.industry_assignments"),
    ] {
        let tool = f
            .api
            .call(
                "get_security_state",
                json!({"securities":["sz.000001"],"fact":fact,"as_of":"2026-06-01"}),
            )
            .unwrap();
        assert_eq!(tool["dataset"], dataset, "{fact}");
        let query = f
            .api
            .call(
                "query_data",
                json!({
                    "dataset":dataset,"contract_version":"v2",
                    "selection":{"kind":"single","entity":"sz.000001"},
                    "filter":{"kind":"daily","start_date":"2026-06-01","end_date":"2026-06-01","as_of":"2026-06-01"},
                    "version":{"kind":"latest_per_entity"}
                }),
            )
            .unwrap();
        same_answer(&tool, &query);
    }
}

#[test]
fn get_security_state_known_at_hides_rows_learned_later() {
    let f = fixture();
    seed_security_names(&f);
    let all = json!(["sz.000001", "sz.000002", "sz.000003"]);
    let call = |known_at: Option<&str>| {
        let mut input = json!({"securities":all,"fact":"security_names","as_of":"2026-12-31"});
        if let Some(day) = known_at {
            input["known_at"] = json!(day);
        }
        f.api.call("get_security_state", input).unwrap()
    };
    let now = call(None);
    assert_eq!(now["columns"]["name"], json!(["ST旧名", "新名", "无日期"]));
    // Known by 2026-11-01: the 11-02 row and the row without a known date drop out.
    let first = call(Some("2026-11-01"));
    assert_eq!(first["columns"]["name"], json!(["ST旧名"]));
    assert_eq!(first["temporal"]["unknown_known_at_rows"], 1);
    let second = call(Some("2026-11-02"));
    assert_eq!(second["columns"]["name"], json!(["ST旧名", "新名"]));
    // Before anything was known, nothing is returned.
    let before = call(Some("2026-10-31"));
    assert_eq!(before["row_count"], 0);
}

#[test]
fn get_security_state_does_not_match_on_the_interval_end_day() {
    let f = fixture();
    seed_security_names(&f);
    let call = |as_of: &str| {
        f.api
            .call(
                "get_security_state",
                json!({"securities":["sz.000001"],"fact":"security_names","as_of":as_of}),
            )
            .unwrap()
    };
    // sz.000001's name interval ends on 2027-06-01, and the end day is excluded.
    assert_eq!(call("2027-05-31")["row_count"], 1);
    assert_eq!(call("2027-06-01")["row_count"], 0);
    // The first day is included; the day before is not.
    assert_eq!(call("2026-01-05")["row_count"], 1);
    assert_eq!(call("2026-01-04")["row_count"], 0);
}

#[test]
fn get_security_state_defaults_as_of_to_the_last_trading_day_in_both_ends() {
    let f = fixture();
    seed_security_names(&f);
    // The seeded calendar ends in the past, so "the last trading day up to
    // today" is its last day whatever the host clock says.
    commit_calendar_days(&f, &[day(2026, 1, 5), day(2026, 1, 6)]);
    let default = f
        .api
        .call(
            "get_security_state",
            json!({"securities":["sz.000001"],"fact":"security_names"}),
        )
        .unwrap();
    assert_eq!(default["temporal"]["as_of"], "2026-01-06");
    assert_eq!(default["columns"]["name"], json!(["ST旧名"]));
    let query = f
        .api
        .call(
            "query_data",
            state_query(json!(["sz.000001"]), "2026-01-06", None),
        )
        .unwrap();
    same_answer(&default, &query);
    // The same call with the day spelled out is the same answer.
    let explicit = f
        .api
        .call(
            "get_security_state",
            json!({"securities":["sz.000001"],"fact":"security_names","as_of":"2026-01-06"}),
        )
        .unwrap();
    same_answer(&default, &explicit);
}

#[test]
fn get_security_state_says_that_name_history_covers_sz_only() {
    const NOTE: &str = "name history covers SZ only (D-61)";
    let f = fixture();
    seed_security_names(&f);
    let notes = |page: &Value| -> Vec<String> {
        page["limitations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.as_str().unwrap().to_owned())
            .collect()
    };
    let has_note = |page: &Value| notes(page).iter().any(|n| n.contains(NOTE));

    // An SH code answers 0 rows; the note says that is not "no name".
    let sh = f
        .api
        .call(
            "get_security_state",
            json!({"securities":["sh.600006"],"fact":"security_names","as_of":"2026-12-31"}),
        )
        .unwrap();
    assert_eq!(sh["row_count"], 0);
    assert!(has_note(&sh), "{:?}", notes(&sh));
    // The existing limitations stay; the note is added once.
    assert!(notes(&sh).len() >= 2);
    assert_eq!(notes(&sh).iter().filter(|n| n.contains(NOTE)).count(), 1);

    // A mixed batch carries it too, and the SZ rows are still returned.
    let mixed = f
        .api
        .call(
            "get_security_state",
            json!({"securities":["sh.600006","sz.000001"],"fact":"security_names","as_of":"2026-12-31"}),
        )
        .unwrap();
    assert_eq!(mixed["columns"]["name"], json!(["ST旧名"]));
    assert!(has_note(&mixed));

    // SZ only, or another fact for an SH code: no note.
    let sz = f
        .api
        .call(
            "get_security_state",
            json!({"securities":["sz.000001"],"fact":"security_names","as_of":"2026-12-31"}),
        )
        .unwrap();
    assert!(!has_note(&sz));
    let other = f
        .api
        .call(
            "get_security_state",
            json!({"securities":["sh.600006"],"fact":"risk_warnings","as_of":"2026-12-31"}),
        )
        .unwrap();
    assert!(!has_note(&other));
    // `query_data` is unchanged.
    let query = f
        .api
        .call(
            "query_data",
            state_query(json!(["sh.600006"]), "2026-12-31", None),
        )
        .unwrap();
    assert!(!has_note(&query));
}

#[test]
fn get_security_state_rejects_bad_input() {
    let f = fixture();
    let call = |input: Value| f.api.call("get_security_state", input).unwrap_err();
    assert_eq!(
        call(json!({"securities":["sz.000001"],"fact":"capital","as_of":"2026-06-01"})).code,
        "invalid_arguments"
    );
    assert_eq!(
        call(json!({"securities":["sz.000001"],"as_of":"2026-06-01"})).code,
        "invalid_arguments"
    );
    assert_eq!(
        call(json!({"securities":[],"fact":"security_names"})).code,
        "invalid_arguments"
    );
    // A date range is not part of this tool (it asks about one day).
    assert_eq!(
        call(json!({"securities":["sz.000001"],"fact":"security_names","start_date":"2026-01-01","end_date":"2026-02-01"})).code,
        "invalid_arguments"
    );
}

fn seed_capital_events(f: &Fixture, symbol: &str) {
    use crate::security_master::{CapitalEvent, CapitalEventSet};
    let MarketBackend::Compact(repo) = &f.backend else {
        panic!("fixture must open the compact backend");
    };
    let binding = repo
        .read(|c| series::read_series_adjusted(c, symbol, Period::Day, Adjustment::Forward))
        .unwrap()
        .unwrap()
        .source_binding_digest;
    // 10,000 万股 -> 12,000 万股 total, float unchanged.
    let bits = [1000f32, 10_000f32, 1000f32, 12_000f32].map(f32::to_bits);
    let set = CapitalEventSet {
        events: vec![
            CapitalEvent::from_raw(0, day(2020, 1, 6), 3, bits).unwrap(),
            CapitalEvent::from_raw(1, day(2020, 1, 8), 3, bits).unwrap(),
        ],
        ..CapitalEventSet::empty(symbol)
    };
    repo.write(|c| {
        crate::sqlite_market::security::replace_capital_events(
            c,
            &set,
            binding,
            "task-capital",
            "item-1",
        )
    })
    .unwrap();
}

fn events_query(dataset: &str, start: &str, end: &str) -> Value {
    json!({
        "dataset":dataset,"contract_version":"v2",
        "selection":{"kind":"single","entity":SYMBOL},
        "filter":{"kind":"daily","start_date":start,"end_date":end},
        "version":{"kind":"latest_per_entity"}
    })
}

#[test]
fn get_security_events_answers_like_query_data_for_both_kinds() {
    let f = fixture();
    seed_daily_pair(&f);
    seed_adjustment_events(&f, SYMBOL);
    seed_capital_events(&f, SYMBOL);

    let capital = f
        .api
        .call(
            "get_security_events",
            json!({"securities":[SYMBOL],"event":"capital","start_date":"2020-01-01","end_date":"2020-12-31"}),
        )
        .unwrap();
    assert_eq!(capital["dataset"], "market.capital_events");
    assert_eq!(capital["row_count"], 2, "{capital}");
    same_answer(
        &capital,
        &f.api
            .call(
                "query_data",
                events_query("market.capital_events", "2020-01-01", "2020-12-31"),
            )
            .unwrap(),
    );

    let adjustment = f
        .api
        .call(
            "get_security_events",
            json!({"securities":[SYMBOL],"event":"adjustment","start_date":"2020-01-06","end_date":"2020-01-06"}),
        )
        .unwrap();
    assert_eq!(adjustment["dataset"], "market.adjustment_events");
    assert_eq!(
        adjustment["columns"]["effective_day"],
        json!(["2020-01-06"])
    );
    assert_eq!(adjustment["columns"]["category"], json!([1]));
    same_answer(
        &adjustment,
        &f.api
            .call(
                "query_data",
                events_query("market.adjustment_events", "2020-01-06", "2020-01-06"),
            )
            .unwrap(),
    );

    // The date range narrows the capital events.
    let narrow = f
        .api
        .call(
            "get_security_events",
            json!({"securities":[SYMBOL],"event":"capital","start_date":"2020-01-07","end_date":"2020-01-31"}),
        )
        .unwrap();
    assert_eq!(narrow["row_count"], 1);
}

#[test]
fn get_security_events_has_no_known_at_and_no_as_of() {
    let f = fixture();
    let base = json!({"securities":[SYMBOL],"event":"capital","start_date":"2020-01-01","end_date":"2020-12-31"});
    for extra in ["known_at", "as_of"] {
        let mut input = base.clone();
        input[extra] = json!("2020-06-01");
        let error = f.api.call("get_security_events", input).unwrap_err();
        assert_eq!(error.code, "invalid_arguments", "{extra}");
    }
    let mut input = base.clone();
    input["event"] = json!("split");
    assert_eq!(
        f.api.call("get_security_events", input).unwrap_err().code,
        "invalid_arguments"
    );
    // The schema does not offer either field.
    let tools = f.api.tools();
    let tool = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "get_security_events")
        .unwrap();
    for hidden in ["known_at", "as_of"] {
        assert!(tool["inputSchema"]["properties"].get(hidden).is_none());
    }
}

// ---- PR-2: get_security_history ---------------------------------------------
//
// Fixtures are invented. `sz.000010` has four name intervals (two ST spells)
// and `sz.000011` one open interval:
//   sz.000010 "ST甲"  2020-01-01 .. 2021-01-01  known 2026-11-01
//   sz.000010 "甲"    2021-01-01 .. 2022-01-01  known 2026-11-01
//   sz.000010 "*ST甲" 2022-01-01 .. 2023-01-01  known 2026-11-03
//   sz.000010 "甲"    2023-01-01 ..             known 2026-11-03
//   sz.000011 "乙"    2019-01-01 ..             known 2026-11-01

fn seed_name_history(f: &Fixture) {
    use crate::security_master::Exchange;
    use crate::security_master::names::{NameCertainty, NameInterval, NameSet, NameSourceKind};
    use crate::sqlite_market::security_names::SecurityNameWrite;

    let row = |code: &str, from: NaiveDate, to: Option<NaiveDate>, name: &str, known: NaiveDate| {
        NameInterval {
            exchange: Exchange::Sz,
            code: code.into(),
            effective_from: from,
            effective_to: to,
            name: name.into(),
            certainty: if to.is_some() {
                NameCertainty::Interval
            } else {
                NameCertainty::Unbounded
            },
            known_at: Some(known),
            source_kind: NameSourceKind::SzseNameHistory,
            source_record_id: Some(format!("{code}|{from}")),
        }
    };
    let early = day(2026, 11, 1);
    let late = day(2026, 11, 3);
    let set = NameSet {
        rows: vec![
            row(
                "000010",
                day(2020, 1, 1),
                Some(day(2021, 1, 1)),
                "ST甲",
                early,
            ),
            row(
                "000010",
                day(2021, 1, 1),
                Some(day(2022, 1, 1)),
                "甲",
                early,
            ),
            row(
                "000010",
                day(2022, 1, 1),
                Some(day(2023, 1, 1)),
                "*ST甲",
                late,
            ),
            row("000010", day(2023, 1, 1), None, "甲", late),
            row("000011", day(2019, 1, 1), None, "乙", early),
        ],
        coverage: Vec::new(),
        pending_sources: Vec::new(),
    };
    f.backend
        .commit_security_names(&SecurityNameWrite {
            set: &set,
            etf: crate::sqlite_market::security_names::EtfPart::Keep,
            task_id: "task-names",
            item_id: "derived:security_names",
            generation_id: "gen-names",
            generation_manifest_sha256: &"41".repeat(32),
            member_sha256: &"42".repeat(32),
            security_event_sha256: &"43".repeat(32),
            generation_evidence: &json!({"generation_id": "gen-names"}),
        })
        .unwrap()
        .expect("a non-empty name set registers its version");
}

/// Three ST spells for `sz.000010` with gaps between them, and one for
/// `sz.000011`.
fn seed_st_spells(f: &Fixture) {
    use crate::security_master::coverage::CoverageWindow;
    use crate::security_master::risk_warnings::RiskWarningSet;
    use crate::security_master::{Exchange, RiskStatus, RiskWarningInterval};
    use crate::sqlite_market::risk_warnings::RiskWarningWrite;

    let spell = |code: &str, from: NaiveDate, to: Option<NaiveDate>, known: NaiveDate| {
        RiskWarningInterval {
            exchange: Exchange::Sz,
            code: code.into(),
            effective_from: from,
            effective_to: to,
            status: RiskStatus::OtherRiskWarning,
            raw_status_code: "ST".into(),
            announced_at: None,
            known_at: Some(known),
            source_kind: "szse_name_history".into(),
            source_record_id: Some(format!("sz|{code}|{from}")),
            source_document_sha256: Some("a".repeat(64)),
        }
    };
    let set = RiskWarningSet {
        rows: vec![
            spell(
                "000010",
                day(2015, 3, 2),
                Some(day(2016, 5, 3)),
                day(2026, 11, 1),
            ),
            spell(
                "000010",
                day(2018, 4, 2),
                Some(day(2019, 6, 3)),
                day(2026, 11, 1),
            ),
            spell("000010", day(2022, 4, 1), None, day(2026, 11, 3)),
            spell("000011", day(2019, 2, 1), None, day(2026, 11, 1)),
        ],
        coverage: vec![CoverageWindow {
            source_kind: "szse_name_history".into(),
            exchange: Exchange::Sz,
            from: day(1994, 1, 3),
            to: day(2026, 11, 1),
            observed: true,
            rows: 4,
            reason: "fixture".into(),
        }],
        pending_sources: Vec::new(),
        sh_deferred: Vec::new(),
    };
    f.backend
        .commit_risk_warnings(&RiskWarningWrite {
            set: &set,
            task_id: "task-risk",
            item_id: "derived:risk_warnings",
            generation_id: "gen-risk",
            generation_manifest_sha256: &"51".repeat(32),
            member_sha256: &"52".repeat(32),
            security_event_sha256: &"53".repeat(32),
            generation_evidence: &json!({"generation_id": "gen-risk"}),
            replay_inputs_digest: None,
        })
        .unwrap()
        .expect("a non-empty set registers its version");
}

/// `sz.000010`: a confirmed halt and a derived gap; `sz.000011`: one gap.
fn seed_suspension_history(f: &Fixture) {
    use crate::security_master::suspensions::SuspensionSet;
    use crate::security_master::{
        Exchange, SuspensionCertainty, SuspensionInterval, SuspensionReason,
    };
    let MarketBackend::Compact(repo) = &f.backend else {
        panic!("fixture must open the compact backend");
    };
    let confirmed =
        |code: &str, from: NaiveDate, to: NaiveDate, known: NaiveDate| SuspensionInterval {
            exchange: Exchange::Sz,
            code: code.into(),
            start_day: from,
            end_day: to,
            certainty: SuspensionCertainty::Confirmed,
            missing_days: 3,
            reason: SuspensionReason::ExchangeAnnouncement,
            known_at: Some(known),
            source_kind: "exchange_announcement".into(),
            source_record_id: Some(format!("ann|{code}|{from}")),
        };
    let gap = |code: &str, from: NaiveDate, to: NaiveDate| SuspensionInterval {
        exchange: Exchange::Sz,
        code: code.into(),
        start_day: from,
        end_day: to,
        certainty: SuspensionCertainty::UnresolvedGap,
        missing_days: 2,
        reason: SuspensionReason::MissingBarNoEvent,
        known_at: None,
        source_kind: "bar_gap".into(),
        source_record_id: None,
    };
    let rows = vec![
        confirmed("000010", day(2020, 3, 2), day(2020, 3, 4), day(2020, 3, 1)),
        gap("000010", day(2021, 8, 2), day(2021, 8, 3)),
        gap("000011", day(2021, 9, 6), day(2021, 9, 7)),
    ];
    let set = SuspensionSet {
        confirmed_rows: 1,
        gap_rows: 2,
        single_day_gap_rows: 0,
        multi_day_gap_rows: 2,
        rows,
        ..SuspensionSet::default()
    };
    repo.write(|c| {
        let binding = crate::sqlite_market::series_commit::put_generation_binding(
            c,
            "gen-susp",
            &"61".repeat(32),
            &"62".repeat(32),
            &"63".repeat(32),
            &json!({"generation_id": "gen-susp"}),
        )?;
        crate::sqlite_market::suspensions::replace_suspensions(
            c,
            &set,
            binding,
            "task-susp",
            "derived:suspensions",
        )
    })
    .unwrap()
    .expect("a non-empty set registers its version");
}

fn history_call(f: &Fixture, input: Value) -> Value {
    f.api.call("get_security_history", input).unwrap()
}

fn history_query(dataset: &str, entities: Value, start: &str, end: &str) -> Value {
    json!({
        "dataset": dataset,
        "contract_version": "v2",
        "selection": {"kind": "batch", "entities": entities},
        "filter": {"kind": "daily", "start_date": start, "end_date": end, "as_of": end},
        "version": {"kind": "latest_per_entity"}
    })
}

#[test]
fn get_security_history_returns_every_st_spell_that_overlaps_the_window() {
    let f = fixture();
    seed_st_spells(&f);
    let one = json!(["sz.000010"]);

    // The whole range returns all three spells, oldest first.
    let all = history_call(
        &f,
        json!({"securities":one,"fact":"risk_warnings","start_date":"2010-01-01","end_date":"2026-12-31"}),
    );
    assert_eq!(all["dataset"], "market.risk_warnings");
    assert_eq!(all["row_count"], 3, "{all}");
    assert_eq!(
        all["columns"]["effective_from"],
        json!(["2015-03-02", "2018-04-02", "2022-04-01"])
    );
    assert_eq!(
        all["columns"]["effective_to"],
        json!(["2016-05-03", "2019-06-03", null])
    );
    // A history read has no record-set day, so the temporal block has none.
    assert!(
        all["temporal"].get("as_of").is_none(),
        "{}",
        all["temporal"]
    );
    assert_eq!(all["temporal"]["known_at"], Value::Null);

    // Spells that end before, or start after, the window are not returned.
    let middle = history_call(
        &f,
        json!({"securities":one,"fact":"risk_warnings","start_date":"2017-01-01","end_date":"2020-12-31"}),
    );
    assert_eq!(middle["columns"]["effective_from"], json!(["2018-04-02"]));
    let none = history_call(
        &f,
        json!({"securities":one,"fact":"risk_warnings","start_date":"2016-06-01","end_date":"2018-03-31"}),
    );
    assert_eq!(none["row_count"], 0);

    // The end of a spell is exclusive and the window start is inclusive of
    // what is still in effect: a spell ending on the window's first day is out.
    let touching = history_call(
        &f,
        json!({"securities":one,"fact":"risk_warnings","start_date":"2016-05-03","end_date":"2016-12-31"}),
    );
    assert_eq!(touching["row_count"], 0);
    let inside = history_call(
        &f,
        json!({"securities":one,"fact":"risk_warnings","start_date":"2016-05-02","end_date":"2016-12-31"}),
    );
    assert_eq!(inside["columns"]["effective_from"], json!(["2015-03-02"]));

    // An open spell that began before the window is still returned.
    let open = history_call(
        &f,
        json!({"securities":one,"fact":"risk_warnings","start_date":"2025-01-01","end_date":"2025-01-31"}),
    );
    assert_eq!(open["columns"]["effective_from"], json!(["2022-04-01"]));

    // Several securities come back together, each with its own spells.
    let both = history_call(
        &f,
        json!({"securities":["sz.000010","sz.000011"],"fact":"risk_warnings","start_date":"2010-01-01","end_date":"2026-12-31"}),
    );
    assert_eq!(both["row_count"], 4);
    assert_eq!(
        both["entities"],
        json!(["sz.000010", "sz.000010", "sz.000010", "sz.000011"])
    );
}

#[test]
fn get_security_history_known_at_hides_rows_learned_later() {
    let f = fixture();
    seed_st_spells(&f);
    let call = |known_at: Option<&str>| {
        let mut input = json!({"securities":["sz.000010"],"fact":"risk_warnings",
                               "start_date":"2010-01-01","end_date":"2026-12-31"});
        if let Some(day) = known_at {
            input["known_at"] = json!(day);
        }
        history_call(&f, input)
    };
    assert_eq!(call(None)["row_count"], 3);
    // The 2022 spell was first known on 2026-11-03.
    let early = call(Some("2026-11-02"));
    assert_eq!(
        early["columns"]["effective_from"],
        json!(["2015-03-02", "2018-04-02"])
    );
    assert_eq!(early["temporal"]["known_at"], "2026-11-02");
    assert_eq!(call(Some("2026-11-03"))["row_count"], 3);
    // Before anything was known, nothing is returned.
    assert_eq!(call(Some("2026-10-31"))["row_count"], 0);
}

#[test]
fn get_security_history_reads_names_and_says_that_sh_has_none() {
    const NOTE: &str = "name history covers SZ only (D-61)";
    let f = fixture();
    seed_name_history(&f);
    let page = history_call(
        &f,
        json!({"securities":["sz.000010"],"fact":"security_names","start_date":"2020-06-01","end_date":"2022-06-01"}),
    );
    assert_eq!(page["columns"]["name"], json!(["ST甲", "甲", "*ST甲"]));
    let notes = |page: &Value| page["limitations"].to_string();
    assert!(!notes(&page).contains(NOTE));

    // The same call at one day returns what `get_security_state` returns.
    let state = f
        .api
        .call(
            "get_security_state",
            json!({"securities":["sz.000010"],"fact":"security_names","as_of":"2021-06-01"}),
        )
        .unwrap();
    assert_eq!(state["columns"]["name"], json!(["甲"]));
    let day_window = history_call(
        &f,
        json!({"securities":["sz.000010"],"fact":"security_names","start_date":"2021-06-01","end_date":"2021-06-01"}),
    );
    assert_eq!(day_window["columns"]["name"], state["columns"]["name"]);

    let sh = history_call(
        &f,
        json!({"securities":["sh.600006"],"fact":"security_names","start_date":"2020-01-01","end_date":"2026-12-31"}),
    );
    assert_eq!(sh["row_count"], 0);
    assert!(notes(&sh).contains(NOTE), "{}", notes(&sh));
    // Another fact for the same SH code carries no such note.
    let other = history_call(
        &f,
        json!({"securities":["sh.600006"],"fact":"risk_warnings","start_date":"2020-01-01","end_date":"2026-12-31"}),
    );
    assert!(!notes(&other).contains(NOTE));
}

#[test]
fn get_security_history_reads_suspension_intervals_by_overlap() {
    let f = fixture();
    seed_suspension_history(&f);
    let one = json!(["sz.000010"]);
    let all = history_call(
        &f,
        json!({"securities":one,"fact":"suspensions","start_date":"2020-01-01","end_date":"2022-12-31"}),
    );
    assert_eq!(
        all["columns"]["start_day"],
        json!(["2020-03-02", "2021-08-02"])
    );
    assert_eq!(
        all["columns"]["certainty"],
        json!(["confirmed", "unresolved_gap"])
    );
    // A window that starts inside a halt still returns it (end day inclusive).
    let inside = history_call(
        &f,
        json!({"securities":one,"fact":"suspensions","start_date":"2020-03-04","end_date":"2020-03-10"}),
    );
    assert_eq!(inside["columns"]["start_day"], json!(["2020-03-02"]));
    let after = history_call(
        &f,
        json!({"securities":one,"fact":"suspensions","start_date":"2020-03-05","end_date":"2020-03-10"}),
    );
    assert_eq!(after["row_count"], 0);
    // A derived gap has no `known_at`, so a cutoff drops it and counts it.
    let known = history_call(
        &f,
        json!({"securities":one,"fact":"suspensions","start_date":"2020-01-01","end_date":"2022-12-31","known_at":"2020-03-01"}),
    );
    assert_eq!(known["columns"]["start_day"], json!(["2020-03-02"]));
    assert_eq!(known["temporal"]["unknown_known_at_rows"], 1);
}

#[test]
fn get_security_history_pages_and_keeps_a_cursor_to_its_own_request() {
    let f = fixture();
    seed_st_spells(&f);
    let base = json!({"securities":["sz.000010"],"fact":"risk_warnings",
                      "start_date":"2010-01-01","end_date":"2026-12-31","page_size":2});
    let first = history_call(&f, base.clone());
    assert_eq!(first["row_count"], 2);
    assert_eq!(first["page_complete"], false);
    let mut next = base.clone();
    next["cursor"] = first["next_cursor"].clone();
    let second = history_call(&f, next.clone());
    assert_eq!(second["columns"]["effective_from"], json!(["2022-04-01"]));
    assert_eq!(second["page_complete"], true);

    // The cursor does not move to another window or to a point-in-time read.
    let mut other = next.clone();
    other["end_date"] = json!("2026-06-30");
    assert_eq!(
        f.api.call("get_security_history", other).unwrap_err().code,
        "invalid_arguments"
    );
    let point = f.api.call(
        "get_security_state",
        json!({"securities":["sz.000010"],"fact":"risk_warnings","as_of":"2026-12-31",
               "page_size":2,"cursor":first["next_cursor"]}),
    );
    assert!(point.is_err());
}

#[test]
fn get_security_history_rejects_bad_input() {
    let f = fixture();
    let call = |input: Value| f.api.call("get_security_history", input).unwrap_err();
    let ok = json!({"securities":["sz.000010"],"fact":"risk_warnings",
                    "start_date":"2020-01-01","end_date":"2020-12-31"});
    let with = |key: &str, value: Value| {
        let mut input = ok.clone();
        input[key] = value;
        input
    };
    // `security_master` is a lifecycle record, not a history of states.
    assert_eq!(
        call(with("fact", json!("security_master"))).code,
        "invalid_arguments"
    );
    assert_eq!(
        call(with("fact", json!("capital"))).code,
        "invalid_arguments"
    );
    // The range is required, ordered, and there is no `as_of`.
    assert_eq!(
        call(json!({"securities":["sz.000010"],"fact":"risk_warnings"})).code,
        "invalid_arguments"
    );
    assert_eq!(
        call(with("start_date", json!("2021-01-01"))).code,
        "invalid_arguments"
    );
    assert_eq!(
        call(with("as_of", json!("2020-06-01"))).code,
        "invalid_arguments"
    );
    assert_eq!(
        call(with("securities", json!([]))).code,
        "invalid_arguments"
    );
    // The tool schema offers `known_at` and no `as_of`.
    let tools = f.api.tools();
    let tool = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "get_security_history")
        .unwrap();
    let properties = &tool["inputSchema"]["properties"];
    assert!(properties.get("known_at").is_some());
    assert!(properties.get("as_of").is_none());
    assert_eq!(
        properties["fact"]["enum"],
        json!(["security_names", "risk_warnings", "suspensions", "industry"])
    );
}

/// `query_data` cannot reach the history mode: the filter kind is not part of
/// its input, and its point-in-time answer is the same whether or not the
/// history tool exists.
#[test]
fn query_data_cannot_ask_for_a_history_filter() {
    let f = fixture();
    seed_st_spells(&f);
    let mut request = history_query(
        "market.risk_warnings",
        json!(["sz.000010"]),
        "2010-01-01",
        "2026-12-31",
    );
    request["filter"] = json!({"kind":"history","start_date":"2010-01-01","end_date":"2026-12-31"});
    assert_eq!(
        f.api.call("query_data", request).unwrap_err().code,
        "invalid_arguments"
    );
}

/// The regression: `query_data` answers exactly as before. A window that
/// holds several spells still returns only the one in effect on `as_of`, and
/// the `temporal` block still names that day.
#[test]
fn query_data_keeps_its_point_in_time_answer_over_a_long_window() {
    let f = fixture();
    seed_st_spells(&f);
    seed_name_history(&f);
    for (as_of, expected) in [
        ("2015-06-01", json!(["2015-03-02"])),
        ("2017-01-01", json!([])),
        ("2018-06-01", json!(["2018-04-02"])),
        ("2026-12-31", json!(["2022-04-01"])),
    ] {
        let page = f
            .api
            .call(
                "query_data",
                json!({
                    "dataset":"market.risk_warnings","contract_version":"v2",
                    "selection":{"kind":"single","entity":"sz.000010"},
                    "filter":{"kind":"daily","start_date":"2010-01-01","end_date":"2026-12-31","as_of":as_of},
                    "version":{"kind":"latest_per_entity"}
                }),
            )
            .unwrap();
        let from = page["columns"]
            .get("effective_from")
            .cloned()
            .unwrap_or(json!([]));
        assert_eq!(from, expected, "as_of {as_of}");
        assert_eq!(page["temporal"]["as_of"], as_of);
    }
    // Without `as_of` the default is still the last trading day, never "all".
    let default = f
        .api
        .call(
            "query_data",
            history_query(
                "market.security_names",
                json!(["sz.000010"]),
                "2010-01-01",
                "2026-12-31",
            ),
        )
        .unwrap();
    assert_eq!(default["columns"]["name"], json!(["甲"]));
}

// ---- query_data golden answers ----------------------------------------------
//
// `query_data` over the M2 interval datasets must answer exactly as it did
// before `get_security_history` added a history mode to the read path. The
// answers below were recorded from the code before that change
// (`query_data_golden.json`) and are compared byte for byte, minus the fields
// that differ per call (`snapshot_id`, `snapshot_time`, `next_cursor`, `published_at`).

fn golden_requests() -> Vec<(String, Value)> {
    let window = |dataset: &str, entity: &str, start: &str, end: &str, as_of: Option<&str>| {
        let mut filter = json!({"kind":"daily","start_date":start,"end_date":end});
        if let Some(as_of) = as_of {
            filter["as_of"] = json!(as_of);
        }
        json!({
            "dataset": dataset,
            "contract_version": "v2",
            "selection": {"kind": "batch", "entities": [entity, "sz.000011"]},
            "filter": filter,
            "version": {"kind": "latest_per_entity"}
        })
    };
    let mut out = Vec::new();
    for dataset in [
        "market.security_names",
        "market.risk_warnings",
        "market.suspensions",
    ] {
        for (label, start, end, as_of) in [
            (
                "long-window-as_of-in-spell",
                "2010-01-01",
                "2026-12-31",
                Some("2018-06-01"),
            ),
            (
                "long-window-as_of-in-gap",
                "2010-01-01",
                "2026-12-31",
                Some("2017-01-01"),
            ),
            (
                "long-window-as_of-open",
                "2010-01-01",
                "2026-12-31",
                Some("2026-12-31"),
            ),
            (
                "as_of-outside-window",
                "2020-01-01",
                "2020-12-31",
                Some("2018-06-01"),
            ),
            (
                "as_of-on-interval-end",
                "2010-01-01",
                "2026-12-31",
                Some("2016-05-03"),
            ),
            ("single-day", "2020-03-03", "2020-03-03", Some("2020-03-03")),
            ("default-as_of", "2010-01-01", "2026-12-31", None),
        ] {
            out.push((
                format!("{dataset} {label}"),
                window(dataset, "sz.000010", start, end, as_of),
            ));
        }
    }
    let mut known = window(
        "market.risk_warnings",
        "sz.000010",
        "2010-01-01",
        "2026-12-31",
        Some("2026-12-31"),
    );
    known["known_at"] = json!("2026-11-02");
    out.push(("market.risk_warnings known_at".into(), known));
    let mut known = window(
        "market.suspensions",
        "sz.000010",
        "2010-01-01",
        "2026-12-31",
        Some("2021-08-02"),
    );
    known["known_at"] = json!("2020-03-01");
    out.push(("market.suspensions known_at".into(), known));
    out
}

/// `published_at` is the wall clock of the fixture's write, not part of the
/// answer's shape or content.
fn without_published_at(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.remove("published_at");
            map.values_mut().for_each(without_published_at);
        }
        Value::Array(items) => items.iter_mut().for_each(without_published_at),
        _ => {}
    }
}

fn golden_answers(f: &Fixture) -> Value {
    seed_name_history(f);
    seed_st_spells(f);
    seed_suspension_history(f);
    commit_calendar_days(f, &[day(2021, 8, 2), day(2021, 8, 3)]);
    let mut answers = serde_json::Map::new();
    for (label, request) in golden_requests() {
        let mut answer = match f.api.call("query_data", request) {
            Ok(answer) => answer,
            Err(error) => json!({"error": error.code, "message": error.message}),
        };
        if let Some(object) = answer.as_object_mut() {
            for key in ["snapshot_id", "snapshot_time", "next_cursor"] {
                object.remove(key);
            }
        }
        without_published_at(&mut answer);
        answers.insert(label, answer);
    }
    Value::Object(answers)
}

#[test]
fn query_data_answers_match_the_recorded_golden_answers() {
    let f = fixture();
    let answers = golden_answers(&f);
    if let Ok(path) = std::env::var("RECORD_QUERY_DATA_GOLDEN") {
        std::fs::write(path, serde_json::to_string_pretty(&answers).unwrap() + "\n").unwrap();
        return;
    }
    let golden: Value = serde_json::from_str(include_str!("query_data_golden.json")).unwrap();
    let (answers, golden) = (answers.as_object().unwrap(), golden.as_object().unwrap());
    assert_eq!(
        answers.keys().collect::<Vec<_>>(),
        golden.keys().collect::<Vec<_>>()
    );
    for (label, expected) in golden {
        assert_eq!(&answers[label], expected, "{label}");
    }
}

// ---- ETF PR-3: find_securities kind=etf ------------------------------------
//
// Invented fixture. Main table (class 2 unless noted):
//   sh.510300 current   名称 "沪深300ETF华泰柏瑞" (first seen 2026-11-05)
//   sz.159919 current   名称 "沪深300ETF嘉实"
//   sh.510500 current   名称 "中证500ETF南方"; before 2026-11-08 "中证500指数ETF"
//   sh.510050 delisted  no observed name (older than the first snapshot)
//   sz.159901 current   name "ＥＴＦ深证１００" typed full-width on the list
//   sh.600006 class 0   an A-share, never an ETF result

fn seed_etf_fixture(f: &Fixture) {
    use crate::security_master::Exchange;
    use crate::security_master::names::{
        EtfNameSet, NameCertainty, NameInterval, NameSet, NameSourceKind,
    };
    use crate::sqlite_market::security_names::{EtfPart, SecurityNameWrite};

    seed_security_master(
        f,
        &[
            ("sh.510300", None),
            ("sz.159919", None),
            ("sh.510500", None),
            ("sh.510050", Some(day(2026, 10, 22))),
            ("sz.159901", None),
            ("sh.600006", None),
        ],
    );
    let MarketBackend::Compact(repo) = &f.backend else {
        panic!("fixture must open the compact backend");
    };
    repo.write(|c| {
        c.execute(
            "UPDATE security_master SET instrument_class=2 WHERE code <> '600006'",
            [],
        )
        .map_err(|e| crate::market_storage::Error::Storage(e.to_string()))?;
        Ok::<(), crate::market_storage::Error>(())
    })
    .unwrap();
    seed_security_names(f);

    let row =
        |exchange: Exchange, code: &str, from: NaiveDate, to: Option<NaiveDate>, name: &str| {
            NameInterval {
                exchange,
                code: code.into(),
                effective_from: from,
                effective_to: to,
                name: name.into(),
                certainty: if to.is_some() {
                    NameCertainty::Interval
                } else {
                    NameCertainty::Unbounded
                },
                known_at: Some(from),
                source_kind: NameSourceKind::ExchangeSnapshot,
                source_record_id: Some(format!("etf_list:{code}")),
            }
        };
    let first = day(2026, 11, 5);
    let etf = EtfNameSet {
        rows: vec![
            row(Exchange::Sh, "510300", first, None, "沪深300ETF华泰柏瑞"),
            row(Exchange::Sz, "159919", first, None, "沪深300ETF嘉实"),
            row(
                Exchange::Sh,
                "510500",
                first,
                Some(day(2026, 11, 8)),
                "中证500指数ETF",
            ),
            row(
                Exchange::Sh,
                "510500",
                day(2026, 11, 8),
                None,
                "中证500ETF南方",
            ),
            row(Exchange::Sz, "159901", first, None, "ＥＴＦ深证１００"),
        ],
        entities: [
            (Exchange::Sh, "510300".to_string()),
            (Exchange::Sz, "159919".to_string()),
            (Exchange::Sh, "510500".to_string()),
            (Exchange::Sh, "510050".to_string()),
            (Exchange::Sz, "159901".to_string()),
        ]
        .into_iter()
        .collect(),
    };
    let a_share = NameSet {
        rows: f
            .backend
            .read(crate::sqlite_market::security::read_security_names)
            .unwrap()
            .iter()
            .map(|row| row.to_interval().unwrap())
            .collect(),
        coverage: Vec::new(),
        pending_sources: Vec::new(),
    };
    f.backend
        .commit_security_names(&SecurityNameWrite {
            set: &a_share,
            etf: EtfPart::Replace(&etf),
            task_id: "task-names",
            item_id: "derived:security_names",
            generation_id: "gen-names",
            generation_manifest_sha256: &"41".repeat(32),
            member_sha256: &"42".repeat(32),
            security_event_sha256: &"43".repeat(32),
            generation_evidence: &json!({"generation_id": "gen-names"}),
        })
        .unwrap();
}

fn find_etf(f: &Fixture, input: Value) -> Value {
    let mut input = input;
    input["kind"] = json!("etf");
    f.api.call("find_securities", input).unwrap()
}

fn etf_entities(answer: &Value) -> Vec<String> {
    answer["entities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn find_etf_matches_names_across_width_case_and_whitespace() {
    let f = fixture();
    seed_etf_fixture(&f);
    let both = vec!["sh.510300".to_owned(), "sz.159919".to_owned()];
    for query in [
        "沪深300ETF",
        "300ETF",
        "300etf",
        "沪深３００ＥＴＦ",
        "沪深 300 Etf",
        "　沪深300　ETF ",
        "沪深300ＥｔＦ",
    ] {
        let answer = find_etf(&f, json!({"query": query}));
        assert_eq!(etf_entities(&answer), both, "{query:?}");
    }
    // The stored full-width name is found by its half-width spelling.
    let wide = find_etf(&f, json!({"query": "etf深证100"}));
    assert_eq!(etf_entities(&wide), ["sz.159901"]);
    assert_eq!(wide["columns"]["name"], json!(["ＥＴＦ深证１００"]));
    // A substring of one name only.
    let one = find_etf(&f, json!({"query": "华泰"}));
    assert_eq!(etf_entities(&one), ["sh.510300"]);
}

#[test]
fn find_etf_returns_status_name_basis_and_observation_day() {
    let f = fixture();
    seed_etf_fixture(&f);
    let all = find_etf(&f, json!({}));
    assert_eq!(
        etf_entities(&all),
        [
            "sh.510050",
            "sh.510300",
            "sh.510500",
            "sz.159901",
            "sz.159919"
        ]
    );
    assert_eq!(all["layout"], "table");
    assert_eq!(all["row_count"], 5);
    let columns = &all["columns"];
    assert_eq!(
        columns["name"],
        json!([
            null,
            "沪深300ETF华泰柏瑞",
            "中证500ETF南方",
            "ＥＴＦ深证１００",
            "沪深300ETF嘉实"
        ])
    );
    assert_eq!(
        columns["name_basis"],
        json!([
            "unavailable",
            "observed_from_first_snapshot",
            "observed_from_first_snapshot",
            "observed_from_first_snapshot",
            "observed_from_first_snapshot"
        ])
    );
    assert_eq!(
        columns["status"],
        json!(["delisted", "current", "current", "current", "current"])
    );
    assert_eq!(columns["instrument_class"], json!([2, 2, 2, 2, 2]));
    // The latest name's own first observation, not the code's first.
    assert_eq!(
        columns["name_observed_from"],
        json!([null, "2026-11-05", "2026-11-08", "2026-11-05", "2026-11-05"])
    );
    assert_eq!(columns["delisting_date"][0], "2026-10-22");
    assert!(columns["delisting_date"][1].is_null());
    // No A-share leaks in, and no empty-string name.
    assert!(!etf_entities(&all).contains(&"sh.600006".to_owned()));
    assert!(
        columns["name"]
            .as_array()
            .unwrap()
            .iter()
            .all(|n| n.is_null() || !n.as_str().unwrap().is_empty())
    );
}

#[test]
fn find_etf_searches_the_latest_name_only_and_never_a_missing_one() {
    let f = fixture();
    seed_etf_fixture(&f);
    // The earlier name of 510500 does not match; the later one does.
    assert!(etf_entities(&find_etf(&f, json!({"query": "指数ETF"}))).is_empty());
    assert_eq!(
        etf_entities(&find_etf(&f, json!({"query": "500etf"}))),
        ["sh.510500"]
    );
    // An ETF with no name is found by its code and by nothing else.
    let by_code = find_etf(&f, json!({"code": "510050"}));
    assert_eq!(etf_entities(&by_code), ["sh.510050"]);
    assert_eq!(by_code["columns"]["name_basis"], json!(["unavailable"]));
    assert!(by_code["columns"].get("name").is_none(), "{by_code}");
    assert!(etf_entities(&find_etf(&f, json!({"query": "510050"}))).is_empty());
    // Exchange short forms are outside the contract.
    for query in ["华夏300", "广发500", "300ETF华夏"] {
        assert!(etf_entities(&find_etf(&f, json!({"query": query}))).is_empty());
    }
    // A-share names are not searched.
    assert!(etf_entities(&find_etf(&f, json!({"query": "新名"}))).is_empty());
    // code and query combine as an intersection.
    assert!(etf_entities(&find_etf(&f, json!({"code":"510300","query":"嘉实"}))).is_empty());
    assert_eq!(
        etf_entities(&find_etf(&f, json!({"code":"510300","query":"华泰"}))),
        ["sh.510300"]
    );
}

#[test]
fn find_etf_says_what_its_names_do_and_do_not_cover() {
    let f = fixture();
    seed_etf_fixture(&f);
    let answer = find_etf(&f, json!({"query": "沪深300"}));
    let text = answer["limitations"].to_string();
    for needle in [
        "first list snapshot",
        "华夏300",
        "广发500",
        "name_basis unavailable",
        "delisted",
    ] {
        assert!(text.contains(needle), "{needle}: {text}");
    }
    // An empty result keeps the same limitations.
    let none = find_etf(&f, json!({"query": "不存在"}));
    assert_eq!(none["row_count"], 0);
    assert!(none["limitations"].to_string().contains("华夏300"));
}

#[test]
fn find_etf_before_any_write_is_absent_and_empty() {
    let f = fixture();
    let answer = find_etf(&f, json!({"query": "沪深300"}));
    assert_eq!(answer["row_count"], 0);
    assert!(
        answer["limitations"]
            .to_string()
            .contains("no registered version")
    );
    assert!(
        answer["versions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["status"] == "absent")
    );
}

#[test]
fn find_etf_pages_and_binds_the_cursor_to_its_request() {
    let f = fixture();
    seed_etf_fixture(&f);
    let mut seen = Vec::new();
    let mut cursor: Option<Value> = None;
    let mut pages = 0;
    loop {
        let mut input = json!({"page_size": 2});
        if let Some(cursor) = &cursor {
            input["cursor"] = cursor.clone();
        }
        let page = find_etf(&f, input);
        pages += 1;
        assert!(page["row_count"].as_u64().unwrap() <= 2);
        seen.extend(etf_entities(&page));
        // Each page keeps its columns aligned with its own rows.
        assert_eq!(
            page["columns"]["status"].as_array().unwrap().len() as u64,
            page["row_count"].as_u64().unwrap()
        );
        if page["next_cursor"].is_null() {
            break;
        }
        cursor = Some(page["next_cursor"].clone());
    }
    assert_eq!(pages, 3);
    assert_eq!(
        seen,
        [
            "sh.510050",
            "sh.510300",
            "sh.510500",
            "sz.159901",
            "sz.159919"
        ]
    );

    // A cursor belongs to the request that issued it.
    let first = find_etf(&f, json!({"page_size": 1}));
    let token = first["next_cursor"].clone();
    for other in [
        f.api.call(
            "find_securities",
            json!({"page_size":1,"cursor":token.clone()}),
        ),
        f.api.call(
            "find_securities",
            json!({"kind":"etf","query":"沪深","page_size":1,"cursor":token}),
        ),
    ] {
        assert_eq!(other.unwrap_err().code, "invalid_arguments");
    }
}

#[test]
fn find_securities_kind_defaults_to_stock_and_rejects_bad_input() {
    let f = fixture();
    publish_catalog(
        &f,
        "catalog-kind",
        &[
            ("600006", crate::catalog::Exchange::Shanghai),
            ("000001", crate::catalog::Exchange::Shenzhen),
        ],
    );
    let plain = f
        .api
        .call("find_securities", json!({"query":"600006"}))
        .unwrap();
    let stock = f
        .api
        .call("find_securities", json!({"query":"600006","kind":"stock"}))
        .unwrap();
    same_answer(&stock, &plain);
    assert_eq!(stock["columns"], plain["columns"]);
    assert_eq!(stock["limitations"], plain["limitations"]);

    for (input, why) in [
        (json!({"kind":"fund"}), "unknown kind"),
        (json!({"kind":"ETF"}), "kind is lower case"),
        (json!({"kind":"all"}), "all is not offered"),
        (json!({"kind":""}), "empty kind"),
        (json!({"kind":7}), "not a string"),
        (json!({"kind":"etf","as_of":"2026-11-05"}), "extra field"),
        (json!({"kind":"etf","include_delisted":true}), "extra field"),
        (json!({"kind":"etf","fields":["name"]}), "extra field"),
        (
            json!({"kind":"etf","industry_code":"I1"}),
            "ETF has no industry",
        ),
        (json!({"kind":"etf","query":"  　"}), "blank query"),
    ] {
        let error = f.api.call("find_securities", input.clone()).unwrap_err();
        assert_eq!(error.code, "invalid_arguments", "{why}: {input}");
    }
}

#[test]
fn query_data_cannot_reach_the_etf_name_filter() {
    let f = fixture();
    seed_etf_fixture(&f);
    let error = f
        .api
        .call(
            "query_data",
            json!({
                "dataset": "market.security_names",
                "contract_version": "v2",
                "selection": {"kind": "market"},
                "filter": {"kind": "find_etf", "name_query": "沪深"},
                "version": {"kind": "latest_per_entity"}
            }),
        )
        .unwrap_err();
    assert_eq!(error.code, "invalid_arguments");
}

// ---- D-82 derived read-only tools (issue-433 PR-B) -------------------------
//
// The two tools call the research read models directly, so each test either
// compares the tool's row with the research function's own answer (the
// equivalence rule of PR-1) or pins one fail-closed state. The fixture is
// built through the production writers, so the rows the tools read carry the
// encoding a daily run produces.

/// Five consecutive trading days after the 2020-08-24 GEM reform: on the GEM
/// board every status class shares one band from that day, so a limit is
/// `known` while the risk status is still `unknown` (design §2.2).
fn derived_days() -> Vec<NaiveDate> {
    vec![
        day(2020, 9, 7),
        day(2020, 9, 8),
        day(2020, 9, 9),
        day(2020, 9, 10),
        day(2020, 9, 11),
    ]
}

fn derived_lifecycle(
    exchange: crate::security_master::Exchange,
    code: &str,
    listing: Option<NaiveDate>,
    delisting: Option<NaiveDate>,
    instrument_class: u8,
) -> crate::security_master::SecurityLifecycleFact {
    use crate::security_master::{
        DelistingReason, DelistingSource, IssuerResolution, ListingSource, SecurityLifecycleFact,
    };
    SecurityLifecycleFact {
        exchange,
        code: code.into(),
        effective_from: listing.unwrap_or(day(2019, 1, 2)),
        effective_to: None,
        instrument_class,
        issuer_key: None,
        issuer_resolution: IssuerResolution::IssuerKey,
        listing_date: listing,
        listing_date_tdx: listing,
        listing_source: if listing.is_some() {
            ListingSource::ExchangeList
        } else {
            ListingSource::Unknown
        },
        b_listing_date: None,
        delisting_date: delisting,
        delisting_date_tdx: delisting,
        delisting_source: if delisting.is_some() {
            DelistingSource::ExchangeList
        } else {
            DelistingSource::Unknown
        },
        delisting_reason_class: DelistingReason::Unknown,
        arrangement_start: None,
        arrangement_end: None,
        listing_conflict: false,
        delisting_conflict: false,
        known_at: None,
    }
}

/// A raw daily series with one bar per listed day, all at `base` plus the
/// day's index, so a rendered price is checkable by hand.
fn seed_derived_series(
    c: &Connection,
    binding: [u8; 32],
    evidence: [u8; 32],
    entity: &str,
    indexes: &[usize],
    base: i64,
    seed: u8,
) {
    use crate::compact_market::{CompactBar, epoch_day};
    use crate::market_data::{CoverageStatus, Period};
    use crate::sqlite_market::series::NewSeries;

    let days = derived_days();
    let bars: Vec<CompactBar> = indexes
        .iter()
        .map(|index| CompactBar {
            trade_day: epoch_day(days[*index]),
            open_units: base,
            high_units: base + 10,
            low_units: base - 10,
            close_units: base + *index as i64,
            volume: Some(100),
            amount: Some("100500.00".into()),
            r7_raw: Some(7),
        })
        .collect();
    let key = series::insert_series(
        c,
        &NewSeries {
            entity: entity.into(),
            period: Period::Day,
            adjustment: Adjustment::Raw,
            instrument_class: crate::market_data::INSTRUMENT_CLASS_STOCK,
            price_scale: 2,
            // The series identity is the content digest, so each entity needs
            // its own: a shared digest collides on `market_series.version_id`.
            content_digest: [seed; 32],
            record_count: bars.len() as i64,
            first_day: bars[0].trade_day,
            last_day: bars[bars.len() - 1].trade_day,
            coverage_status: CoverageStatus::Complete,
            source_binding_digest: binding,
            evidence_digest: evidence,
            last_event_seq: 1,
            created_at: "2026-10-26T00:00:00Z".into(),
            updated_at: "2026-10-26T00:00:00Z".into(),
        },
    )
    .unwrap();
    series::insert_bars(c, key, &bars).unwrap();
}

/// The witness set of the derived-tool tests, one entity per state:
///
/// | entity | witness |
/// |---|---|
/// | `sz.300114` | GEM, bars every day: `tradable`, limit `known` (invariant band) |
/// | `sz.000001` | main board, no risk interval: `tradable`, limit `unknown`/`st_unknown` |
/// | `sz.000631` | bars on day 0 and 4, `unresolved_gap` 1..3: `unknown`/`data_gap` |
/// | `sz.000004` | no bar on day 1, `confirmed` suspension: `not_tradable`/`suspended` |
/// | `sh.510050` | ETF (`instrument_class=2`): `unknown`/`not_a_share` |
/// | `sh.600028` | A+H capital events: market cap `known` |
/// | `sh.600519` | not in the main table at all: `unknown`/`not_in_master` |
fn derived_fixture() -> Fixture {
    use crate::security_master::assembly::Assembly;
    use crate::security_master::suspensions::SuspensionSet;
    use crate::security_master::{
        CapitalEvent, CapitalEventSet, Exchange, SuspensionCertainty, SuspensionInterval,
        SuspensionReason,
    };
    use crate::sqlite_market::{evidence_store, security, suspensions};

    let f = fixture();
    let days = derived_days();
    commit_calendar_days(&f, &days);

    let MarketBackend::Compact(repo) = &f.backend else {
        panic!("fixture must open the compact backend");
    };
    repo.write(|c| {
        let generation = evidence_store::put_evidence(
            c,
            crate::compact_market::EvidenceKind::Generation,
            &json!({"generation": "gen-derived"}),
        )?;
        let binding = evidence_store::put_source_binding(
            c,
            &crate::compact_market::SourceBinding {
                generation_id: "gen-derived".into(),
                generation_manifest_sha256: [0x0b; 32],
                day_member_sha256: [0x0c; 32],
                security_event_sha256: [0x0e; 32],
                evidence_digest: generation,
            },
        )?;
        // The series store validates the whole bundle, so the fixture has to
        // write a complete `VersionEvidence` (same shape `seed_daily_pair`
        // publishes), not a stub.
        let evidence = evidence_store::put_evidence(
            c,
            crate::compact_market::EvidenceKind::Version,
            &serde_json::to_value(crate::compact_market::VersionEvidence {
                source: "official".into(),
                coverage_reason: "fixture".into(),
                unit_currency: units(),
                adjustment_evidence: crate::market_data::AdjustmentEvidence::confirmed(
                    "raw", "fixture",
                ),
                excluded_bars: None,
            })
            .unwrap(),
        )?;

        let mut seed = 0x20u8;
        for (entity, indexes, base) in [
            ("sz.300114", (0..5).collect::<Vec<_>>(), 2000),
            ("sz.000001", (0..5).collect(), 1000),
            ("sz.000631", vec![0, 4], 6310),
            (
                "sz.000004",
                (0..5).filter(|index| !(1..=2).contains(index)).collect(),
                4000,
            ),
            ("sh.510050", (0..5).collect(), 3000),
            ("sh.600028", (0..5).collect(), 1000),
            // Ex-rights witness: a cat1 event with an allotment component
            // between the previous close and `as_of` makes the reference price
            // unverifiable (limit-v2, D-84). It has to be a GEM code: on the
            // main board the unknown risk status stops the derivation before
            // the basis is reached.
            ("sz.300115", (0..5).collect(), 6000),
        ] {
            seed += 1;
            seed_derived_series(c, binding, evidence, entity, &indexes, base, seed);
        }

        security::replace_security_master(
            c,
            &Assembly {
                rows: vec![
                    derived_lifecycle(Exchange::Sz, "300114", Some(day(2020, 1, 2)), None, 0),
                    derived_lifecycle(Exchange::Sz, "000001", Some(day(2020, 1, 2)), None, 0),
                    derived_lifecycle(Exchange::Sz, "000631", Some(day(2020, 1, 2)), None, 0),
                    derived_lifecycle(Exchange::Sz, "000004", Some(day(2020, 1, 2)), None, 0),
                    derived_lifecycle(Exchange::Sh, "510050", Some(day(2020, 1, 2)), None, 2),
                    derived_lifecycle(Exchange::Sh, "600028", Some(day(2020, 1, 2)), None, 0),
                    derived_lifecycle(Exchange::Sz, "300115", Some(day(2020, 1, 2)), None, 0),
                ],
                migrations: Vec::new(),
                conflicts: Vec::new(),
                pending_sources: Vec::new(),
                unresolved: Vec::new(),
                exchange_as_of: Some(days[4]),
            },
            binding,
            "task-master",
            "derived:security_master",
        )?;

        // 600028: 1.0e9 float A shares, 1.2e9 company total (the rest is H).
        security::replace_capital_events(
            c,
            &CapitalEventSet {
                entity: "sh.600028".into(),
                events: vec![
                    CapitalEvent::from_raw(
                        0,
                        days[0],
                        9,
                        [
                            f32::to_bits(0.0),
                            f32::to_bits(0.0),
                            f32::to_bits(100_000.0),
                            f32::to_bits(120_000.0),
                        ],
                    )
                    .unwrap(),
                ],
                excluded: 0,
                excluded_by_category: Default::default(),
            },
            binding,
            "task-capital",
            "derived:capital_events",
        )?;

        let suspension = |code: &str,
                          start: NaiveDate,
                          end: NaiveDate,
                          certainty: SuspensionCertainty,
                          missing_days: i64,
                          reason: SuspensionReason| SuspensionInterval {
            exchange: Exchange::Sz,
            code: code.into(),
            start_day: start,
            end_day: end,
            certainty,
            missing_days,
            reason,
            known_at: None,
            source_kind: "fixture".into(),
            source_record_id: None,
        };
        let rows = vec![
            suspension(
                "000631",
                days[1],
                days[3],
                SuspensionCertainty::UnresolvedGap,
                3,
                SuspensionReason::MissingBarNoEvent,
            ),
            suspension(
                "000004",
                days[1],
                days[2],
                SuspensionCertainty::Confirmed,
                2,
                SuspensionReason::ExchangeAnnouncement,
            ),
        ];
        let set = SuspensionSet {
            confirmed_rows: rows
                .iter()
                .filter(|row| row.certainty == SuspensionCertainty::Confirmed)
                .count(),
            gap_rows: rows
                .iter()
                .filter(|row| row.certainty == SuspensionCertainty::UnresolvedGap)
                .count(),
            single_day_gap_rows: rows
                .iter()
                .filter(|row| {
                    row.certainty == SuspensionCertainty::UnresolvedGap && row.missing_days == 1
                })
                .count(),
            multi_day_gap_rows: rows
                .iter()
                .filter(|row| {
                    row.certainty == SuspensionCertainty::UnresolvedGap && row.missing_days > 1
                })
                .count(),
            deferred_beyond_calendar: 0,
            deferred_samples: Vec::new(),
            skipped_no_trading_day: 0,
            skipped_samples: Vec::new(),
            pending_sources: Vec::new(),
            rows,
        };
        suspensions::replace_suspensions(
            c,
            &set,
            binding,
            "task-suspensions",
            "derived:suspensions",
        )?;

        // 300115: one cat1 event on the last day, with a positive C4
        // (allotment) factor. The gbbq formula is only about 61% accurate for
        // allotments, so the reference price cannot be verified and the limit
        // must be `unknown` + `ex_rights_unverified` (design §3.2, D-84).
        crate::sqlite_market::derived::replace_adjustment_events(
            c,
            &crate::market_data::AdjustmentEventSet {
                entity: "sz.300115".into(),
                events: vec![crate::market_data::AdjustmentEvent {
                    ordinal: 1,
                    effective_day: days[4],
                    category: 1,
                    c_bits: [0; 4],
                    factors: [
                        rust_decimal::Decimal::new(10, 0),
                        rust_decimal::Decimal::new(10, 0),
                        rust_decimal::Decimal::ZERO,
                        rust_decimal::Decimal::new(3, 1),
                    ],
                    known_at: None,
                    basis_status: crate::market_data::FactorBasisStatus::Verified,
                }],
                excluded: 0,
                precision: crate::market_data::FactorPrecision::Stock2dp,
            },
            binding,
            "task-events",
            "item-1",
        )?;

        crate::sqlite_market::rules::replace_trading_rules(
            c,
            &crate::trading_rules::rule_book().unwrap(),
            binding,
            "task-rules",
            "derived:price_limit_rules",
        )?;
        Ok(())
    })
    .unwrap();
    f
}

/// The witnesses the `limit_basis` test needs on top of [`derived_fixture`]:
///
/// * `sz.300116` — a cat1 event with no allotment on the 2020 witness day, so
///   the basis is the computed `reference_price` (GEM after 2020-08-24 has one
///   band for every status, so the derivation reaches the basis).
/// * `sz.300118` — a 2009-11 window, the only period where the official r7
///   reference price is reachable (before the 2010-09-14 cutoff) on a board
///   whose band does not depend on the risk status. Before 2020-08-24 the GEM
///   ST and non-ST bands differ, so it also needs a `normal` risk-warning row:
///   without a status the derivation stops at `st_unknown` before the basis.
fn derived_limit_basis_fixture() -> Fixture {
    use crate::compact_market::{CompactBar, EvidenceKind, SourceBinding, epoch_day};
    use crate::market_data::{Adjustment, CoverageStatus, Period};
    use crate::security_master::assembly::Assembly;
    use crate::security_master::coverage::CoverageWindow;
    use crate::security_master::risk_warnings::RiskWarningSet;
    use crate::security_master::suspensions::SuspensionSet;
    use crate::security_master::{
        Exchange, RiskStatus, RiskWarningInterval, SuspensionCertainty, SuspensionInterval,
        SuspensionReason,
    };
    use crate::sqlite_market::risk_warnings::RiskWarningWrite;
    use crate::sqlite_market::series::NewSeries;
    use crate::sqlite_market::{evidence_store, security, series, suspensions};

    let f = derived_fixture();
    let days = derived_days();
    let gem_days = vec![
        day(2009, 10, 30),
        day(2009, 11, 2),
        day(2009, 11, 3),
        day(2009, 11, 4),
        day(2009, 11, 5),
        day(2009, 11, 6),
        day(2009, 11, 9),
        day(2009, 11, 10),
        day(2009, 11, 11),
        day(2009, 11, 12),
        day(2009, 11, 13),
        day(2009, 11, 16),
        day(2009, 11, 17),
    ];

    let MarketBackend::Compact(repo) = &f.backend else {
        panic!("fixture must open the compact backend");
    };
    repo.write(|c| {
        let generation = evidence_store::put_evidence(
            c,
            EvidenceKind::Generation,
            &json!({"generation": "gen-basis"}),
        )?;
        let binding = evidence_store::put_source_binding(
            c,
            &SourceBinding {
                generation_id: "gen-basis".into(),
                generation_manifest_sha256: [0x1b; 32],
                day_member_sha256: [0x1c; 32],
                security_event_sha256: [0x1e; 32],
                evidence_digest: generation,
            },
        )?;
        let evidence = evidence_store::put_evidence(
            c,
            EvidenceKind::Version,
            &serde_json::to_value(crate::compact_market::VersionEvidence {
                source: "official".into(),
                coverage_reason: "fixture".into(),
                unit_currency: units(),
                adjustment_evidence: crate::market_data::AdjustmentEvidence::confirmed(
                    "raw", "fixture",
                ),
                excluded_bars: None,
            })
            .unwrap(),
        )?;

        // sz.300116: the 2020 window, same bars as the other witnesses there.
        seed_derived_series(
            c,
            binding,
            evidence,
            "sz.300116",
            &[0, 1, 2, 3, 4],
            7000,
            0x81,
        );

        // sz.300118: the 2009 window. Every bar carries r7_raw=7 except the
        // witness day, whose 2702 is the official reference price in fen.
        let bars: Vec<CompactBar> = gem_days
            .iter()
            .enumerate()
            .map(|(index, when)| CompactBar {
                trade_day: epoch_day(*when),
                open_units: 2500,
                high_units: 2520,
                low_units: 2490,
                close_units: 2500 + index as i64,
                volume: Some(100),
                amount: Some("100500.00".into()),
                r7_raw: Some(if *when == day(2009, 11, 17) { 2702 } else { 7 }),
            })
            .collect();
        let key = series::insert_series(
            c,
            &NewSeries {
                entity: "sz.300118".into(),
                period: Period::Day,
                adjustment: Adjustment::Raw,
                instrument_class: crate::market_data::INSTRUMENT_CLASS_STOCK,
                price_scale: 2,
                content_digest: [0x82; 32],
                record_count: bars.len() as i64,
                first_day: bars[0].trade_day,
                last_day: bars[bars.len() - 1].trade_day,
                coverage_status: CoverageStatus::Complete,
                source_binding_digest: binding,
                evidence_digest: evidence,
                last_event_seq: 1,
                created_at: "2026-10-26T00:00:00Z".into(),
                updated_at: "2026-10-26T00:00:00Z".into(),
            },
        )
        .unwrap();
        series::insert_bars(c, key, &bars).unwrap();

        let mut rows = security::read_security_master(c)?;
        rows.push(derived_lifecycle(
            Exchange::Sz,
            "300116",
            Some(day(2020, 1, 2)),
            None,
            0,
        ));
        rows.push(derived_lifecycle(
            Exchange::Sz,
            "300118",
            Some(day(2009, 10, 30)),
            None,
            0,
        ));
        security::replace_security_master(
            c,
            &Assembly {
                rows,
                migrations: Vec::new(),
                conflicts: Vec::new(),
                pending_sources: Vec::new(),
                unresolved: Vec::new(),
                exchange_as_of: Some(days[4]),
            },
            binding,
            "task-master-basis",
            "derived:security_master",
        )?;

        // Cat1 events without an allotment component: the reference price is
        // computable, so the basis is not withheld as `ex_rights_unverified`.
        for (entity, when, item) in [
            ("sz.300116", days[4], "item-basis-300116"),
            ("sz.300118", day(2009, 11, 17), "item-basis-300118"),
        ] {
            crate::sqlite_market::derived::replace_adjustment_events(
                c,
                &crate::market_data::AdjustmentEventSet {
                    entity: entity.into(),
                    events: vec![crate::market_data::AdjustmentEvent {
                        ordinal: 1,
                        effective_day: when,
                        category: 1,
                        c_bits: [
                            1.6f32.to_bits(),
                            0.0f32.to_bits(),
                            3.0f32.to_bits(),
                            0.0f32.to_bits(),
                        ],
                        factors: [
                            rust_decimal::Decimal::new(16, 1),
                            rust_decimal::Decimal::ZERO,
                            rust_decimal::Decimal::new(30, 1),
                            rust_decimal::Decimal::ZERO,
                        ],
                        known_at: None,
                        basis_status: crate::market_data::FactorBasisStatus::Verified,
                    }],
                    excluded: 0,
                    precision: crate::market_data::FactorPrecision::Stock2dp,
                },
                binding,
                "task-events-basis",
                item,
            )?;
        }

        // The basis is a property of the day, not of trading: 300116 is
        // suspended on the witness day and still has a derivable limit. The
        // two existing rows are kept as they are.
        let suspension = |code: &str,
                          start: NaiveDate,
                          end: NaiveDate,
                          certainty: SuspensionCertainty,
                          missing_days: i64,
                          reason: SuspensionReason| SuspensionInterval {
            exchange: Exchange::Sz,
            code: code.into(),
            start_day: start,
            end_day: end,
            certainty,
            missing_days,
            reason,
            known_at: None,
            source_kind: "fixture".into(),
            source_record_id: None,
        };
        let set = SuspensionSet {
            rows: vec![
                suspension(
                    "000631",
                    days[1],
                    days[3],
                    SuspensionCertainty::UnresolvedGap,
                    3,
                    SuspensionReason::MissingBarNoEvent,
                ),
                suspension(
                    "000004",
                    days[1],
                    days[2],
                    SuspensionCertainty::Confirmed,
                    2,
                    SuspensionReason::ExchangeAnnouncement,
                ),
                suspension(
                    "300116",
                    days[4],
                    days[4],
                    SuspensionCertainty::Confirmed,
                    1,
                    SuspensionReason::ExchangeAnnouncement,
                ),
            ],
            confirmed_rows: 2,
            gap_rows: 1,
            single_day_gap_rows: 0,
            multi_day_gap_rows: 1,
            deferred_beyond_calendar: 0,
            deferred_samples: Vec::new(),
            skipped_no_trading_day: 0,
            skipped_samples: Vec::new(),
            pending_sources: Vec::new(),
        };
        suspensions::replace_suspensions(
            c,
            &set,
            binding,
            "task-suspensions-basis",
            "derived:suspensions",
        )?;
        Ok::<(), crate::market_storage::Error>(())
    })
    .unwrap();

    // A `normal` status for the 2009 witness. D-81 admits `normal` only for SZ
    // name-history rows, which is exactly what this is.
    let set = RiskWarningSet {
        rows: vec![RiskWarningInterval {
            exchange: Exchange::Sz,
            code: "300118".into(),
            effective_from: day(2009, 10, 30),
            effective_to: None,
            status: RiskStatus::Normal,
            raw_status_code: "normal".into(),
            announced_at: None,
            known_at: None,
            source_kind: "szse_name_history".into(),
            source_record_id: Some("sz|300118|2009-10-30".into()),
            source_document_sha256: None,
        }],
        coverage: vec![CoverageWindow {
            source_kind: "szse_name_history".into(),
            exchange: Exchange::Sz,
            from: day(2009, 10, 30),
            to: day(2009, 11, 17),
            observed: true,
            rows: 1,
            reason: "fixture".into(),
        }],
        pending_sources: Vec::new(),
        sh_deferred: Vec::new(),
    };
    f.backend
        .commit_risk_warnings(&RiskWarningWrite {
            set: &set,
            task_id: "task-risk-basis",
            item_id: "derived:risk_warnings",
            generation_id: "gen-basis-risk",
            generation_manifest_sha256: &"61".repeat(32),
            member_sha256: &"62".repeat(32),
            security_event_sha256: &"63".repeat(32),
            generation_evidence: &json!({"generation_id": "gen-basis-risk"}),
            replay_inputs_digest: None,
        })
        .unwrap();

    let mut all_days = days.clone();
    all_days.extend_from_slice(&gem_days);
    // The 2020 witnesses are listed on 2020-01-02. The committed calendar is
    // replaced, so the listing day has to stay in it: a listing that predates
    // the calendar's first day is only `seasoned` while the calendar is short.
    all_days.push(day(2020, 1, 2));
    all_days.sort_unstable();
    commit_calendar_days(&f, &all_days);
    f
}

fn tradability_row(answer: &Value, entity: &str) -> Value {
    let index = answer["entities"]
        .as_array()
        .unwrap()
        .iter()
        .position(|value| value == entity)
        .unwrap_or_else(|| panic!("{entity} missing from {answer}"));
    let mut row = serde_json::Map::new();
    for (column, values) in answer["columns"].as_object().unwrap() {
        row.insert(column.clone(), values[index].clone());
    }
    Value::Object(row)
}

#[test]
fn get_tradability_matches_the_research_functions_row_by_row() {
    use crate::research::context::ResearchContext;
    use crate::research::temporal::Temporal;
    use crate::research::{price_limit, tradability};

    let f = derived_fixture();
    let as_of = derived_days()[2];
    let securities = [
        "sz.300114",
        "sz.000001",
        "sz.000631",
        "sz.000004",
        "sh.510050",
        "sh.600519",
    ];
    let answer = f
        .api
        .call(
            "get_tradability",
            json!({"securities": securities, "as_of": as_of.to_string()}),
        )
        .unwrap();
    assert_eq!(answer["layout"], "table");
    assert_eq!(answer["as_of"], json!(as_of.to_string()));
    assert_eq!(answer["entities"], json!(securities));
    // One row per entity, including the ETF and the code with no master row.
    for column in answer["columns"].as_object().unwrap().values() {
        assert_eq!(
            column.as_array().unwrap().len(),
            securities.len(),
            "{column}"
        );
    }

    // The tool is a view over the research functions: every value must equal
    // the function's own answer for the same day, not a second derivation.
    let MarketBackend::Compact(repo) = &f.backend else {
        panic!("fixture must open the compact backend");
    };
    repo.read(|c| {
        let ctx = ResearchContext::new(c);
        let temporal = Temporal {
            as_of,
            known_at: None,
        };
        for entity in securities {
            let report = tradability::tradability_in(&ctx, entity, temporal).unwrap();
            let limit = price_limit::price_limit_in(&ctx, entity, temporal).unwrap();
            let row = tradability_row(&answer, entity);
            let expected_state = match report.state {
                tradability::Tradability::Tradable => "tradable",
                tradability::Tradability::NotTradable(_) => "not_tradable",
                tradability::Tradability::Unknown(_) => "unknown",
            };
            assert_eq!(row["state"], json!(expected_state), "{entity}");
            assert_eq!(row["reason"], json!(report.reason_code()), "{entity}");
            assert_eq!(row["limit_state"], json!(limit.state.as_str()), "{entity}");
            assert_eq!(
                row["risk_status"],
                json!(limit.status_class.map_or("unknown", |class| class.as_str())),
                "{entity}"
            );
            let rendered = |units: Option<i64>| match (units, limit.price_scale) {
                (Some(units), Some(scale)) => {
                    let divisor = 10_i64.pow(scale);
                    json!(format!(
                        "{}.{:0width$}",
                        units / divisor,
                        units.abs() % divisor,
                        width = scale as usize
                    ))
                }
                _ => Value::Null,
            };
            assert_eq!(row["limit_up"], rendered(limit.limit_up_units), "{entity}");
            assert_eq!(
                row["limit_down"],
                rendered(limit.limit_down_units),
                "{entity}"
            );
            assert_eq!(
                row["prev_close"],
                rendered(limit.prev_close_units),
                "{entity}"
            );
            assert_eq!(
                row["prev_close_day"],
                limit
                    .prev_close_day
                    .map_or(Value::Null, |day| json!(day.to_string())),
                "{entity}"
            );
            assert_eq!(
                row["limit_basis"],
                limit
                    .limit_basis
                    .map_or(Value::Null, |basis| json!(basis.as_str())),
                "{entity}"
            );
            assert_eq!(
                row["rule_id"],
                limit.rule_id.as_ref().map_or(Value::Null, |id| json!(id)),
                "{entity}"
            );
            assert_eq!(
                row["suspension"],
                report
                    .suspension
                    .as_ref()
                    .map_or(Value::Null, |suspension| {
                        json!(format!(
                            "{} {}..{}",
                            suspension.certainty.as_str(),
                            suspension.start_day,
                            suspension.end_day
                        ))
                    }),
                "{entity}"
            );
        }
        Ok::<(), crate::market_storage::Error>(())
    })
    .unwrap();

    // The algorithm labels are the research constants, not copies.
    assert_eq!(
        answer["algorithm_versions"],
        json!({
            "tradability": crate::research::tradability::ALGORITHM_VERSION,
            "price_limit": crate::research::price_limit::READ_ALGORITHM_VERSION,
        })
    );
    // `known_at` is not a parameter, and the block says so.
    assert_eq!(answer["temporal"]["known_at"], Value::Null);
    assert!(answer["inputs"].as_array().unwrap().iter().any(|input| {
        input["dataset"] == json!("suspensions") && input["version_id"].is_string()
    }));
}

#[test]
fn get_tradability_answers_every_fail_closed_state() {
    let f = derived_fixture();
    // Day 2 (2020-09-09) witnesses the gap and the confirmed suspension.
    let answer = f
        .api
        .call(
            "get_tradability",
            json!({
                "securities": [
                    "sz.000631", "sz.000004", "sh.510050", "sh.600519"
                ],
                "as_of": derived_days()[2].to_string(),
            }),
        )
        .unwrap();

    // Missing bars plus an unresolved gap is a data gap, never a suspension
    // (AC-M2-8).
    let gap = tradability_row(&answer, "sz.000631");
    assert_eq!(gap["state"], json!("unknown"));
    assert_eq!(gap["reason"], json!("data_gap"));
    assert_eq!(
        gap["suspension"],
        json!(format!(
            "unresolved_gap {}..{}",
            derived_days()[1],
            derived_days()[3]
        ))
    );

    // A confirmed suspension is a named event: not tradable.
    let suspended = tradability_row(&answer, "sz.000004");
    assert_eq!(suspended["state"], json!("not_tradable"));
    assert_eq!(suspended["reason"], json!("suspended"));

    // An ETF is not an A share; the tool says so instead of "not listed".
    let etf = tradability_row(&answer, "sh.510050");
    assert_eq!(etf["state"], json!("unknown"));
    assert_eq!(etf["reason"], json!("not_a_share"));
    assert_eq!(etf["limit_reason"], json!("not_a_share"));

    // A code the main table does not contain still gets a row.
    let absent = tradability_row(&answer, "sh.600519");
    assert_eq!(absent["state"], json!("unknown"));
    assert_eq!(absent["reason"], json!("not_in_master"));

    // Day 4 is the fifth calendar day, so the listing phase is `seasoned` and
    // the GEM band is knowable even though the risk status is not: every
    // status class resolves to the same rule from 2020-08-24.
    let answer = f
        .api
        .call(
            "get_tradability",
            json!({
                "securities": ["sz.300114", "sz.000001"],
                "as_of": derived_days()[4].to_string(),
            }),
        )
        .unwrap();

    let gem = tradability_row(&answer, "sz.300114");
    assert_eq!(gem["state"], json!("tradable"));
    assert_eq!(gem["reason"], json!("tradable"));
    assert_eq!(gem["limit_state"], json!("known"));
    // The status itself stays unknown: `known` here is the band invariant, and
    // must not be read as "classified as normal" (design §2.2).
    assert_eq!(gem["risk_status"], json!("unknown"));
    assert_eq!(gem["limit_reason"], Value::Null);
    // Prev close is the previous day's 不复权 close (2003 units at scale 2),
    // rendered from the series' own scale rather than assumed to be 2 places.
    assert_eq!(gem["prev_close"], json!("20.03"));
    assert_eq!(gem["prev_close_day"], json!(derived_days()[3].to_string()));
    assert!(gem["limit_up"].is_string(), "{gem}");
    assert!(gem["limit_down"].is_string(), "{gem}");

    // A main-board member with no risk interval: tradable, but the ST band
    // cannot be ruled out, so the limit is unknown (D-82 Q4: reported as it
    // is, not filled in with the 10% default).
    let main = tradability_row(&answer, "sz.000001");
    assert_eq!(main["state"], json!("tradable"));
    assert_eq!(main["limit_state"], json!("unknown"));
    assert_eq!(main["limit_reason"], json!("st_unknown"));
    assert_eq!(main["limit_up"], Value::Null);
    assert_eq!(main["limit_down"], Value::Null);
}

#[test]
fn get_tradability_reports_ex_rights_unverified_instead_of_a_biased_limit() {
    let f = derived_fixture();
    // 300115 has a cat1 event with an allotment component on the day itself,
    // so the reference price the band is computed from cannot be verified.
    // The day still has a bar, so the security is tradable; only the limit is
    // withheld, and it is withheld as `unknown`, never as the stale prev-close
    // band (design §3.2 / Q3(a), D-84 limit-v2).
    let answer = f
        .api
        .call(
            "get_tradability",
            json!({"securities": ["sz.300115"], "as_of": derived_days()[4].to_string()}),
        )
        .unwrap();
    let row = tradability_row(&answer, "sz.300115");
    assert_eq!(row["state"], json!("tradable"), "{row}");
    assert_eq!(row["limit_state"], json!("unknown"), "{row}");
    assert_eq!(row["limit_reason"], json!("ex_rights_unverified"), "{row}");
    assert_eq!(row["limit_up"], Value::Null);
    assert_eq!(row["limit_down"], Value::Null);
    // The previous close is still reported: it exists, it is just not the
    // basis of a limit.
    assert_eq!(row["prev_close"], json!("60.03"));
    // No limit price, no basis: the column is null, not the previous close.
    assert_eq!(row["limit_basis"], Value::Null, "{row}");
}

#[test]
fn get_tradability_names_the_basis_of_each_limit() {
    let f = derived_limit_basis_fixture();

    // 2020-09-11: the GEM band is the same for every status, so all three
    // reach a basis.
    let answer = f
        .api
        .call(
            "get_tradability",
            json!({
                "securities": ["sz.300114", "sz.300116", "sz.300115"],
                "as_of": derived_days()[4].to_string(),
            }),
        )
        .unwrap();

    // No ex-rights event: the previous close is the basis.
    let prev = tradability_row(&answer, "sz.300114");
    assert_eq!(prev["limit_state"], json!("known"), "{prev}");
    assert_eq!(prev["limit_basis"], json!("prev_close"), "{prev}");

    // An ex-rights event after the cutoff: the computed reference price. The
    // code is also suspended that day, and the basis is still reported: it
    // describes the day's limit, not whether an order would have filled.
    let reference = tradability_row(&answer, "sz.300116");
    assert_eq!(reference["state"], json!("not_tradable"), "{reference}");
    assert_eq!(reference["reason"], json!("suspended"), "{reference}");
    assert_eq!(reference["limit_state"], json!("known"), "{reference}");
    assert_eq!(
        reference["limit_basis"],
        json!("reference_price"),
        "{reference}"
    );
    assert!(reference["limit_up"].is_string(), "{reference}");

    // Before the 2010-09-14 cutoff the official r7 reference price wins: prev
    // close 25.11, r7 2702 fen (27.02), 10% band → 29.72 / 24.32.
    let answer = f
        .api
        .call(
            "get_tradability",
            json!({"securities": ["sz.300118"], "as_of": "2009-11-17"}),
        )
        .unwrap();
    let official = tradability_row(&answer, "sz.300118");
    assert_eq!(official["limit_state"], json!("known"), "{official}");
    assert_eq!(official["limit_basis"], json!("official_r7"), "{official}");
    assert_eq!(official["prev_close"], json!("25.11"), "{official}");
    assert_eq!(official["limit_up"], json!("29.72"), "{official}");
    assert_eq!(official["limit_down"], json!("24.32"), "{official}");
}

#[test]
fn get_tradability_says_input_absent_when_suspensions_were_never_written() {
    // A fresh database has a main table and a calendar but no suspensions
    // dataset. Every row must be unknown + input_absent: "no row" is not "no
    // suspension" (G8).
    let f = fixture();
    commit_calendar_days(&f, &derived_days());
    let answer = f
        .api
        .call(
            "get_tradability",
            json!({"securities": ["sz.000001"], "as_of": "2020-09-09"}),
        )
        .unwrap();
    assert_eq!(answer["columns"]["state"], json!(["unknown"]));
    assert_eq!(answer["columns"]["reason"], json!(["input_absent"]));
    assert_eq!(answer["columns"]["limit_state"], json!(["unknown"]));
    assert_eq!(answer["columns"]["limit_reason"], json!(["input_absent"]));
    // No input, no basis: the column exists and is null, never a borrowed
    // label.
    assert_eq!(answer["columns"]["limit_basis"], json!([null]));
    assert!(
        answer["limitations"][0]
            .as_str()
            .unwrap()
            .contains("input_absent")
    );

    // Both branches produce a table, so they must carry the same column set:
    // a caller can read a response without asking which branch answered.
    let present = derived_fixture()
        .api
        .call(
            "get_tradability",
            json!({"securities": ["sz.000001"], "as_of": derived_days()[2].to_string()}),
        )
        .unwrap();
    let mut absent_columns: Vec<&String> = answer["columns"].as_object().unwrap().keys().collect();
    let mut present_columns: Vec<&String> =
        present["columns"].as_object().unwrap().keys().collect();
    absent_columns.sort();
    present_columns.sort();
    assert_eq!(absent_columns, present_columns);
}

#[test]
fn get_tradability_and_market_cap_reject_known_at_with_a_pointer_to_get_security_state() {
    let f = derived_fixture();
    for (tool, input) in [
        (
            "get_tradability",
            json!({"securities": ["sz.000001"], "as_of": "2020-09-09", "known_at": "2020-09-10"}),
        ),
        (
            "get_market_cap",
            json!({"securities": ["sh.600028"], "scope": "float_a", "as_of": "2020-09-09",
                   "known_at": "2020-09-10"}),
        ),
    ] {
        let error = f.api.call(tool, input).unwrap_err();
        assert_eq!(error.code, "invalid_arguments", "{tool}");
        assert!(error.message.contains("known_at is not accepted"), "{tool}");
        assert!(error.message.contains("get_security_state"), "{tool}");
    }
}

#[test]
fn derived_tools_reject_indices_and_oversized_or_duplicate_batches() {
    let f = derived_fixture();
    for tool in ["get_tradability", "get_market_cap"] {
        let mut input = json!({"securities": ["sh.000300"], "as_of": "2020-09-09"});
        if tool == "get_market_cap" {
            input["scope"] = json!("float_a");
        }
        let error = f.api.call(tool, input).unwrap_err();
        assert_eq!(error.code, "invalid_arguments", "{tool}");
        assert!(error.message.contains("get_index_bars"), "{tool}");

        // 101 entities: the v2 path's 1..100 rule applies here too.
        let many: Vec<String> = (0..101)
            .map(|index| format!("sz.{:06}", 1000 + index))
            .collect();
        let mut input = json!({"securities": many, "as_of": "2020-09-09"});
        if tool == "get_market_cap" {
            input["scope"] = json!("float_a");
        }
        let error = f.api.call(tool, input).unwrap_err();
        assert_eq!(error.code, "invalid_arguments", "{tool}");
        assert!(error.message.contains("1..100"), "{}", error.message);

        let mut input = json!({"securities": ["sz.000001", "sz.000001"]});
        if tool == "get_market_cap" {
            input["scope"] = json!("float_a");
        }
        let error = f.api.call(tool, input).unwrap_err();
        assert_eq!(error.code, "invalid_arguments", "{tool}");
        assert!(error.message.contains("duplicate"), "{}", error.message);
    }
}

#[test]
fn get_market_cap_matches_the_research_function_and_names_its_scope() {
    use crate::research::market_cap::{MarketCapOptions, Reconciliation, ShareScope, market_cap};
    use crate::research::temporal::Temporal;

    let f = derived_fixture();
    let as_of = derived_days()[2];
    let answer = f
        .api
        .call(
            "get_market_cap",
            json!({"securities": ["sh.600028", "sz.000001"], "scope": "float_a",
                   "as_of": as_of.to_string()}),
        )
        .unwrap();
    assert_eq!(answer["layout"], "table");
    assert_eq!(answer["scope"], json!("float_a"));
    assert_eq!(answer["entities"], json!(["sh.600028", "sz.000001"]));

    let MarketBackend::Compact(repo) = &f.backend else {
        panic!("fixture must open the compact backend");
    };
    repo.read(|c| {
        let temporal = Temporal {
            as_of,
            known_at: None,
        };
        for (index, entity) in ["sh.600028", "sz.000001"].into_iter().enumerate() {
            let expected = market_cap(
                c,
                entity,
                temporal,
                ShareScope::FloatA,
                &MarketCapOptions::default(),
            )
            .unwrap();
            let state = answer["columns"]["state"][index].clone();
            assert_eq!(
                state,
                json!(match expected.state {
                    crate::research::market_cap::MarketCapState::Known => "known",
                    crate::research::market_cap::MarketCapState::Unknown => "unknown",
                }),
                "{entity}"
            );
            assert_eq!(
                answer["columns"]["reason"][index],
                expected
                    .reason
                    .map_or(Value::Null, |reason| json!(reason.as_str())),
                "{entity}"
            );
            // 分 × shares → 元 with two decimals.
            assert_eq!(
                answer["columns"]["market_cap"][index],
                expected.value_units.map_or(Value::Null, |units| {
                    json!(format!("{}.{:02}", units / 100, units % 100))
                }),
                "{entity}"
            );
            assert_eq!(
                answer["columns"]["shares"][index],
                expected.shares.map_or(Value::Null, |value| json!(value)),
                "{entity}"
            );
            assert_eq!(
                answer["columns"]["share_event_day"][index],
                expected
                    .share_day
                    .map_or(Value::Null, |day| json!(day.to_string())),
                "{entity}"
            );
            assert_eq!(
                answer["columns"]["reconciliation"][index],
                json!(Reconciliation::NotAvailable.as_str()),
                "{entity}"
            );
        }
        Ok::<(), crate::market_storage::Error>(())
    })
    .unwrap();

    // 600028 has 1.0e9 float shares and closed at 10.02 on day 2.
    assert_eq!(answer["columns"]["state"][0], json!("known"));
    assert_eq!(answer["columns"]["shares"][0], json!(1_000_000_000i64));
    assert_eq!(answer["columns"]["market_cap"][0], json!("10020000000.00"));

    // 000001 has no capital event: unknown, not a stale value.
    assert_eq!(answer["columns"]["state"][1], json!("unknown"));
    assert_eq!(answer["columns"]["reason"][1], json!("no_share_event"));

    // `a_share_total` is always unknown and says why.
    let total = f
        .api
        .call(
            "get_market_cap",
            json!({"securities": ["sh.600028"], "scope": "a_share_total",
                   "as_of": as_of.to_string()}),
        )
        .unwrap();
    assert_eq!(total["columns"]["state"], json!(["unknown"]));
    assert_eq!(
        total["columns"]["reason"],
        json!(["a_share_total_not_admitted"])
    );

    // `company_total` carries the A+B+H caveat.
    let company = f
        .api
        .call(
            "get_market_cap",
            json!({"securities": ["sh.600028"], "scope": "company_total",
                   "as_of": as_of.to_string()}),
        )
        .unwrap();
    assert_eq!(company["columns"]["shares"], json!([1_200_000_000i64]));
    assert!(
        company["limitations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line.as_str().unwrap().contains("A+B+H"))
    );
    assert_eq!(
        company["algorithm_versions"],
        json!({"market_cap": crate::research::market_cap::ALGORITHM_VERSION})
    );
}

#[test]
fn derived_tools_are_not_paginated() {
    let f = derived_fixture();
    for tool in ["get_tradability", "get_market_cap"] {
        let mut input = json!({"securities": ["sz.000001"], "as_of": "2020-09-09"});
        if tool == "get_market_cap" {
            input["scope"] = json!("float_a");
        }
        for field in ["page_size", "cursor"] {
            let mut rejected = input.clone();
            rejected[field] = if field == "page_size" {
                json!(10)
            } else {
                json!("x")
            };
            let error = f.api.call(tool, rejected).unwrap_err();
            assert_eq!(error.code, "invalid_arguments", "{tool}/{field}");
        }
        let answer = f.api.call(tool, input).unwrap();
        assert!(answer.get("next_cursor").is_none(), "{tool}");
        assert!(answer.get("page_complete").is_none(), "{tool}");
        assert!(answer.get("snapshot_id").is_none(), "{tool}");
    }
}

#[test]
fn derived_tools_admit_through_the_read_budget() {
    use crate::data_supply::cursor_cache::CursorLimits;
    use crate::data_supply::query::Queries;
    use crate::data_supply::read_limits::{ReadBudget, ReadLimits};

    let mut f = derived_fixture();
    // A budget that cannot fit anything: the same admission gate the v2 read
    // path uses (#547). The tool must be refused, not served outside it.
    let repository = f.api.queries.repository.clone();
    f.api.queries = Queries::with_limits(
        repository,
        &ReadLimits::new(
            CursorLimits::PRODUCTION,
            ReadBudget::with_probe(Some(1), || Some(usize::MAX / 4)),
        ),
    );
    for tool in ["get_tradability", "get_market_cap"] {
        let mut input = json!({"securities": ["sz.000001"], "as_of": "2020-09-09"});
        if tool == "get_market_cap" {
            input["scope"] = json!("float_a");
        }
        let error = f.api.call(tool, input).unwrap_err();
        assert_eq!(error.code, "memory_budget_exceeded", "{tool}");
    }
}

/// Measurement probe, not a regression gate:
/// `cargo nextest run --run-ignored only -E 'test(probe_derived_tools)' --no-capture`.
///
/// Builds a fixture the size of the production read-model inputs (7.4k main
/// table rows, ~70k suspension rows, a 300-day calendar, rules) through the
/// production writers, then reports the peak resident growth and the wall time
/// of one `get_tradability` call for 1, 10 and 100 entities. `READ_RESERVE` in
/// `derived_tools.rs` is set from the 100-entity figure.
#[test]
#[ignore]
fn probe_derived_tools_peak_heap_and_latency() {
    use crate::security_master::assembly::Assembly;
    use crate::security_master::suspensions::SuspensionSet;
    use crate::security_master::{
        Exchange, SuspensionCertainty, SuspensionInterval, SuspensionReason,
    };
    use crate::sqlite_market::{evidence_store, security, suspensions};
    use std::time::Instant;

    fn status_kib(key: &str) -> usize {
        std::fs::read_to_string("/proc/self/status")
            .unwrap()
            .lines()
            .find(|line| line.starts_with(key))
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|value| value.parse().ok())
            .unwrap_or(0)
            * 1024
    }
    fn mib(bytes: usize) -> f64 {
        bytes as f64 / 1048576.0
    }

    let f = fixture();
    // 300 trading days.
    let mut calendar_days = Vec::new();
    let mut cursor = day(2019, 1, 2);
    while calendar_days.len() < 300 {
        if !matches!(cursor.format("%a").to_string().as_str(), "Sat" | "Sun") {
            calendar_days.push(cursor);
        }
        cursor = cursor.succ_opt().unwrap();
    }
    commit_calendar_days(&f, &calendar_days);
    let as_of = calendar_days[200];

    let MarketBackend::Compact(repo) = &f.backend else {
        panic!("fixture must open the compact backend");
    };
    repo.write(|c| {
        let generation = evidence_store::put_evidence(
            c,
            crate::compact_market::EvidenceKind::Generation,
            &json!({"generation": "gen-probe"}),
        )?;
        let binding = evidence_store::put_source_binding(
            c,
            &crate::compact_market::SourceBinding {
                generation_id: "gen-probe".into(),
                generation_manifest_sha256: [0x0b; 32],
                day_member_sha256: [0x0c; 32],
                security_event_sha256: [0x0e; 32],
                evidence_digest: generation,
            },
        )?;
        let evidence = evidence_store::put_evidence(
            c,
            crate::compact_market::EvidenceKind::Version,
            &serde_json::to_value(crate::compact_market::VersionEvidence {
                source: "official".into(),
                coverage_reason: "probe".into(),
                unit_currency: units(),
                adjustment_evidence: crate::market_data::AdjustmentEvidence::confirmed(
                    "raw", "probe",
                ),
                excluded_bars: None,
            })
            .unwrap(),
        )?;

        // 7.4k main-table rows: 4,000 SZ main-board codes and 3,400 SH ones.
        let mut rows = Vec::new();
        for index in 1..=4_000u32 {
            rows.push(derived_lifecycle(
                Exchange::Sz,
                &format!("{index:06}"),
                Some(day(2018, 1, 2)),
                None,
                0,
            ));
        }
        for index in 0..3_400u32 {
            rows.push(derived_lifecycle(
                Exchange::Sh,
                &format!("{:06}", 600_000 + index),
                Some(day(2018, 1, 2)),
                None,
                0,
            ));
        }
        security::replace_security_master(
            c,
            &Assembly {
                rows,
                migrations: Vec::new(),
                conflicts: Vec::new(),
                pending_sources: Vec::new(),
                unresolved: Vec::new(),
                exchange_as_of: Some(calendar_days[299]),
            },
            binding,
            "task-master",
            "derived:security_master",
        )?;

        // ~70k suspension rows: 9 gap intervals per SZ code, one per 30 days.
        let mut suspension_rows = Vec::new();
        for index in 1..=4_000u32 {
            for step in 0..9usize {
                let start = calendar_days[step * 30 + 10];
                let end = calendar_days[step * 30 + 12];
                suspension_rows.push(SuspensionInterval {
                    exchange: Exchange::Sz,
                    code: format!("{index:06}"),
                    start_day: start,
                    end_day: end,
                    certainty: SuspensionCertainty::UnresolvedGap,
                    missing_days: 3,
                    reason: SuspensionReason::MissingBarNoEvent,
                    known_at: None,
                    source_kind: "probe".into(),
                    source_record_id: None,
                });
            }
        }
        for index in 0..3_400u32 {
            for step in 0..9usize {
                let start = calendar_days[step * 30 + 40];
                let end = calendar_days[step * 30 + 42];
                suspension_rows.push(SuspensionInterval {
                    exchange: Exchange::Sh,
                    code: format!("{:06}", 600_000 + index),
                    start_day: start,
                    end_day: end,
                    certainty: SuspensionCertainty::UnresolvedGap,
                    missing_days: 3,
                    reason: SuspensionReason::MissingBarNoEvent,
                    known_at: None,
                    source_kind: "probe".into(),
                    source_record_id: None,
                });
            }
        }
        let count = |filter: fn(&SuspensionInterval) -> bool| {
            suspension_rows.iter().filter(|row| filter(row)).count()
        };
        let set = SuspensionSet {
            confirmed_rows: count(|row| row.certainty == SuspensionCertainty::Confirmed),
            gap_rows: count(|row| row.certainty == SuspensionCertainty::UnresolvedGap),
            single_day_gap_rows: count(|row| {
                row.certainty == SuspensionCertainty::UnresolvedGap && row.missing_days == 1
            }),
            multi_day_gap_rows: count(|row| {
                row.certainty == SuspensionCertainty::UnresolvedGap && row.missing_days > 1
            }),
            deferred_beyond_calendar: 0,
            deferred_samples: Vec::new(),
            skipped_no_trading_day: 0,
            skipped_samples: Vec::new(),
            pending_sources: Vec::new(),
            rows: suspension_rows,
        };
        suspensions::replace_suspensions(
            c,
            &set,
            binding,
            "task-suspensions",
            "derived:suspensions",
        )?;
        crate::sqlite_market::rules::replace_trading_rules(
            c,
            &crate::trading_rules::rule_book().unwrap(),
            binding,
            "task-rules",
            "derived:price_limit_rules",
        )?;

        // One raw bar on `as_of` for the 100 entities the probe asks about, so
        // the answer is `tradable` and the price-limit path runs too.
        let mut seed = 0x30u8;
        for index in 1..=100u32 {
            seed = seed.wrapping_add(1);
            let entity = format!("sz.{index:06}");
            let close = 1000 + index as i64;
            let bar = crate::compact_market::CompactBar {
                trade_day: crate::compact_market::epoch_day(as_of),
                open_units: close,
                high_units: close + 10,
                low_units: close - 10,
                close_units: close,
                volume: Some(100),
                amount: Some("100500.00".into()),
                r7_raw: Some(7),
            };
            let key = series::insert_series(
                c,
                &crate::sqlite_market::series::NewSeries {
                    entity: entity.clone(),
                    period: crate::market_data::Period::Day,
                    adjustment: Adjustment::Raw,
                    instrument_class: crate::market_data::INSTRUMENT_CLASS_STOCK,
                    price_scale: 2,
                    content_digest: [seed; 32],
                    record_count: 1,
                    first_day: bar.trade_day,
                    last_day: bar.trade_day,
                    coverage_status: crate::market_data::CoverageStatus::Complete,
                    source_binding_digest: binding,
                    evidence_digest: evidence,
                    last_event_seq: 1,
                    created_at: "2026-10-26T00:00:00Z".into(),
                    updated_at: "2026-10-26T00:00:00Z".into(),
                },
            )?;
            series::insert_bars(c, key, &[bar])?;
        }
        Ok(())
    })
    .unwrap();

    let entities: Vec<String> = (1..=100).map(|index| format!("sz.{index:06}")).collect();
    let ask = |count: usize| {
        let securities: Vec<Value> = entities[..count]
            .iter()
            .map(|entity| json!(entity))
            .collect();
        json!({"securities": securities, "as_of": as_of.to_string()})
    };

    // `VmHWM` is a process-wide high-water mark that the fixture build already
    // pushed up, so the call's own peak is sampled from a watchdog thread:
    // `RssAnon` every millisecond while the call runs.
    let sample = |call: &dyn Fn() -> Value| {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let peak = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let watch = {
            let stop = stop.clone();
            let peak = peak.clone();
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    peak.fetch_max(status_kib("RssAnon"), std::sync::atomic::Ordering::Relaxed);
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            })
        };
        let before = status_kib("RssAnon");
        let started = Instant::now();
        let answer = call();
        let elapsed = started.elapsed();
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        watch.join().unwrap();
        let peak = peak.load(std::sync::atomic::Ordering::Relaxed);
        (answer, elapsed, before, peak)
    };

    for count in [1usize, 10, 100] {
        // Three runs: the first is cold (the shared inputs are decoded), the
        // rest show the steady state. The peak is reported for the first run
        // because that is the one the reserve has to cover.
        let mut walls = Vec::new();
        for run in 0..3 {
            let (answer, elapsed, before, peak) =
                sample(&|| f.api.call("get_tradability", ask(count)).unwrap());
            assert_eq!(answer["entities"].as_array().unwrap().len(), count);
            walls.push(elapsed.as_secs_f64() * 1000.0);
            if run == 0 {
                println!(
                    "get_tradability entities={count:>3}: sampled_peak_growth={:>6.1} MiB \
                     (rss_anon {} -> {} MiB), response={} B",
                    mib(peak.saturating_sub(before)),
                    mib(before),
                    mib(peak),
                    serde_json::to_vec(&answer).unwrap().len(),
                );
            }
        }
        println!(
            "get_tradability entities={count:>3}: wall ms runs={:?}",
            walls
                .iter()
                .map(|ms| (ms * 10.0).round() / 10.0)
                .collect::<Vec<_>>()
        );
    }
    println!(
        "READ_RESERVE = {} B ({:.1} MiB)",
        crate::data_supply::derived_tools::READ_RESERVE,
        mib(crate::data_supply::derived_tools::READ_RESERVE)
    );
}
