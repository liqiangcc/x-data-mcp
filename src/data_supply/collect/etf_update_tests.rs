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
        day_record(day_u32(3), [1140, 1160, 1130, 1150], 0.0, 1_000),
        day_record(day_u32(2), [1150, 1160, 1140, 1155], 0.0, 1_000),
    ]
}
/// `sz159338` anchor: 42,937,950 hands ×100 = 4,293,795,000 shares, amount
/// 4,937,864,250 yuan = 1.15 yuan per share, inside the bar's raw/1000 range.
///
/// Dated yesterday on purpose: once stored it is part of the next day's base
/// window, which is the shape every later daily run sees.
fn hand_bar() -> [u8; 32] {
    day_record_r7(
        day_u32(1),
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

/// Today's ordinary bar of `sz159338`, appended after its stored hand bar.
fn hand_ordinary_today() -> [u8; 32] {
    day_record(day_u32(0), [1158, 1165, 1150, 1160], 0.0, 5_000)
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Generation {
    First,
    Second,
    /// Second plus one unrelated filler member, so the generation is new while
    /// every ETF member keeps the bytes it had in the second generation.
    Third,
    /// Third plus an ordinary bar behind the stored hand bar of `sz159338`.
    Fourth,
    /// Fourth plus a factor change whose 除权除息日 is **after** every stored
    /// bar (`sz159919`, #607 PR-C), plus one more 不复权 bar for an ETF whose
    /// factors do not move (`sh510050`). This is the only generation where the
    /// derived 前复权 series must be rewritten rather than merely extended.
    Fifth,
    /// Fifth plus a degenerate `c3 = 0` share conversion for `sz159919`: the
    /// factor fact itself is storable, so only the **derived** 前复权 build
    /// fails. The item must roll back as a whole (#607 PR-C requirement 2).
    Sixth,
}

/// One mirror generation. Second-generation deltas, per ETF:
///
/// | ETF | first generation | second generation | expected update |
/// |---|---|---|---|
/// | `sh510050` | 3 bars | + today | append |
/// | `sz159919` | 3 bars | identical | no_change |
/// | `sz159901` | 3 bars | identical bytes, one new cat1 event | metadata |
/// | `sz159338` | 2 bars | + a valid hand-recorded bar | append, ×100 |
///
/// Third generation: every ETF member is byte-identical to the second one
/// (`sz159338` now carries its stored hand bar in the base window). Fourth
/// generation: `sz159338` gains an ordinary bar behind the hand bar.
/// | `sh510300` | 3 bars | first bar rewritten + today | `etf_history_rewrite` |
/// | `sz159922` | 3 bars | + a hand-marked bar whose amount disagrees | item failure |
/// Tomorrow as a raw `yyyymmdd` int — an 除权除息日 strictly after every bar
/// the fixtures store (`day_u32(0)` is today).
fn tomorrow_raw() -> u32 {
    (crate::calendar::shanghai_today() + chrono::Days::new(1))
        .format("%Y%m%d")
        .to_string()
        .parse()
        .unwrap()
}

fn write_mirror(dir: &Path, generation: Generation) {
    std::fs::create_dir_all(dir).unwrap();
    let second = generation >= Generation::Second;
    let third = generation >= Generation::Third;
    let fourth = generation >= Generation::Fourth;
    let fifth = generation >= Generation::Fifth;
    let sixth = generation >= Generation::Sixth;
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
        // cat9 (股本变动) is a capital fact, not a factor fact: it reaches
        // `market.capital_events` and stays out of `market.adjustment_events`.
        gbbq_factor_record(0, "159919", 20240601, 9, [100.0, 100.0, 120.0, 120.0]),
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
    if sixth {
        // c3 = 0: `etf_event_transform` refuses the division, but the factor
        // row is a well-formed f32 fact and stores fine. The failure therefore
        // lands in the 前复权 derivation, after the 不复权 half has written.
        records.push(gbbq_factor_record(
            0,
            "159919",
            tomorrow_raw(),
            11,
            [0.0, 0.0, 0.0, 0.0],
        ));
    }
    if fifth {
        // A dividend whose 除权除息日 is tomorrow: every stored 不复权 bar is
        // strictly before it, so the derived 前复权 series must be rewritten,
        // not extended (#607 PR-C requirement 2).
        records.push(gbbq_factor_record(
            0,
            "159919",
            tomorrow_raw(),
            1,
            [0.661, 0.0, 0.0, 0.0],
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
            bars.push(hand_bar());
        }
        if fourth {
            bars.push(hand_ordinary_today());
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
    let filler_changed = day_record(day_u32(0), [1128, 1130, 1125, 1129], 0.0, 101).to_vec();
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
    for (index, name) in fillers.iter().enumerate() {
        let last = index + 1 == fillers.len();
        members.push((
            name.as_str(),
            if third && last {
                filler_changed.as_slice()
            } else {
                a_short.as_slice()
            },
        ));
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
    fn advance_generation(&self) -> String {
        self.advance_to(Generation::Second, "req-second", &self.gen_first)
    }
    fn advance_to(&self, generation: Generation, request: &str, previous: &str) -> String {
        write_mirror(self.mirror.path(), generation);
        let next = install(&self.service, request, Some(previous));
        assert_ne!(next, previous);
        next
    }
    /// `volume_quantum` of every stored bar of an ETF raw series, oldest first.
    fn quanta(&self, entity: &str) -> Vec<Option<i64>> {
        self.backend
            .read(|c| {
                let row = series::read_series_adjusted(c, entity, Period::Day, Adjustment::Raw)?
                    .expect("ETF raw series");
                let mut statement = c
                    .prepare(
                        "SELECT volume_quantum FROM series_bars WHERE series_key=?1 \
                         ORDER BY trade_day",
                    )
                    .unwrap();
                let quanta = statement
                    .query_map(rusqlite::params![row.series_key], |r| r.get(0))
                    .unwrap()
                    .collect::<Result<Vec<Option<i64>>, _>>()
                    .unwrap();
                Ok(quanta)
            })
            .unwrap()
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

#[test]
fn stored_hand_bar_in_the_base_window_is_no_change_then_append_never_a_rewrite() {
    // Production's second daily run: `sz159338` already stores its
    // hand-recorded bar (×100, volume_quantum=100) and the next generation
    // either carries the same bytes or adds an ordinary bar. The continuity
    // comparison against the stored base window must neither call that a
    // rewrite nor disturb the stored hand bar.
    let env = setup();
    let second = env.advance_generation();
    let view = env.plan("market.daily", Some("sh_sz_etf_current"));
    let task = env.run("run-etf-g2", &view, Some("sh_sz_etf_current"));
    assert!(
        succeeded(&task, "sz.159338"),
        "{}",
        error_of(&task, "sz.159338")
    );
    let stored = env.etf_head("sz.159338");
    assert_eq!(stored.1, 3, "two collected bars plus the appended hand bar");
    assert_eq!(env.quanta("sz.159338")[2], Some(100));

    // (a) Same bytes: the stored hand bar sits in the base window.
    let third = env.advance_to(Generation::Third, "req-third", &second);
    let view = env.plan("market.daily", Some("sh_sz_etf_current"));
    let items = view["counts"]["eligible"].as_i64().unwrap();
    assert!(items >= 1, "sz.159338 trails the target day: {view}");
    let task = env.run("run-etf-g3", &view, Some("sh_sz_etf_current"));
    assert!(
        succeeded(&task, "sz.159338"),
        "{}",
        error_of(&task, "sz.159338")
    );
    assert_eq!(
        env.etf_head("sz.159338"),
        stored,
        "identical bytes keep the version (no_change)"
    );
    let hand = env.read_raw("sz", "159338");
    assert_eq!(hand.len(), 3);
    assert_eq!(volume_of(&hand[2]).as_deref(), Some("4293795000"));
    assert_eq!(env.quanta("sz.159338")[2], Some(100));

    // (b) An ordinary bar behind the stored hand bar appends.
    env.advance_to(Generation::Fourth, "req-fourth", &third);
    let view = env.plan("market.daily", Some("sh_sz_etf_current"));
    let task = env.run("run-etf-g4", &view, Some("sh_sz_etf_current"));
    assert!(
        succeeded(&task, "sz.159338"),
        "{}",
        error_of(&task, "sz.159338")
    );
    let (version, count, last_day) = env.etf_head("sz.159338");
    assert_ne!(version, stored.0, "an appended bar moves the version");
    assert_eq!(count, 4);
    assert_eq!(
        crate::compact_market::epoch_date(last_day).unwrap(),
        crate::calendar::shanghai_today()
    );
    let rows = env.read_raw("sz", "159338");
    assert_eq!(rows.len(), 4);
    assert_eq!(
        volume_of(&rows[2]).as_deref(),
        Some("4293795000"),
        "the stored hand bar keeps its ×100 volume"
    );
    assert_eq!(close_of(&rows[3]), "1.16");
    assert_eq!(
        env.quanta("sz.159338")[2],
        Some(100),
        "the stored hand bar keeps volume_quantum=100"
    );
}

const FULL_RANGE: (&str, &str) = ("2000-01-01", "2100-01-01");

fn fact_query(dataset: &str, selection: Value, range: (&str, &str), version: &str) -> Value {
    json!({
        "dataset": dataset,
        "contract_version": version,
        "selection": selection,
        "filter": {"kind": "daily", "start_date": range.0, "end_date": range.1},
        "version": {"kind": "latest_per_entity"},
    })
}

fn etf(exchange: &str, code: &str) -> Value {
    json!({"kind": "single", "entity": {"exchange": exchange, "code": code}})
}

impl Env {
    /// `query_data` v2 over the local owner's tool surface.
    fn facts(&self, dataset: &str, selection: Value, range: (&str, &str)) -> Value {
        self.api
            .call("query_data", fact_query(dataset, selection, range, "v2"))
            .unwrap()
    }
    fn version_of(page: &Value, entity: &str) -> Value {
        page["versions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["entity"] == json!(entity))
            .unwrap_or_else(|| panic!("no versions row for {entity}: {page}"))
            .clone()
    }
}

/// T14 / AC-M3-2-6: the ETF cat1 and cat11 factor facts written by the ETF
/// collection path are readable through `query_data` `market.adjustment_events`.
///
/// Pinned read behavior: v2 only; `single`/`batch` selection; rows carry
/// `effective_day`, `category` and `c1..c4` but no `basis_status`; rows follow the source (gbbq decode) order; an ETF without factor events is
/// `absent`.
#[test]
fn etf_cat1_and_cat11_factor_facts_are_readable_through_adjustment_events() {
    let env = setup();

    // cat1: 159919 (0.661 dividend). cat11: 159901 (2:1 and 5:1, source order).
    let cat1 = env.facts("market.adjustment_events", etf("sz", "159919"), FULL_RANGE);
    assert_eq!(cat1["layout"], json!("table"));
    assert_eq!(cat1["entities"], json!(["sz.159919"]));
    assert_eq!(cat1["columns"]["effective_day"], json!(["2025-03-31"]));
    assert_eq!(cat1["columns"]["category"], json!([1]));
    assert_eq!(
        cat1["columns"]["c1"],
        json!(["0.661"]),
        "f32 shortest, not 2dp"
    );
    assert_eq!(cat1["columns"]["c3"], json!(["0"]));
    let row = Env::version_of(&cat1, "sz.159919");
    assert_eq!(row["status"], json!("present"));
    assert!(row["version_id"].as_str().is_some_and(|v| v.len() == 64));
    assert_eq!(
        cat1["metadata"]["source"],
        json!("derived:adjustment_events")
    );
    assert_eq!(cat1["metadata"]["coverage_status"], json!("complete"));

    let cat11 = env.facts("market.adjustment_events", etf("sz", "159901"), FULL_RANGE);
    assert_eq!(cat11["entities"], json!(["sz.159901", "sz.159901"]));
    assert_eq!(cat11["columns"]["category"], json!([11, 11]));
    assert_eq!(
        cat11["columns"]["effective_day"],
        json!(["2021-04-12", "2010-11-22"]),
        "the date tells the two events apart"
    );
    assert_eq!(
        cat11["columns"]["c3"],
        json!(["2", "5"]),
        "source order (2021 then 2010), not date order"
    );
    assert_eq!(
        Env::version_of(&cat11, "sz.159901")["status"],
        json!("present")
    );

    // The wire carries the date but no basis column and no known_at.
    for page in [&cat1, &cat11] {
        let columns = page["columns"].as_object().unwrap();
        assert!(columns.contains_key("effective_day"), "{page}");
        assert!(columns.get("basis_status").is_none(), "{page}");
        assert!(columns.get("known_at").is_none(), "{page}");
    }
    let only_2021 = env.facts(
        "market.adjustment_events",
        etf("sz", "159901"),
        ("2021-01-01", "2021-12-31"),
    );
    assert_eq!(only_2021["columns"]["c3"], json!(["2"]));
    assert_eq!(only_2021["columns"]["effective_day"], json!(["2021-04-12"]));
    let none_in_range = env.facts(
        "market.adjustment_events",
        etf("sz", "159901"),
        ("2022-01-01", "2022-12-31"),
    );
    assert_eq!(none_in_range["row_count"], json!(0));
    assert_eq!(
        Env::version_of(&none_in_range, "sz.159901")["status"],
        json!("present"),
        "a present version with no rows in range is not absent"
    );

    // A batch mixes present and absent members: 510050 has no factor events
    // (no registration), 589001 has no series at all. Both are `absent`.
    let batch = env.facts(
        "market.adjustment_events",
        json!({"kind": "batch", "entities": [
            {"exchange": "sz", "code": "159919"},
            {"exchange": "sz", "code": "159901"},
            {"exchange": "sh", "code": "510050"},
            {"exchange": "sh", "code": "589001"},
        ]}),
        FULL_RANGE,
    );
    assert_eq!(batch["row_count"], json!(3));
    for (entity, status) in [
        ("sz.159919", "present"),
        ("sz.159901", "present"),
        ("sh.510050", "absent"),
        ("sh.589001", "absent"),
    ] {
        assert_eq!(Env::version_of(&batch, entity)["status"], json!(status));
    }

    // v1 is not a contract for this dataset.
    let v1 = env
        .api
        .call(
            "query_data",
            fact_query(
                "market.adjustment_events",
                etf("sz", "159919"),
                FULL_RANGE,
                "v1",
            ),
        )
        .unwrap_err();
    assert_eq!(v1.code, "invalid_arguments");
    assert!(
        v1.message.contains("requires contract_version v2"),
        "{}",
        v1.message
    );

    // The version list names the same version the query read.
    let versions = env
        .api
        .call(
            "list_data_versions",
            json!({
                "dataset": "market.adjustment_events",
                "contract_version": "v2",
                "selection": etf("sz", "159919"),
            }),
        )
        .unwrap();
    assert_eq!(versions["row_count"], json!(1), "{versions}");
    assert_eq!(
        versions["versions"][0]["version_id"],
        Env::version_of(&cat1, "sz.159919")["version_id"]
    );
}

/// The stored `basis_status` is per row (cat1 verified, cat11 pending) but has
/// no read face; the storage accessor is the only place it is observable.
#[test]
fn etf_factor_rows_store_basis_status_per_category() {
    let env = setup();
    let basis = |entity: &str| {
        env.backend
            .read(|c| derived::read_adjustment_events(c, entity))
            .unwrap()
            .into_iter()
            .map(|e| (e.category, e.basis_status.as_str()))
            .collect::<Vec<_>>()
    };
    assert_eq!(basis("sz.159919"), vec![(1, "verified")]);
    assert_eq!(basis("sz.159901"), vec![(11, "pending"), (11, "pending")]);
}

/// A later generation's new cat1 event replaces the whole set: the read face
/// shows the new row and a new `version_id`.
#[test]
fn etf_factor_read_follows_a_metadata_only_update() {
    let env = setup();
    let before = env.facts("market.adjustment_events", etf("sz", "159901"), FULL_RANGE);
    let before_version = Env::version_of(&before, "sz.159901")["version_id"].clone();
    env.advance_generation();
    let view = env.plan("market.daily", Some("sh_sz_etf_current"));
    let task = env.run("run-etf-facts", &view, Some("sh_sz_etf_current"));
    assert!(
        succeeded(&task, "sz.159901"),
        "{}",
        error_of(&task, "sz.159901")
    );

    let after = env.facts("market.adjustment_events", etf("sz", "159901"), FULL_RANGE);
    assert_eq!(after["columns"]["category"], json!([11, 11, 1]));
    assert_eq!(after["columns"]["c1"], json!(["0", "0", "0.1"]));
    assert_ne!(
        Env::version_of(&after, "sz.159901")["version_id"],
        before_version
    );
    // The untouched cat1 member keeps its version.
    let untouched = env.facts("market.adjustment_events", etf("sz", "159919"), FULL_RANGE);
    assert_eq!(untouched["columns"]["c1"], json!(["0.661"]));
}

/// `selection.kind=universe` has no ETF scope for `market.adjustment_events`
/// (the ETF scopes are `market.daily` only), and the access gate applies to
/// ETF entities like any other.
#[test]
fn etf_factor_reads_have_no_universe_scope_and_respect_access() {
    let env = setup();
    let scope = env
        .api
        .call(
            "query_data",
            fact_query(
                "market.adjustment_events",
                json!({"kind": "universe", "scope": "sh_sz_etf_current"}),
                FULL_RANGE,
                "v2",
            ),
        )
        .unwrap_err();
    assert_eq!(scope.code, "invalid_arguments");
    assert!(
        scope.message.contains("only supported for market.daily"),
        "{}",
        scope.message
    );

    let query = |access: &Access| {
        let query: Query = serde_json::from_value(fact_query(
            "market.adjustment_events",
            etf("sz", "159919"),
            FULL_RANGE,
            "v2",
        ))
        .unwrap();
        Queries::new(SqliteSupply {
            market: env.backend.clone(),
        })
        .query_v2(query, access)
    };
    assert!(query(&Access::local()).is_ok());
    let mut no_dataset = Access::local();
    no_dataset.datasets.remove(&Dataset::AdjustmentEvents);
    assert!(
        query(&no_dataset)
            .unwrap_err()
            .to_string()
            .contains("forbidden")
    );
    let mut other_securities = Access::local();
    other_securities.securities =
        Some([crate::data_supply::wire::Security::parse("sz.159901").unwrap()].into());
    assert!(
        query(&other_securities)
            .unwrap_err()
            .to_string()
            .contains("forbidden")
    );
}

/// `market.capital_events` for an ETF: only the capital categories are stored,
/// so cat1/cat11 never appear there. An ETF with a cat9 record reads it; an
/// ETF with only cat1/cat11 records is `absent` (nothing registered).
#[test]
fn etf_capital_events_read_only_capital_categories() {
    let env = setup();
    let page = env.facts("market.capital_events", etf("sz", "159919"), FULL_RANGE);
    assert_eq!(page["entities"], json!(["sz.159919"]), "{page}");
    assert_eq!(page["columns"]["category"], json!([9]));
    assert_eq!(page["columns"]["suspicious"], json!([false]));
    assert_eq!(page["row_count"], json!(1));
    assert_eq!(
        Env::version_of(&page, "sz.159919")["status"],
        json!("present")
    );
    assert_eq!(page["metadata"]["source"], json!("derived:capital_events"));

    let factor_only = env.facts("market.capital_events", etf("sz", "159901"), FULL_RANGE);
    assert_eq!(factor_only["row_count"], json!(0));
    assert_eq!(
        Env::version_of(&factor_only, "sz.159901")["status"],
        json!("absent")
    );
    assert!(
        factor_only["limitations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l
                .as_str()
                .is_some_and(|l| l.contains("no registered version"))),
        "{factor_only}"
    );
}

// ---------------------------------------------------------------------------
// #607 PR-C — the derived 前复权 series
// ---------------------------------------------------------------------------

impl Env {
    /// Head facts of an entity's daily series at one 口径, if it exists.
    fn forward_head(&self, entity: &str, adjustment: Adjustment) -> Option<(String, i64, i64)> {
        self.backend
            .read(|c| {
                Ok(
                    series::read_series_adjusted(c, entity, Period::Day, adjustment)?
                        .map(|row| (row.version_id, row.record_count, row.last_day)),
                )
            })
            .unwrap()
    }
    /// Every stored bar of a series at one 口径, oldest first, as
    /// `(date, open, high, low, close)` price units.
    fn bars(&self, entity: &str, adjustment: Adjustment) -> Vec<(i64, i64, i64, i64, i64)> {
        self.backend
            .read(|c| {
                let row = series::read_series_adjusted(c, entity, Period::Day, adjustment)?
                    .expect("series row");
                Ok(series::all_bars(c, row.series_key)?
                    .into_iter()
                    .map(|bar| {
                        (
                            bar.trade_day,
                            bar.open_units,
                            bar.high_units,
                            bar.low_units,
                            bar.close_units,
                        )
                    })
                    .collect())
            })
            .unwrap()
    }
    /// Number of `market_events` rows of one entity at one 口径 — used to prove
    /// an append happened without a rewrite.
    fn event_kinds(&self, entity: &str, adjustment: Adjustment) -> Vec<(&'static str, i64)> {
        self.backend
            .read(|c| {
                let mut statement = c
                    .prepare(
                        "SELECT kind, count(*) FROM market_events \
                         WHERE entity=?1 AND period=?2 AND adjustment=?3 GROUP BY kind \
                         ORDER BY kind",
                    )
                    .unwrap();
                let rows = statement
                    .query_map(rusqlite::params![entity, 1i64, adjustment.code()], |r| {
                        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
                    })
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                Ok(rows
                    .into_iter()
                    .map(|(kind, count)| (Box::leak(kind.into_boxed_str()) as &'static str, count))
                    .collect())
            })
            .unwrap()
    }
}

#[test]
fn etf_forward_series_is_built_inside_the_first_collection_transaction() {
    // First backfill: the 不复权 collection also creates the 前复权 series,
    // with the same bar count and the shape design §4.1 names.
    let env = setup();
    let raw = env
        .forward_head("sz.159919", Adjustment::Raw)
        .expect("raw series");
    let adj = env
        .forward_head("sz.159919", Adjustment::Forward)
        .expect("forward series");
    assert_eq!(raw.1, adj.1, "forward row count mirrors raw");
    assert_eq!(raw.2, adj.2, "forward last_day mirrors raw");
    let shape = env
        .backend
        .read(|c| {
            let row =
                series::read_series_adjusted(c, "sz.159919", Period::Day, Adjustment::Forward)?
                    .expect("forward series");
            Ok((row.instrument_class, row.adjustment.code(), row.price_scale))
        })
        .unwrap();
    // `instrument_class=2` (ETF), `adjustment=1` (`Adjustment::Forward`'s
    // stored code — design §4.1 draws it as "2", the code's value is 1),
    // `price_scale=6` (1e-6 元).
    assert_eq!(
        shape,
        (
            crate::market_data::INSTRUMENT_CLASS_ETF,
            Adjustment::Forward.code(),
            6
        )
    );
}

#[test]
fn etf_forward_append_only_writes_the_new_tail() {
    // `sh510050` gains one 不复权 bar and its factors do not move: the derived
    // 前复权 series must be extended, never rewritten — the stored prefix
    // keeps its bytes and its version chain records an `append`.
    let env = setup();
    let prefix = env.bars("sh.510050", Adjustment::Forward);
    let prefix_version = env.forward_head("sh.510050", Adjustment::Forward);
    assert_eq!(prefix.len(), 3);
    env.advance_generation();

    let view = env.plan("market.daily", Some("sh_sz_etf_current"));
    let task = env.run("run-etf-append", &view, Some("sh_sz_etf_current"));
    assert!(
        succeeded(&task, "sh.510050"),
        "{}",
        error_of(&task, "sh.510050")
    );

    let after = env.bars("sh.510050", Adjustment::Forward);
    assert_eq!(after.len(), 4, "the new 不复权 bar is mirrored");
    assert_eq!(&after[..3], &prefix[..], "the stored prefix is untouched");
    // The 前复权 tail is the 不复权 tail with the price fields rescaled only:
    // no event applies, so 前复权 == 不复权 × 10^3.
    let raw = env.bars("sh.510050", Adjustment::Raw);
    assert_eq!(after[3].4, raw[3].4 * 1_000);
    // An append, not a rewrite: the only event for this 口径 is an append.
    let kinds = env.event_kinds("sh.510050", Adjustment::Forward);
    assert!(
        kinds.iter().any(|(kind, _)| *kind == "append"),
        "forward events: {kinds:?}"
    );
    assert!(
        kinds.iter().all(|(kind, _)| *kind != "replace"),
        "forward events: {kinds:?}"
    );
    assert_ne!(
        env.forward_head("sh.510050", Adjustment::Forward),
        prefix_version
    );
}

#[test]
fn etf_forward_factor_change_rewrites_the_whole_series() {
    // `sz159919` gains a dividend whose 除权除息日 is after every stored bar:
    // every bar is adjusted, so the whole derived series is rewritten.
    let env = setup();
    let second = env.advance_generation();
    // Third generation: identical bytes and events, so the series is stable.
    let third = env.advance_to(Generation::Third, "req-third", &second);
    let before = env.bars("sz.159919", Adjustment::Forward);

    env.advance_to(Generation::Fifth, "req-fifth", &third);
    let view = env.plan("market.daily", Some("sh_sz_etf_current"));
    let task = env.run("run-etf-factor", &view, Some("sh_sz_etf_current"));
    assert!(
        succeeded(&task, "sz.159919"),
        "{}",
        error_of(&task, "sz.159919")
    );

    let after = env.bars("sz.159919", Adjustment::Forward);
    assert_eq!(after.len(), before.len(), "no new bar, same bar count");
    assert_ne!(after, before, "every earlier bar must move");
    // 41.780 元 − 0.661/10 = 41.7139 元 → 41,713,900 units; the raw 厘 value
    // is 41,780, so the adjustment is observable at 6dp.
    assert_eq!(after[0].4, 41_713_900);
    let kinds = env.event_kinds("sz.159919", Adjustment::Forward);
    assert!(
        kinds.iter().any(|(kind, _)| *kind == "replace"),
        "forward events: {kinds:?}"
    );
}

#[test]
fn etf_forward_no_change_leaves_the_series_untouched() {
    // Same `.day` bytes and same factors: the derived series must not move
    // (`data_versions +0` on a no-change run).
    let env = setup();
    let before = env.forward_head("sz.159919", Adjustment::Forward);
    let second = env.advance_generation();
    env.advance_to(Generation::Third, "req-third", &second);
    let view = env.plan("market.daily", Some("sh_sz_etf_current"));
    let task = env.run("run-etf-nochange", &view, Some("sh_sz_etf_current"));
    assert!(
        succeeded(&task, "sz.159919"),
        "{}",
        error_of(&task, "sz.159919")
    );
    assert_eq!(env.forward_head("sz.159919", Adjustment::Forward), before);
}

#[test]
fn etf_forward_build_failure_rolls_the_whole_item_back() {
    // The degenerate `c3 = 0` factor is storable, so the 不复权 half of the
    // item succeeds first; the derived 前复权 build then fails. The item must
    // fail **and** leave no trace: same 不复权 head, same factor facts, no
    // 前复权 event. A half-committed state (不复权 ahead of 前复权) is exactly
    // what requirement 2 forbids.
    let env = setup();
    let second = env.advance_generation();
    let third = env.advance_to(Generation::Third, "req-third", &second);
    let fourth = env.advance_to(Generation::Fourth, "req-fourth", &third);
    let fifth = env.advance_to(Generation::Fifth, "req-fifth", &fourth);
    let raw_before = env.forward_head("sz.159919", Adjustment::Raw);
    let forward_before = env.forward_head("sz.159919", Adjustment::Forward);
    let events_before = env
        .backend
        .read(|c| crate::sqlite_market::derived::read_adjustment_events(c, "sz.159919"))
        .unwrap();

    env.advance_to(Generation::Sixth, "req-sixth", &fifth);
    let view = env.plan("market.daily", Some("sh_sz_etf_current"));
    let task = env.run("run-etf-rollback", &view, Some("sh_sz_etf_current"));
    assert!(!succeeded(&task, "sz.159919"), "the item must fail");
    assert!(
        error_of(&task, "sz.159919").contains("forward chain failed"),
        "{}",
        error_of(&task, "sz.159919")
    );

    assert_eq!(
        env.forward_head("sz.159919", Adjustment::Raw),
        raw_before,
        "不复权 must not commit without its 前复权 half"
    );
    assert_eq!(
        env.forward_head("sz.159919", Adjustment::Forward),
        forward_before,
        "前复权 head must not move"
    );
    let events_after = env
        .backend
        .read(|c| crate::sqlite_market::derived::read_adjustment_events(c, "sz.159919"))
        .unwrap();
    assert_eq!(events_after, events_before, "factor facts rolled back too");
}

#[test]
fn etf_forward_rerun_of_the_same_input_keeps_every_version() {
    // `version_id` is the `series-fold-v2` digest of the content, so replaying
    // one item over identical bars and identical factors must not mint a new
    // version (#607 PR-C requirement 4).
    let env = setup();
    let second = env.advance_generation();
    env.advance_to(Generation::Third, "req-third", &second);
    let view = env.plan("market.daily", Some("sh_sz_etf_current"));
    let first = env.run("run-etf-idem-1", &view, Some("sh_sz_etf_current"));
    assert!(
        succeeded(&first, "sz.159919"),
        "{}",
        error_of(&first, "sz.159919")
    );
    let after_first = env.forward_head("sz.159919", Adjustment::Forward);
    let raw_after_first = env.forward_head("sz.159919", Adjustment::Raw);

    // A second plan over the same generation and target day.
    let again = env.plan("market.daily", Some("sh_sz_etf_current"));
    let second_run = env.run("run-etf-idem-2", &again, Some("sh_sz_etf_current"));
    assert!(
        second_run.items.is_empty() || succeeded(&second_run, "sz.159919"),
        "{:?}",
        error_of(&second_run, "sz.159919")
    );
    assert_eq!(
        env.forward_head("sz.159919", Adjustment::Forward),
        after_first,
        "the same input must keep the same 前复权 version"
    );
    assert_eq!(
        env.forward_head("sz.159919", Adjustment::Raw),
        raw_after_first
    );
}

#[test]
fn etf_raw_series_stays_byte_identical_next_to_the_derived_one() {
    // #607 PR-C requirement 5: adding the 前复权 series must not move one byte
    // of the 不复权 series — same 3dp shape, same prices, same volume/amount,
    // and the raw read face still answers exactly what it answered before.
    let env = setup();
    let raw = env.bars("sz.159919", Adjustment::Raw);
    let shape = env
        .backend
        .read(|c| {
            let row = series::read_series_adjusted(c, "sz.159919", Period::Day, Adjustment::Raw)?
                .expect("raw series");
            Ok((row.instrument_class, row.price_scale, row.adjustment.code()))
        })
        .unwrap();
    assert_eq!(
        shape,
        (
            crate::market_data::INSTRUMENT_CLASS_ETF,
            3,
            Adjustment::Raw.code()
        )
    );
    // 41.780 / 41.86 / 41.953 元 at 厘 precision — untouched by the derivation.
    assert_eq!(
        raw.iter().map(|b| b.4).collect::<Vec<_>>(),
        vec![41_780, 41_860, 41_953]
    );
    // The 前复权 row is the derived one and differs only where an event
    // reaches: no event applies to these bars, so it is the identity chain
    // rescaled to 1e-6 元.
    let adj = env.bars("sz.159919", Adjustment::Forward);
    assert_eq!(
        adj.iter().map(|b| b.4).collect::<Vec<_>>(),
        vec![41_780_000, 41_860_000, 41_953_000]
    );
    // The read face still serves the 不复权 page unchanged.
    let rows = env.read_raw("sz", "159919");
    assert_eq!(rows.len(), 3);
    assert_eq!(close_of(&rows[0]), "41.78");
}

// ---------------------------------------------------------------------------
// #607 PR-C review r1 item 1 — the stored 前复权 series must not be visible to
// any read or maintenance surface that does not know about it.
// ---------------------------------------------------------------------------

impl Env {
    /// Delete every derived 前复权 row, reproducing the pre-PR-C store while
    /// keeping every other byte of the fixture. The series rows, their bars,
    /// their events and their evidence all go, because a real pre-PR-C store
    /// had none of them.
    fn drop_etf_forward_series(&self) {
        let MarketBackend::Compact(repo) = &self.backend else {
            panic!("fixture must open the compact backend");
        };
        repo.tx(|c| {
                let keys: Vec<i64> = c
                    .prepare(
                        "SELECT series_key FROM market_series WHERE adjustment=?1 AND instrument_class=?2",
                    )
                    .unwrap()
                    .query_map(
                        rusqlite::params![
                            Adjustment::Forward.code(),
                            crate::market_data::INSTRUMENT_CLASS_ETF
                        ],
                        |r| r.get(0),
                    )
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                for key in keys {
                    c.execute("DELETE FROM series_bars WHERE series_key=?1", [key])
                        .unwrap();
                    c.execute("DELETE FROM market_series WHERE series_key=?1", [key])
                        .unwrap();
                }
                c.execute(
                    "DELETE FROM market_events WHERE period=1 AND adjustment=?1 AND entity IN \
                     (SELECT entity FROM security_master WHERE instrument_class=?2)",
                    rusqlite::params![
                        Adjustment::Forward.code(),
                        crate::market_data::INSTRUMENT_CLASS_ETF
                    ],
                )
                .unwrap();
                Ok::<(), crate::market_storage::Error>(())
            })
            .unwrap();
    }

    /// Every read/maintenance surface that accepts an ETF, rendered to JSON so
    /// two stores can be compared byte for byte.
    fn etf_surface_snapshot(&self, task_id: &str) -> Vec<(&'static str, serde_json::Value)> {
        let daily = |code: &str| {
            self.api
                .call(
                    "query_data",
                    json!({
                        "dataset": "market.daily",
                        "contract_version": "v1",
                        "selection": {"kind": "single", "entity": {"exchange": "sz", "code": code}},
                        "filter": {"kind": "daily", "start_date": "2000-01-01", "end_date": "2100-01-01"},
                        "version": {"kind": "latest_per_entity"},
                        "adjustment": "raw",
                        "page_size": 100
                    }),
                )
                .unwrap()
        };
        let versions = |code: &str| {
            self.api
                .call(
                    "list_data_versions",
                    json!({
                        "dataset": "market.daily",
                        "contract_version": "v1",
                        "selection": {"kind": "single", "entity": {"exchange": "sz", "code": code}},
                        "adjustment": "raw",
                    }),
                )
                .unwrap()
        };
        let availability = |code: &str| {
            self.api
                .call(
                    "get_data_availability",
                    json!({
                        "dataset": "market.daily",
                        "contract_version": "v1",
                        "selection": {"kind": "single", "entity": {"exchange": "sz", "code": code}},
                        "filter": {"kind": "daily", "start_date": "2000-01-01", "end_date": "2100-01-01"},
                        "adjustment": "raw",
                    }),
                )
                .unwrap()
        };
        vec![
            ("market.daily raw 159919", daily("159919")),
            ("market.daily raw 159922", daily("159922")),
            ("versions 159919", versions("159919")),
            ("versions 159922", versions("159922")),
            ("availability 159919", availability("159919")),
            ("availability 159922", availability("159922")),
            (
                "get_task_progress",
                self.api
                    .call("get_task_progress", json!({"task_id": task_id}))
                    .unwrap(),
            ),
            (
                "get_task",
                self.api
                    .call("get_task", json!({"task_id": task_id}))
                    .unwrap(),
            ),
        ]
    }
}

#[test]
fn a_stored_etf_forward_series_changes_no_read_or_maintenance_surface() {
    // The fixture runs once with the derived 前复权 series present and once
    // with it deleted. Every surface an ETF can reach must render identically:
    // PR-C added storage, not delivery.
    let with_series = setup();
    let second = with_series.advance_generation();
    let third = with_series.advance_to(Generation::Third, "req-third", &second);
    with_series.advance_to(Generation::Fifth, "req-fifth", &third);
    let view = with_series.plan("market.daily", Some("sh_sz_etf_current"));
    let task = with_series.run("run-surface", &view, Some("sh_sz_etf_current"));
    let present = with_series.etf_surface_snapshot(&task.task.task_id);
    // The derived rows really are there, otherwise this test proves nothing.
    for entity in ["sz.159919", "sz.159922"] {
        assert!(
            with_series
                .forward_head(entity, Adjustment::Forward)
                .is_some(),
            "{entity} must have a derived 前复权 series in this arm"
        );
    }

    // Same store, derived rows removed: the pre-PR-C shape.
    with_series.drop_etf_forward_series();
    for entity in ["sz.159919", "sz.159922"] {
        assert!(
            with_series
                .forward_head(entity, Adjustment::Forward)
                .is_none(),
            "{entity} 前复权 series must be gone in this arm"
        );
    }
    let absent = with_series.etf_surface_snapshot(&task.task.task_id);

    // Strip the only two fields that legitimately differ between two
    // invocations of the same store: the wall-clock query time and the
    // snapshot id built from it.
    fn volatile_free(value: &serde_json::Value) -> serde_json::Value {
        let mut value = value.clone();
        if let Some(map) = value.as_object_mut() {
            map.remove("query_time");
            map.remove("snapshot_id");
            map.remove("snapshot_time");
        }
        value
    }
    for ((name, a), (_, b)) in present.iter().zip(absent.iter()) {
        assert_eq!(
            volatile_free(a),
            volatile_free(b),
            "surface {name} differs once the 前复权 rows exist"
        );
    }
}
