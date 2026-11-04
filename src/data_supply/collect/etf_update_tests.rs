//! M3-2 PR-C2: ETF 不复权 daily update through `plan_market_update` /
//! `run_market_update`, on a real compact database and two real verified
//! generations.
//!
//! The mirror content is synthetic (D-47, D-70 Q12): the real `.day` bytes are
//! not committed. Provenance of the anchors reused here (sha256 of the real
//! unpacked members, see `docs/tasks/issue-509-m3-2-etf.md` §12):
//! `sh510050.day` `02f0e517…2e16`, `sz159919.day` `faba834e…d9b3`,
//! `sz159901.day` `b75467e2…a696`, `sz159338.day` `edb7ddae…017d`.
use serde_json::{Value, json};
use std::path::Path;

use super::tests::{
    day_record, day_record_r7, day_u32, drive, gbbq_factor_record, gbbq_member, gbbq_record,
    seed_master, set_private, write_zip,
};
use crate::acquire::{AcquireService, Source};
use crate::catalog::{
    CatalogSnapshot, CompletenessEvidence, Exchange, IndustryInfo, SecurityRecord, SecurityType,
};
use crate::compact_market::EvidenceKind;
use crate::data_supply::Access;
use crate::data_supply::generation::GenerationSource;
use crate::data_supply::mcp::DataApi;
use crate::data_supply::plan::{PlanItem, PlanSeries, resolve_plan};
use crate::data_supply::query::Queries;
use crate::data_supply::wire::{Dataset, PlanScope, Query, Row};
use crate::market_data::{Adjustment, Period};
use crate::mcp::ToolHandler;
use crate::sqlite_catalog::SqliteCatalog;
use crate::sqlite_market::backend::MarketBackend;
use crate::sqlite_market::{derived, evidence_store, series, series_commit};
use crate::sqlite_supply::SqliteSupply;
use crate::sqlite_tasks::SqliteTasks;
use crate::task_control::{Status, TaskRepository, TaskSnapshot};

const HAND_MARKER: u32 = 0xC364_0053;

fn a_days(with_today: bool) -> Vec<u8> {
    let mut bars = vec![
        day_record(day_u32(3), [1128, 1130, 1125, 1129], 0.0, 100),
        day_record(day_u32(2), [1129, 1132, 1128, 1131], 0.0, 110),
        day_record(day_u32(1), [1131, 1135, 1130, 1134], 0.0, 120),
    ];
    if with_today {
        bars.push(day_record(day_u32(0), [1134, 1140, 1133, 1138], 0.0, 130));
    }
    bars.concat()
}

/// ETF 3dp bars (厘 integers): the first publication ends yesterday.
fn etf_base() -> Vec<[u8; 32]> {
    vec![
        day_record(day_u32(3), [41680, 41820, 41600, 41780], 0.0, 10_000),
        day_record(day_u32(2), [41780, 41900, 41700, 41860], 0.0, 11_000),
        day_record(day_u32(1), [41860, 42000, 41800, 41953], 0.0, 12_000),
    ]
}
fn etf_today() -> [u8; 32] {
    day_record(day_u32(0), [41953, 42100, 41900, 42050], 0.0, 13_000)
}
fn etf_with_today() -> Vec<u8> {
    let mut bars = etf_base();
    bars.push(etf_today());
    bars.concat()
}
fn hand_base() -> Vec<[u8; 32]> {
    vec![
        day_record(day_u32(2), [1140, 1160, 1130, 1150], 0.0, 1_000),
        day_record(day_u32(1), [1150, 1160, 1140, 1155], 0.0, 1_000),
    ]
}
/// `sz159338` anchor: 42,937,950 hands ×100 = 4,293,795,000 shares, amount
/// 4,937,864,250 yuan = 1.15 yuan per share, inside the bar's raw/1000 range.
fn hand_today() -> [u8; 32] {
    day_record_r7(
        day_u32(0),
        [1150, 1160, 1140, 1158],
        4_937_864_250.0,
        42_937_950,
        HAND_MARKER,
    )
}
fn delisted_days() -> Vec<u8> {
    [
        day_record(day_u32(4), [2000, 2050, 1990, 2040], 2_040_000.0, 1_000),
        day_record(day_u32(3), [2040, 2060, 2030, 2050], 0.0, 0),
        day_record(day_u32(2), [2050, 2050, 2050, 2050], 0.0, 0),
    ]
    .concat()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Generation {
    First,
    Second,
}

/// One mirror generation. Second-generation deltas, per ETF:
///
/// | ETF | first generation | second generation | expected update |
/// |---|---|---|---|
/// | `sh510050` | 3 bars | + today | append |
/// | `sz159919` | 3 bars | identical | no_change |
/// | `sz159901` | 3 bars | identical bytes, one new cat1 event | metadata |
/// | `sz159338` | 2 bars | + a valid hand-recorded bar | append, ×100 |
/// | `sh510300` | 3 bars | first bar rewritten + today | `etf_history_rewrite` |
/// | `sz159922` | 3 bars | + a hand-marked bar whose amount disagrees | item failure |
fn write_mirror(dir: &Path, generation: Generation) {
    std::fs::create_dir_all(dir).unwrap();
    let second = generation == Generation::Second;
    write_zip(
        &dir.join("base.zip"),
        &[
            ("tdxhy.cfg", b"1|600101|T090302|||X6102\r\n".as_slice()),
            ("infoharbor_ex.code", b"600101|name|kw\r\n".as_slice()),
        ],
    );
    let mut records = vec![
        gbbq_record(1, "600101", 20240101, 1),
        gbbq_record(1, "600102", 20240101, 1),
        gbbq_factor_record(0, "159919", 20250331, 1, [0.661, 0.0, 0.0, 0.0]),
        gbbq_factor_record(0, "159901", 20210412, 11, [0.0, 0.0, 2.0, 0.0]),
        gbbq_factor_record(0, "159901", 20101122, 11, [0.0, 0.0, 5.0, 0.0]),
    ];
    if second {
        // A new dividend between the generations: the `.day` member of
        // `sz159901` is byte-identical, only its event digest moves.
        records.push(gbbq_factor_record(
            0,
            "159901",
            20260101,
            1,
            [0.1, 0.0, 0.0, 0.0],
        ));
    }
    write_zip(
        &dir.join("gbbq.zip"),
        &[("gbbq", gbbq_member(&records).as_slice())],
    );

    let a = a_days(second);
    let a_short = day_record(day_u32(0), [1128, 1130, 1125, 1129], 0.0, 100).to_vec();
    let etf_plain = etf_base().concat();
    let etf_appended = etf_with_today();
    let hand = {
        let mut bars = hand_base();
        if second {
            bars.push(hand_today());
        }
        bars.concat()
    };
    let rewritten = {
        // The first bar's close moves: ETF 不复权 history must be immutable.
        let mut bars = etf_base();
        bars[0] = day_record(day_u32(3), [41680, 41820, 41600, 41790], 0.0, 10_000);
        bars.push(etf_today());
        bars.concat()
    };
    let contradictory = {
        // Hand marker present, amount absent: marker and amount/volume
        // disagree, so the whole item must fail and the series stay as it was.
        let mut bars = etf_base();
        bars.push(day_record_r7(
            day_u32(0),
            [41953, 42100, 41900, 42050],
            0.0,
            13_000,
            HAND_MARKER,
        ));
        bars.concat()
    };
    let delisted = delisted_days();
    let fillers: Vec<String> = (201..=270)
        .map(|code| format!("sh\\lday\\sh600{code:03}.day"))
        .collect();
    let mut members: Vec<(&str, &[u8])> = vec![
        ("sh\\lday\\sh600101.day", a.as_slice()),
        ("sh\\lday\\sh600102.day", a.as_slice()),
        (
            "sh\\lday\\sh510050.day",
            if second {
                etf_appended.as_slice()
            } else {
                etf_plain.as_slice()
            },
        ),
        ("sz\\lday\\sz159919.day", etf_plain.as_slice()),
        ("sz\\lday\\sz159901.day", etf_plain.as_slice()),
        ("sz\\lday\\sz159338.day", hand.as_slice()),
        (
            "sh\\lday\\sh510300.day",
            if second {
                rewritten.as_slice()
            } else {
                etf_plain.as_slice()
            },
        ),
        (
            "sz\\lday\\sz159922.day",
            if second {
                contradictory.as_slice()
            } else {
                etf_plain.as_slice()
            },
        ),
        ("sh\\lday\\sh510070.day", delisted.as_slice()),
    ];
    for name in &fillers {
        members.push((name.as_str(), a_short.as_slice()));
    }
    write_zip(&dir.join("hsjday.zip"), &members);
}

fn install(service: &AcquireService, request: &str, expected_previous: Option<&str>) -> String {
    let acceptance = service.acquire_generation(request).unwrap();
    drive(service);
    let generation_id = service
        .task(&acceptance.task_id)
        .unwrap()
        .generation_id
        .unwrap();
    let verified = service
        .verify_generation(&format!("verify-{request}"), &generation_id)
        .unwrap();
    assert_eq!(verified.state, "consistency_verified", "{verified:?}");
    let activated = service
        .activate_generation(
            &format!("activate-{request}"),
            &generation_id,
            expected_previous,
        )
        .unwrap();
    assert_eq!(activated.state, "active", "{activated:?}");
    generation_id
}

fn catalog_fixture() -> CatalogSnapshot {
    let now = chrono::Utc::now();
    let source = "fixture:catalog".to_string();
    CatalogSnapshot {
        source: source.clone(),
        acquired_at: now,
        completeness: CompletenessEvidence::Complete { expected_count: 2 },
        records: ["600101", "600102"]
            .iter()
            .map(|code| SecurityRecord {
                symbol: (*code).to_string(),
                name: "样例".into(),
                exchange: Exchange::Shanghai,
                security_type: SecurityType::AShare,
                source_security_id: format!("sh{code}"),
                industry: IndustryInfo::Missing {
                    reason: "fixture unknown".into(),
                },
                source: source.clone(),
                acquired_at: now,
            })
            .collect(),
    }
}

/// Two extra current ETFs the shared master seed does not carry.
fn seed_extra_etf_master(backend: &MarketBackend) {
    let MarketBackend::Compact(repo) = backend else {
        panic!("fixture must open the compact backend");
    };
    repo.tx(|c| {
        let binding = series_commit::put_generation_binding(
            c,
            "gen-1",
            &"0b".repeat(32),
            &"0c".repeat(32),
            &"0e".repeat(32),
            &json!({"snapshot_inputs": []}),
        )?;
        let evidence =
            evidence_store::put_evidence(c, EvidenceKind::Continuity, &json!({"mode": "test"}))?;
        for (exchange, code) in [("sh", "510300"), ("sz", "159922")] {
            c.execute(
                "INSERT INTO security_master(exchange,code,effective_from,effective_to,\
                 instrument_class,issuer_resolution,listing_date,listing_source,\
                 delisting_date,delisting_source,delisting_reason_class,listing_conflict,\
                 delisting_conflict,source_binding_digest,evidence_digest) \
                 VALUES(?1,?2,0,NULL,2,'code_date',0,'exchange_list',NULL,'unknown','other',0,0,?3,?4)",
                rusqlite::params![exchange, code, binding.as_slice(), evidence.as_slice()],
            )
            .unwrap();
        }
        Ok::<(), crate::market_storage::Error>(())
    })
    .unwrap();
}

struct Env {
    api: DataApi,
    backend: MarketBackend,
    tasks: SqliteTasks,
    generation: GenerationSource,
    service: AcquireService,
    mirror: tempfile::TempDir,
    gen_first: String,
    _dirs: Vec<tempfile::TempDir>,
}

fn run_idle(api: &DataApi) {
    for _ in 0..8 {
        let reply = api.call("run_worker_once", json!({})).unwrap();
        if reply["idle"] == json!(true) {
            return;
        }
    }
    panic!("worker did not go idle");
}

/// Collected first generation: two A-shares through the explicit path and the
/// ETF set through the `sh_sz_etf_listed_ever` scope, like production.
fn setup() -> Env {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), Generation::First);
    let root = tempfile::tempdir().unwrap();
    let service = AcquireService::open(root.path(), Source::Mirror(mirror.path().into())).unwrap();
    let gen_first = install(&service, "req-first", None);

    let db = tempfile::tempdir().unwrap();
    let path = db.path().join("m.db");
    let tasks = SqliteTasks::new(&path);
    tasks.initialize_compact().unwrap();
    set_private(&path);
    let backend = MarketBackend::open(&tasks).unwrap();
    SqliteCatalog::new(tasks.clone())
        .publish_fixture("catalog-1", catalog_fixture(), None)
        .unwrap();
    seed_master(&backend);
    seed_extra_etf_master(&backend);
    let generation = GenerationSource::new(root.path());
    let api = DataApi::new_with_backend(
        tasks.clone(),
        backend.clone(),
        "run".into(),
        Access::local(),
        Some(generation.clone()),
    )
    .unwrap();
    api.call(
        "start_collection",
        json!({
            "request_id": "seed-a",
            "dataset": "market.daily",
            "contract_version": "v1",
            "selection": {"kind": "explicit", "securities": [
                {"exchange": "sh", "code": "600101"},
                {"exchange": "sh", "code": "600102"},
            ]},
        }),
    )
    .unwrap();
    run_idle(&api);
    api.call(
        "start_collection",
        json!({
            "request_id": "seed-etf",
            "dataset": "market.daily",
            "contract_version": "v2",
            "selection": {"kind": "scope", "scope": "sh_sz_etf_listed_ever"},
        }),
    )
    .unwrap();
    run_idle(&api);
    Env {
        api,
        backend,
        tasks,
        generation,
        service,
        mirror,
        gen_first,
        _dirs: vec![root, db],
    }
}

impl Env {
    fn advance_generation(&self) {
        write_mirror(self.mirror.path(), Generation::Second);
        let second = install(&self.service, "req-second", Some(&self.gen_first));
        assert_ne!(second, self.gen_first);
    }
    fn plan(&self, dataset: &str, scope: Option<&str>) -> Value {
        let mut input = json!({
            "dataset": dataset,
            "contract_version": "v1",
            "end_date": crate::calendar::shanghai_today().to_string(),
        });
        if let Some(scope) = scope {
            input["scope"] = json!(scope);
        }
        self.api.call("plan_market_update", input).unwrap()
    }
    fn run(&self, request: &str, view: &Value, scope: Option<&str>) -> TaskSnapshot {
        let mut input = json!({
            "dataset": "market.daily",
            "contract_version": "v1",
            "request_id": request,
            "target_day": view["target_day"],
            "plan_id": view["plan_id"],
        });
        if let Some(scope) = scope {
            input["scope"] = json!(scope);
        }
        let accepted = self.api.call("run_market_update", input).unwrap();
        let task_id = accepted["task_id"].as_str().unwrap().to_string();
        run_idle(&self.api);
        self.tasks.read_task(&task_id).unwrap().unwrap()
    }
    fn etf_head(&self, entity: &str) -> (String, i64, i64) {
        self.backend
            .read(|c| {
                let row = series::read_series_adjusted(c, entity, Period::Day, Adjustment::Raw)?
                    .expect("ETF raw series");
                Ok((row.version_id, row.record_count, row.last_day))
            })
            .unwrap()
    }
    fn read_raw(&self, exchange: &str, code: &str) -> Vec<Row> {
        let query: Query = serde_json::from_value(json!({
            "dataset": "market.daily",
            "contract_version": "v1",
            "selection": {"kind": "single", "entity": {"exchange": exchange, "code": code}},
            "filter": {"kind": "daily", "start_date": "2000-01-01", "end_date": "2100-01-01"},
            "version": {"kind": "latest_per_entity"},
            "adjustment": "raw",
            "page_size": 100
        }))
        .unwrap();
        Queries::new(SqliteSupply {
            market: self.backend.clone(),
        })
        .query(query, &Access::local())
        .unwrap()
        .records
    }
}

fn outcome<'a>(task: &'a TaskSnapshot, symbol: &str) -> &'a crate::task_control::TaskItem {
    task.items
        .iter()
        .find(|item| item.spec.target["symbol"].as_str() == Some(symbol))
        .unwrap_or_else(|| panic!("item {symbol} missing from the task"))
}
fn succeeded(task: &TaskSnapshot, symbol: &str) -> bool {
    outcome(task, symbol)
        .outcome
        .as_ref()
        .is_some_and(|o| o.success)
}
fn error_of(task: &TaskSnapshot, symbol: &str) -> String {
    outcome(task, symbol)
        .outcome
        .as_ref()
        .and_then(|o| o.error.clone())
        .unwrap_or_default()
}
fn close_of(row: &Row) -> String {
    match row {
        Row::Market { close, .. } => close.clone(),
        other => panic!("expected a market row, got {other:?}"),
    }
}
fn volume_of(row: &Row) -> Option<String> {
    match row {
        Row::Market { volume, .. } => volume.clone(),
        other => panic!("expected a market row, got {other:?}"),
    }
}

#[test]
fn etf_scope_plan_is_the_current_members_with_a_series_and_a_share_plan_is_unchanged() {
    let env = setup();
    env.advance_generation();
    let a_only = env.plan("market.daily", None);
    assert!(a_only.get("scope").is_none(), "no scope key without scope");
    assert_eq!(a_only["counts"]["eligible"], json!(2), "{a_only}");

    let etf = env.plan("market.daily", Some("sh_sz_etf_current"));
    assert_eq!(etf["scope"], json!("sh_sz_etf_current"));
    // 510050/510300/159919/159901/159338/159922 are current with a series;
    // 510070 is a delisted ETF (not current) and 589001 has no series.
    assert_eq!(etf["counts"]["eligible"], json!(6), "{etf}");
    assert_ne!(etf["plan_id"], a_only["plan_id"]);

    let combined = env.plan("market.daily", Some("sh_sz_a_and_etf_current"));
    assert_eq!(combined["counts"]["eligible"], json!(8), "{combined}");
    assert_ne!(combined["plan_id"], a_only["plan_id"]);
    assert_ne!(combined["plan_id"], etf["plan_id"]);

    // Same inputs, same plan_id.
    assert_eq!(
        env.plan("market.daily", Some("sh_sz_etf_current"))["plan_id"],
        etf["plan_id"]
    );
    let today = crate::calendar::shanghai_today();
    let resolved = resolve_plan(
        &env.backend,
        Some(&env.generation),
        Dataset::Daily,
        Some(PlanScope::ShSzEtfCurrent),
        Some(today),
        today,
    )
    .unwrap();
    let symbols: Vec<&str> = resolved
        .items
        .iter()
        .map(|item| match item {
            PlanItem::Daily(i) => {
                assert_eq!(i.series, Some(PlanSeries::EtfRaw));
                i.entity.as_str()
            }
            PlanItem::Yearly(_) => panic!("daily plan carries a yearly item"),
        })
        .collect();
    assert_eq!(
        symbols,
        vec![
            "sh.510050",
            "sh.510300",
            "sz.159338",
            "sz.159901",
            "sz.159919",
            "sz.159922"
        ]
    );
    assert!(
        !symbols.contains(&"sh.510070"),
        "delisted ETF stays out of the daily update"
    );
}

#[test]
fn yearly_plan_never_contains_an_etf_and_rejects_the_scope() {
    let env = setup();
    let today = crate::calendar::shanghai_today();
    let resolved = resolve_plan(
        &env.backend,
        Some(&env.generation),
        Dataset::Yearly,
        None,
        Some(today),
        today,
    )
    .unwrap();
    let is_etf = |entity: &str| {
        let code = entity.rsplit('.').next().unwrap();
        code.starts_with("51") || code.starts_with("159") || code.starts_with("589")
    };
    for item in &resolved.items {
        let entity = match item {
            PlanItem::Yearly(i) => &i.entity,
            PlanItem::Daily(i) => &i.entity,
        };
        assert!(!is_etf(entity), "yearly plan item {entity}");
    }
    for ineligible in &resolved.ineligible {
        assert!(!is_etf(&ineligible.entity), "{}", ineligible.entity);
    }
    // The yearly request has no scope field at all.
    assert!(
        env.api
            .call(
                "plan_market_update",
                json!({"dataset": "market.yearly", "contract_version": "v1", "scope": "sh_sz_etf_current"}),
            )
            .is_err(),
        "a yearly plan must not accept an ETF scope"
    );
}

#[test]
fn etf_only_run_appends_no_changes_refreshes_and_isolates_failures() {
    let env = setup();
    let before = |entity| env.etf_head(entity);
    let heads = [
        "sh.510050",
        "sh.510300",
        "sz.159338",
        "sz.159901",
        "sz.159919",
        "sz.159922",
    ]
    .map(|entity| (entity, before(entity)));
    for (entity, (_, count, _)) in &heads {
        assert!(*count >= 2, "{entity} was collected");
    }
    env.advance_generation();

    let view = env.plan("market.daily", Some("sh_sz_etf_current"));
    let task = env.run("run-etf", &view, Some("sh_sz_etf_current"));
    assert_eq!(task.task.status, Status::PartiallyFailed, "{task:?}");
    assert_eq!(task.items.len(), 6, "ETF-only run has no derived items");
    assert!(
        task.items
            .iter()
            .all(|item| !crate::task_control::derived::is_derived(&item.spec.item_id))
    );

    // The task surface reads the ETF items' raw-only events as their results
    // (an ETF has no forward event) instead of failing the evidence match.
    let progress = env
        .api
        .call("get_task_progress", json!({"task_id": task.task.task_id}))
        .unwrap();
    assert!(progress.is_object(), "{progress}");

    // Append: a new bar, read back at 3 decimals through the raw read face.
    assert!(
        succeeded(&task, "sh.510050"),
        "{}",
        error_of(&task, "sh.510050")
    );
    let (_, count, last_day) = env.etf_head("sh.510050");
    assert_eq!(count, 4);
    assert_eq!(
        crate::compact_market::epoch_date(last_day).unwrap(),
        crate::calendar::shanghai_today()
    );
    let rows = env.read_raw("sh", "510050");
    assert_eq!(rows.len(), 4);
    assert_eq!(close_of(&rows[3]), "42.05", "3dp ETF close, not 2dp");
    assert_eq!(close_of(&rows[2]), "41.953");

    // Same bytes, same events: no_change keeps the version.
    assert!(
        succeeded(&task, "sz.159919"),
        "{}",
        error_of(&task, "sz.159919")
    );
    assert_eq!(env.etf_head("sz.159919"), heads[4].1);

    // Same `.day` bytes, one new cat1 event: metadata move, no bar, and the
    // factor facts are refreshed in the same transaction.
    assert!(
        succeeded(&task, "sz.159901"),
        "{}",
        error_of(&task, "sz.159901")
    );
    assert_eq!(env.etf_head("sz.159901"), heads[3].1);
    let events = env
        .backend
        .read(|c| derived::read_adjustment_events(c, "sz.159901"))
        .unwrap();
    assert_eq!(
        events.iter().map(|e| e.category).collect::<Vec<_>>(),
        vec![11, 11, 1],
        "{events:?}"
    );

    // Hand-recorded bar: ×100 shares and volume_quantum=100.
    assert!(
        succeeded(&task, "sz.159338"),
        "{}",
        error_of(&task, "sz.159338")
    );
    let hand = env.read_raw("sz", "159338");
    assert_eq!(hand.len(), 3);
    assert_eq!(volume_of(&hand[2]).as_deref(), Some("4293795000"));
    let quantum: Option<i64> = env
        .backend
        .read(|c| {
            let row = series::read_series_adjusted(c, "sz.159338", Period::Day, Adjustment::Raw)?
                .unwrap();
            Ok(c.query_row(
                "SELECT volume_quantum FROM series_bars WHERE series_key=?1 \
                 ORDER BY trade_day DESC LIMIT 1",
                rusqlite::params![row.series_key],
                |r| r.get(0),
            )
            .unwrap())
        })
        .unwrap();
    assert_eq!(quantum, Some(100));

    // Non-prefix rewrite: item failure naming the first differing day; the
    // stored series is untouched.
    assert!(!succeeded(&task, "sh.510300"));
    let rewrite = error_of(&task, "sh.510300");
    assert!(rewrite.contains("etf_history_rewrite"), "{rewrite}");
    assert!(
        rewrite.contains(&format!(
            "first_different_day={}",
            crate::calendar::shanghai_today() - chrono::Days::new(3)
        )),
        "{rewrite}"
    );
    assert_eq!(env.etf_head("sh.510300"), heads[1].1);

    // Contradictory hand marker: the item fails and writes nothing.
    assert!(!succeeded(&task, "sz.159922"));
    let contradiction = error_of(&task, "sz.159922");
    assert!(contradiction.contains("contradictory"), "{contradiction}");
    assert_eq!(env.etf_head("sz.159922"), heads[5].1);

    // The delisted ETF never entered the run and keeps its series.
    assert!(
        task.items
            .iter()
            .all(|item| item.spec.target["symbol"] != json!("sh.510070"))
    );
}

#[test]
fn combined_run_keeps_a_share_items_independent_of_etf_failures() {
    let env = setup();
    env.advance_generation();
    let view = env.plan("market.daily", Some("sh_sz_a_and_etf_current"));
    assert_eq!(view["counts"]["eligible"], json!(8), "{view}");
    let task = env.run("run-combined", &view, Some("sh_sz_a_and_etf_current"));
    assert_eq!(task.task.status, Status::PartiallyFailed, "{task:?}");
    for symbol in ["sh.600101", "sh.600102"] {
        assert!(
            succeeded(&task, symbol),
            "{symbol}: {}",
            error_of(&task, symbol)
        );
    }
    for symbol in ["sh.510300", "sz.159922"] {
        assert!(!succeeded(&task, symbol), "{symbol}");
    }
    for symbol in ["sh.510050", "sz.159919", "sz.159901", "sz.159338"] {
        assert!(
            succeeded(&task, symbol),
            "{symbol}: {}",
            error_of(&task, symbol)
        );
    }
    // A combined plan keeps the A-share derived items.
    assert!(
        task.items
            .iter()
            .any(|item| crate::task_control::derived::is_derived(&item.spec.item_id))
    );
    // A-share heads moved to the new generation's last bar.
    let a_last = env
        .backend
        .read(|c| {
            Ok(
                series::read_series_adjusted(c, "sh.600101", Period::Day, Adjustment::Forward)?
                    .unwrap()
                    .last_day,
            )
        })
        .unwrap();
    assert_eq!(
        crate::compact_market::epoch_date(a_last).unwrap(),
        crate::calendar::shanghai_today()
    );
    // Without the scope, the A-share plan afterwards is current and ETF-free.
    let after = env.plan("market.daily", None);
    assert_eq!(after["counts"]["eligible"], json!(0), "{after}");
}

#[test]
fn run_binds_the_scope_through_the_plan_id() {
    let env = setup();
    env.advance_generation();
    let view = env.plan("market.daily", Some("sh_sz_etf_current"));
    // The same plan_id with the scope dropped re-resolves to the A-share plan.
    let error = env
        .api
        .call(
            "run_market_update",
            json!({
                "dataset": "market.daily",
                "contract_version": "v1",
                "request_id": "run-wrong-scope",
                "target_day": view["target_day"],
                "plan_id": view["plan_id"],
            }),
        )
        .unwrap_err();
    assert_eq!(error.code, "plan_stale", "{error:?}");
}

#[test]
fn delisted_series_check_does_not_register_a_delisted_etf() {
    // M3 design P17 / D-70: the delisting check is an A-share fact. The ETF
    // `sh.510070` has a delisting date and a 不复权 series in this database
    // and must still stay out of the registrations and the skipped evidence.
    let env = setup();
    let (_, count, _) = env.etf_head("sh.510070");
    assert_eq!(count, 3, "the delisted ETF owns a series");
    let today = crate::calendar::shanghai_today();
    let resolution = env
        .backend
        .read(|c| crate::data_supply::delisted_series::resolve(c, today))
        .unwrap();
    let registered: Vec<&str> = resolution
        .baseline
        .registrations
        .iter()
        .map(|r| r.entity.as_str())
        .collect();
    assert!(registered.contains(&"sh.600102"), "{registered:?}");
    assert!(
        registered
            .iter()
            .all(|entity| !entity.starts_with("sh.51") && !entity.starts_with("sz.159")),
        "{registered:?}"
    );
    assert!(
        !resolution
            .skipped
            .iter()
            .any(|value| value.to_string().contains("510070")),
        "{:?}",
        resolution.skipped
    );
}
