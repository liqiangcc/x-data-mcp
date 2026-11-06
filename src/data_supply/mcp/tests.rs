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

/// AC-V8-6: adjustment events expose effective-day facts and never a
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
    assert_eq!(page["columns"]["category"], json!([1]));
    assert_eq!(page["columns"]["c1"], json!(["1.50"]));
    assert!(page["columns"].get("known_at").is_none(), "{page}");
    assert!(page["columns"].get("effective_day").is_none(), "{page}");
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
    assert!(limitations.contains("BJ is not available"), "{limitations}");
    assert!(
        limitations.contains("sh: 1 window(s) between 2000-01-01 and 2003-12-31"),
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
    let unavailable = super::mapped(crate::market_storage::Error::Invalid(
        "adjustment_unavailable: ETF 前复权 is not delivered".into(),
    ));
    assert_eq!(unavailable.code, "adjustment_unavailable");
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
