//! Phase B (#260) data-side coverage: `source_generation_id` freezing at
//! acceptance, generation-bound worker sessions (the TOCTOU case), uniform
//! binding refusals and the pinned-session retire guard.
use serde_json::{Value, json};
use std::io::Write;
use std::path::Path;
use x_data_mcp::acquire::{AcquireService, Source};
use x_data_mcp::data_supply::collect::CollectionContract;
use x_data_mcp::data_supply::generation::{
    BindingRefusal, Bound, GenerationSource, SOURCE_GENERATION_KEY, uniform_binding,
};
use x_data_mcp::data_supply::wire::Dataset;
use x_data_mcp::market_data::Period;
use x_data_mcp::market_storage::Scope;
use x_data_mcp::official_market::DailyCollectionRepository;
use x_data_mcp::sqlite_market::SqliteMarket;
use x_data_mcp::sqlite_supply::SqliteSupply;
use x_data_mcp::sqlite_tasks::SqliteTasks;
use x_data_mcp::task_control::{Contract, ItemSpec, RequestOutcome, Status, service::TaskControl};

fn day_record(date: u32, ohlc: [u32; 4], amount: f32, volume: u32) -> [u8; 32] {
    let mut r = [0u8; 32];
    r[0..4].copy_from_slice(&date.to_le_bytes());
    for (i, w) in ohlc.iter().enumerate() {
        r[4 + i * 4..8 + i * 4].copy_from_slice(&w.to_le_bytes());
    }
    r[20..24].copy_from_slice(&amount.to_bits().to_le_bytes());
    r[24..28].copy_from_slice(&volume.to_le_bytes());
    r
}
fn gbbq_record(market: u8, code: &str, date_raw: u32, category: u8) -> [u8; 29] {
    let mut clear = [0u8; 29];
    clear[0] = market;
    clear[1..1 + code.len()].copy_from_slice(code.as_bytes());
    clear[8..12].copy_from_slice(&date_raw.to_le_bytes());
    clear[12] = category;
    x_data_mcp::gbbq::encrypt_record(&clear)
}
fn gbbq_member(records: &[[u8; 29]]) -> Vec<u8> {
    let mut bytes = (records.len() as u32).to_le_bytes().to_vec();
    for r in records {
        bytes.extend_from_slice(r);
    }
    bytes
}
fn write_zip(path: &Path, members: &[(&str, &[u8])]) {
    let file = std::fs::File::create(path).unwrap();
    let mut w = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();
    for (name, bytes) in members {
        w.start_file(*name, options).unwrap();
        w.write_all(bytes).unwrap();
    }
    w.finish().unwrap();
}
fn today_u32() -> u32 {
    x_data_mcp::calendar::shanghai_date(chrono::Utc::now())
        .format("%Y%m%d")
        .to_string()
        .parse()
        .unwrap()
}
/// Three-bar .day member for sh.600006 ending today, with real prices.
fn day_member() -> Vec<u8> {
    let today = today_u32();
    [
        day_record(today - 2, [1128, 1130, 1125, 1129], 0.0, 100),
        day_record(today - 1, [1129, 1132, 1128, 1131], 0.0, 110),
        day_record(today, [1131, 1135, 1130, 1134], 0.0, 120),
    ]
    .concat()
    .to_vec()
}
fn write_mirror(dir: &Path, code: u32) {
    write_mirror_day(dir, code, &day_member())
}
fn write_mirror_day(dir: &Path, code: u32, day: &[u8]) {
    std::fs::create_dir_all(dir).unwrap();
    let tdxhy = format!("1|{code}|T090302|||X6102\r\n").into_bytes();
    let ex = format!("{code}|name|kw\r\n").into_bytes();
    write_zip(
        &dir.join("base.zip"),
        &[
            ("tdxhy.cfg", tdxhy.as_slice()),
            ("infoharbor_ex.code", ex.as_slice()),
        ],
    );
    write_zip(
        &dir.join("gbbq.zip"),
        &[(
            "gbbq",
            gbbq_member(&[gbbq_record(1, "600006", 20240101, 1)]).as_slice(),
        )],
    );
    let name = format!("sh\\lday\\sh{code}.day");
    write_zip(&dir.join("hsjday.zip"), &[(name.as_str(), day)]);
}
fn drive(service: &AcquireService) {
    for _ in 0..16 {
        if service.worker_once().unwrap().is_none() {
            break;
        }
    }
}
/// Install + verify + activate one mirror generation; returns its id.
fn activate_generation(service: &AcquireService, request: &str, expected: Option<&str>) -> String {
    let acceptance = service.acquire_generation(request).unwrap();
    drive(service);
    let generation_id = service
        .task(&acceptance.task_id)
        .unwrap()
        .generation_id
        .unwrap();
    let reply = service
        .verify_generation(&format!("{request}-verify"), &generation_id)
        .unwrap();
    assert_eq!(reply.state, "consistency_verified", "{reply:?}");
    let activated = service
        .activate_generation(&format!("{request}-activate"), &generation_id, expected)
        .unwrap();
    assert_eq!(activated.state, "active", "{activated:?}");
    generation_id
}
fn opened(dir: &tempfile::TempDir) -> SqliteMarket {
    let tasks = SqliteTasks::new(dir.path().join("m.sqlite"));
    tasks.initialize().unwrap();
    SqliteMarket::new(tasks)
}
fn task_id(acceptance: &x_data_mcp::task_control::Acceptance) -> String {
    let RequestOutcome::Task(task) = &acceptance.record.outcome else {
        panic!("expected task outcome");
    };
    task.task.task_id.clone()
}
fn daily_payload(code: &str) -> Value {
    json!({"dataset":"market.daily","contract_version":"v1","selection":{"kind":"explicit","securities":[{"exchange":"sh","code":code}]}})
}

#[test]
fn acceptance_freezes_source_generation_id_and_worker_reads_it() {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), 600006);
    let root = tempfile::tempdir().unwrap();
    let service = AcquireService::open(root.path(), Source::Mirror(mirror.path().into())).unwrap();
    let gen_a = activate_generation(&service, "req-gen-a", None);
    let generation = GenerationSource::new(root.path());

    let dir = tempfile::tempdir().unwrap();
    let market = opened(&dir);
    let supply = SqliteSupply {
        market: x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(market.clone()),
    };
    let control = TaskControl::new(
        market.tasks.clone(),
        "run-g".into(),
        vec![Box::new(Bound {
            inner: Box::new(CollectionContract {
                dataset: Dataset::Daily,
                repository: supply.clone(),
            }),
            source: generation.clone(),
        }) as Box<dyn Contract>],
    )
    .unwrap();
    let accepted = control
        .accept("req-daily", control_key(&supply), daily_payload("600006"))
        .unwrap();
    let id = task_id(&accepted);
    let snapshot = control.read_task(&id).unwrap();
    // The acceptance froze generation A into every item — once.
    for item in &snapshot.items {
        assert_eq!(item.spec.frozen_inputs[SOURCE_GENERATION_KEY], json!(gen_a));
    }

    // Acquire + verify + activate B *after* acceptance. The frozen task
    // still reads A (which alone carries sh.600006), proving the worker
    // never re-consults the mutable active pointer.
    write_mirror(mirror.path(), 600007);
    let _gen_b = activate_generation(&service, "req-gen-b", Some(&gen_a));

    let done = x_data_mcp::data_supply::worker::run_once(
        &control,
        &x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(market.clone()),
        None,
        Some(&generation),
    )
    .unwrap()
    .unwrap();
    assert_eq!(done.task.status, Status::Succeeded, "{done:?}");
    assert!(
        market
            .effective_head(&Scope {
                symbol: "sh.600006".into(),
                period: Period::Day,
            })
            .unwrap()
            .is_some()
    );
}

#[test]
fn missing_and_null_bindings_refuse_the_source() {
    // Generation mode with an unwrapped contract: items carry no binding
    // key — the worker must refuse, never fall back to a mutable root.
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), 600006);
    let root = tempfile::tempdir().unwrap();
    let service = AcquireService::open(root.path(), Source::Mirror(mirror.path().into())).unwrap();
    let _gen_a = activate_generation(&service, "req-gen-a", None);
    let generation = GenerationSource::new(root.path());

    let dir = tempfile::tempdir().unwrap();
    let market = opened(&dir);
    let supply = SqliteSupply {
        market: x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(market.clone()),
    };
    let control = TaskControl::new(
        market.tasks.clone(),
        "run-m".into(),
        vec![Box::new(CollectionContract {
            dataset: Dataset::Daily,
            repository: supply.clone(),
        }) as Box<dyn Contract>],
    )
    .unwrap();
    let _accepted = control
        .accept("req-unbound", control_key(&supply), daily_payload("600006"))
        .unwrap();
    let done = x_data_mcp::data_supply::worker::run_once(
        &control,
        &x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(market.clone()),
        None,
        Some(&generation),
    )
    .unwrap()
    .unwrap();
    assert_ne!(done.task.status, Status::Succeeded);
    assert!(
        market
            .effective_head(&Scope {
                symbol: "sh.600006".into(),
                period: Period::Day,
            })
            .unwrap()
            .is_none()
    );

    // Null binding: a Bound contract on a root with no verified active
    // generation freezes `source_generation_id: null` — same refusal.
    let empty = tempfile::tempdir().unwrap();
    let empty_generation = GenerationSource::new(empty.path());
    assert_eq!(empty_generation.active_verified_id(), None);
    let dir2 = tempfile::tempdir().unwrap();
    let market2 = opened(&dir2);
    let supply2 = SqliteSupply {
        market: x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(market2.clone()),
    };
    let control2 = TaskControl::new(
        market2.tasks.clone(),
        "run-n".into(),
        vec![Box::new(Bound {
            inner: Box::new(CollectionContract {
                dataset: Dataset::Daily,
                repository: supply2.clone(),
            }),
            source: empty_generation.clone(),
        }) as Box<dyn Contract>],
    )
    .unwrap();
    let accepted = control2
        .accept("req-null", control_key(&supply2), daily_payload("600006"))
        .unwrap();
    let snapshot = control2.read_task(&task_id(&accepted)).unwrap();
    assert_eq!(
        snapshot.items[0].spec.frozen_inputs[SOURCE_GENERATION_KEY],
        Value::Null
    );
    let done = x_data_mcp::data_supply::worker::run_once(
        &control2,
        &x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(market2.clone()),
        None,
        Some(&empty_generation),
    )
    .unwrap()
    .unwrap();
    assert_ne!(done.task.status, Status::Succeeded);
}

#[test]
fn uniform_binding_resolution_is_fail_closed() {
    let spec = |item: &str, binding: Option<Value>| ItemSpec {
        item_id: item.into(),
        target: json!({}),
        progress_unit: "security".into(),
        frozen_inputs: match binding {
            Some(v) => json!({SOURCE_GENERATION_KEY: v}),
            None => json!({}),
        },
        scope_keys: ["s".to_string()].into_iter().collect(),
    };
    let empty: Vec<ItemSpec> = vec![];
    assert_eq!(uniform_binding(empty.iter()), Err(BindingRefusal::NoItems));
    assert_eq!(
        uniform_binding([spec("a", None)].iter()),
        Err(BindingRefusal::Missing)
    );
    assert_eq!(
        uniform_binding([spec("a", Some(Value::Null))].iter()),
        Err(BindingRefusal::Null)
    );
    let mixed = [
        spec("a", Some(json!("gen-a"))),
        spec("b", Some(json!("gen-b"))),
    ];
    assert_eq!(uniform_binding(mixed.iter()), Err(BindingRefusal::Mixed));
    // Missing key on any one item poisons an otherwise uniform task.
    let partial = [spec("a", Some(json!("gen-a"))), spec("b", None)];
    assert_eq!(uniform_binding(partial.iter()), Err(BindingRefusal::Mixed));
    let uniform = [
        spec("a", Some(json!("gen-a"))),
        spec("b", Some(json!("gen-a"))),
    ];
    assert_eq!(uniform_binding(uniform.iter()), Ok("gen-a".to_string()));
}

#[test]
fn bound_session_survives_activation_and_blocks_retire() {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), 600006);
    let root = tempfile::tempdir().unwrap();
    let service = AcquireService::open(root.path(), Source::Mirror(mirror.path().into())).unwrap();
    let gen_a = activate_generation(&service, "req-gen-a", None);
    let generation = GenerationSource::new(root.path());
    assert_eq!(
        generation.active_verified_id().as_deref(),
        Some(gen_a.as_str())
    );

    let bound = generation.open_bound(&gen_a, "task:held").unwrap();
    // The session reads the frozen generation even after B activates.
    write_mirror(mirror.path(), 600007);
    let _gen_b = activate_generation(&service, "req-gen-b", Some(&gen_a));
    let batch = bound.source.materialize_daily("sh.600006").unwrap();
    assert_eq!(batch.symbol, "sh.600006");
    assert_eq!(batch.bars.len(), 3);

    // The live session's pin blocks destructive retire of generation A.
    let manifest_sha = service.sync_status().unwrap().generations[0]
        .manifest_sha256
        .clone();
    let retire = service
        .retire_generation("req-retire-held", &gen_a, &manifest_sha)
        .unwrap();
    assert_eq!(retire.state, "refused");
    assert_eq!(retire.reason_code.as_deref(), Some("generation_referenced"));

    // A generation that never verified refuses a session outright.
    let acceptance = service.acquire_generation("req-gen-c").unwrap();
    drive(&service);
    let gen_c = service
        .task(&acceptance.task_id)
        .unwrap()
        .generation_id
        .unwrap();
    assert!(generation.open_bound(&gen_c, "task:c").is_err());
}

fn control_key(supply: &SqliteSupply) -> x_data_mcp::task_control::CommandKey {
    CollectionContract {
        dataset: Dataset::Daily,
        repository: supply.clone(),
    }
    .key()
}

#[test]
fn yearly_derivation_runs_against_bound_generation() {
    use chrono::Datelike;
    use x_data_mcp::market_update::tasks::Operation;
    use x_data_mcp::yearly_publication::tasks::YearlyContract;

    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), 600006);
    let root = tempfile::tempdir().unwrap();
    let service = AcquireService::open(root.path(), Source::Mirror(mirror.path().into())).unwrap();
    let gen_a = activate_generation(&service, "req-gen-a", None);
    let generation = GenerationSource::new(root.path());

    let dir = tempfile::tempdir().unwrap();
    let market = opened(&dir);
    let supply = SqliteSupply {
        market: x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(market.clone()),
    };
    let yearly = YearlyContract {
        operation: Operation::Update,
        repository: market.clone(),
        today: x_data_mcp::calendar::shanghai_today,
    };
    let yearly_key = yearly.key();
    let control = TaskControl::new(
        market.tasks.clone(),
        "run-y".into(),
        vec![
            Box::new(Bound {
                inner: Box::new(CollectionContract {
                    dataset: Dataset::Daily,
                    repository: supply.clone(),
                }),
                source: generation.clone(),
            }) as Box<dyn Contract>,
            Box::new(Bound {
                inner: Box::new(yearly),
                source: generation.clone(),
            }),
        ],
    )
    .unwrap();

    // Seed the daily base through the generation-bound daily path.
    control
        .accept("req-daily", control_key(&supply), daily_payload("600006"))
        .unwrap();
    let done = x_data_mcp::data_supply::worker::run_once(
        &control,
        &x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(market.clone()),
        None,
        Some(&generation),
    )
    .unwrap()
    .unwrap();
    assert_eq!(done.task.status, Status::Succeeded, "{done:?}");
    let base_id = market
        .effective_head(&Scope {
            symbol: "sh.600006".into(),
            period: Period::Day,
        })
        .unwrap()
        .unwrap();

    // Yearly derive: Bound freezes generation A into the item and the
    // worker verifies derivation against that pinned session — no
    // env-installed source exists anywhere in this test. `CurrentHead`
    // promotes the derived yearly to the effective head.
    let year = x_data_mcp::calendar::shanghai_today().year();
    let accepted = control
        .accept(
            "req-yearly",
            yearly_key,
            json!({"items":[{"symbol":"sh.600006","base_daily_version_id":base_id,"years":[year],"intent":{"CurrentHead":{"expected_daily_head":base_id,"expected_yearly_head":null}},"comparison_version_id":null}]}),
        )
        .unwrap();
    let snapshot = control.read_task(&task_id(&accepted)).unwrap();
    assert_eq!(
        snapshot.items[0].spec.frozen_inputs[SOURCE_GENERATION_KEY],
        json!(gen_a)
    );
    let done = x_data_mcp::data_supply::worker::run_once(
        &control,
        &x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(market.clone()),
        None,
        Some(&generation),
    )
    .unwrap()
    .unwrap();
    assert_eq!(done.task.status, Status::Succeeded, "{done:?}");
    assert!(
        market
            .effective_head(&Scope {
                symbol: "sh.600006".into(),
                period: Period::Year,
            })
            .unwrap()
            .is_some()
    );
}

#[test]
fn yearly_derivation_without_binding_refuses() {
    use x_data_mcp::market_update::tasks::Operation;
    use x_data_mcp::yearly_publication::tasks::YearlyContract;

    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), 600006);
    let root = tempfile::tempdir().unwrap();
    let service = AcquireService::open(root.path(), Source::Mirror(mirror.path().into())).unwrap();
    let _gen_a = activate_generation(&service, "req-gen-a", None);
    let generation = GenerationSource::new(root.path());

    let dir = tempfile::tempdir().unwrap();
    let market = opened(&dir);
    let supply = SqliteSupply {
        market: x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(market.clone()),
    };
    let control = TaskControl::new(
        market.tasks.clone(),
        "run-u".into(),
        vec![
            Box::new(Bound {
                inner: Box::new(CollectionContract {
                    dataset: Dataset::Daily,
                    repository: supply.clone(),
                }),
                source: generation.clone(),
            }) as Box<dyn Contract>,
            // Unwrapped: the accepted item carries no binding key.
            Box::new(YearlyContract {
                operation: Operation::Update,
                repository: market.clone(),
                today: x_data_mcp::calendar::shanghai_today,
            }),
        ],
    )
    .unwrap();
    control
        .accept("req-daily", control_key(&supply), daily_payload("600006"))
        .unwrap();
    let done = x_data_mcp::data_supply::worker::run_once(
        &control,
        &x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(market.clone()),
        None,
        Some(&generation),
    )
    .unwrap()
    .unwrap();
    assert_eq!(done.task.status, Status::Succeeded, "{done:?}");
    let base_id = market
        .effective_head(&Scope {
            symbol: "sh.600006".into(),
            period: Period::Day,
        })
        .unwrap()
        .unwrap();
    let year = chrono::Datelike::year(&x_data_mcp::calendar::shanghai_today());
    control
        .accept(
            "req-yearly",
        YearlyContract {
            operation: Operation::Update,
            repository: market.clone(),
            today: x_data_mcp::calendar::shanghai_today,
        }
        .key(),
        json!({"items":[{"symbol":"sh.600006","base_daily_version_id":base_id,"years":[year],"intent":{"CurrentHead":{"expected_daily_head":base_id,"expected_yearly_head":null}},"comparison_version_id":null}]}),
    )
    .unwrap();
    let done = x_data_mcp::data_supply::worker::run_once(
        &control,
        &x_data_mcp::sqlite_market::backend::MarketBackend::Legacy(market.clone()),
        None,
        Some(&generation),
    )
    .unwrap()
    .unwrap();
    assert_ne!(done.task.status, Status::Succeeded);
    assert!(
        market
            .effective_head(&Scope {
                symbol: "sh.600006".into(),
                period: Period::Year,
            })
            .unwrap()
            .is_none()
    );
}

/// The process-scoped verified-session cache: the first open byte-verifies
/// and inspects the generation; later opens on the same GenerationSource
/// reuse the verdict — but each open still takes a fresh task pin, and
/// verification failures are never cached.
#[test]
fn bound_session_cache_verifies_once_and_keeps_per_open_pins() {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), 600006);
    let root = tempfile::tempdir().unwrap();
    let service = AcquireService::open(root.path(), Source::Mirror(mirror.path().into())).unwrap();
    let gen_id = activate_generation(&service, "req-cache", None);
    let generation = GenerationSource::new(root.path());

    // Two opens on the same generation: one verification, two live pins.
    let a = generation.open_bound(&gen_id, "task:cache-a").unwrap();
    let b = generation.open_bound(&gen_id, "task:cache-b").unwrap();
    assert_eq!(
        generation.store().pins(&gen_id).unwrap(),
        vec!["task:cache-a".to_string(), "task:cache-b".to_string()]
    );
    drop(a);
    drop(b);
    assert!(generation.store().pins(&gen_id).unwrap().is_empty());

    // Documented relaxation: artifact bytes rewritten after the process's
    // first verification still open on the cached verdict — while a fresh
    // GenerationSource (the next process) re-verifies and fails closed.
    let dir = root.path().join("generations").join(&gen_id);
    std::fs::write(dir.join("hsjday.zip"), b"tampered").unwrap();
    generation.open_bound(&gen_id, "task:cached").unwrap();
    assert!(
        GenerationSource::new(root.path())
            .open_bound(&gen_id, "task:fresh")
            .is_err()
    );

    // The accept path shares the session cache for the byte-level verdict:
    // the same process still resolves the cached generation while a fresh
    // source re-verifies and refuses — the active *pointer* itself is
    // re-read on every call, so activation switching is unaffected.
    assert_eq!(
        generation.active_verified_id().as_deref(),
        Some(gen_id.as_str())
    );
    assert!(
        GenerationSource::new(root.path())
            .active_verified_id()
            .is_none()
    );
}

/// #288 regression: a daily `update_data` append across a generation
/// boundary must commit on the compact backend. The typed ops path cannot
/// carry the new coverage claim (the reason text binds the new manifest
/// sha) nor rebind `source_bindings`, so the envelope fallback is the
/// designed carrier — and before the fix it staged a record with no
/// `compact_source`, which `stage_tx` rejects fail-closed.
#[test]
fn explicit_daily_update_appends_across_generations_on_compact() {
    use x_data_mcp::market_storage::MarketRepository;
    use x_data_mcp::market_update::tasks::{Operation, RangeContract};
    use x_data_mcp::sqlite_market::backend::MarketBackend;

    let today = today_u32();
    // Gen A covers bars through yesterday; gen B is the exact same prefix
    // plus one newer record — a real append a continuity probe can prove.
    let member_a = [
        day_record(today - 3, [1128, 1130, 1125, 1129], 0.0, 100),
        day_record(today - 2, [1129, 1132, 1128, 1131], 0.0, 110),
        day_record(today - 1, [1131, 1135, 1130, 1134], 0.0, 120),
    ]
    .concat();
    let mut member_b = member_a.clone();
    member_b.extend_from_slice(&day_record(today, [1134, 1138, 1133, 1137], 0.0, 130));

    let mirror = tempfile::tempdir().unwrap();
    write_mirror_day(mirror.path(), 600006, &member_a);
    let root = tempfile::tempdir().unwrap();
    let service = AcquireService::open(root.path(), Source::Mirror(mirror.path().into())).unwrap();
    let gen_a = activate_generation(&service, "req-gen-a", None);
    let generation = GenerationSource::new(root.path());

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("market.db");
    let tasks = SqliteTasks::new(&db);
    tasks.initialize_compact().unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o600)).unwrap();
    let backend = MarketBackend::open(&tasks).unwrap();
    assert!(matches!(backend, MarketBackend::Compact(_)));
    let tasks = backend.tasks();
    let supply = SqliteSupply {
        market: backend.clone(),
    };
    let range = RangeContract {
        operation: Operation::Update,
        repository: backend.clone(),
        today: x_data_mcp::calendar::shanghai_today,
    };
    let range_key = range.key();
    let control = TaskControl::new(
        tasks.clone(),
        "run-u".into(),
        vec![
            Box::new(Bound {
                inner: Box::new(CollectionContract {
                    dataset: Dataset::Daily,
                    repository: supply.clone(),
                }),
                source: generation.clone(),
            }) as Box<dyn Contract>,
            Box::new(Bound {
                inner: Box::new(range),
                source: generation.clone(),
            }),
        ],
    )
    .unwrap();

    // Seed the daily head from generation A.
    control
        .accept("req-daily", control_key(&supply), daily_payload("600006"))
        .unwrap();
    let done =
        x_data_mcp::data_supply::worker::run_once(&control, &backend, None, Some(&generation))
            .unwrap()
            .unwrap();
    assert_eq!(done.task.status, Status::Succeeded, "{done:?}");
    let scope = Scope {
        symbol: "sh.600006".into(),
        period: Period::Day,
    };
    let base_id = backend.effective_head(&scope).unwrap().unwrap();

    // Advance the mirror to gen B and activate it — the update task then
    // freezes B into every item at acceptance.
    write_mirror_day(mirror.path(), 600006, &member_b);
    let gen_b = activate_generation(&service, "req-gen-b", Some(&gen_a));
    let day = x_data_mcp::calendar::shanghai_today();
    let accepted = control
        .accept(
            "req-update",
            range_key,
            json!({"items":[{"symbol":"sh.600006","base_version_id":base_id,"start_date":day,"end_date":day}]}),
        )
        .unwrap();
    let snapshot = control.read_task(&task_id(&accepted)).unwrap();
    assert_eq!(
        snapshot.items[0].spec.frozen_inputs[SOURCE_GENERATION_KEY],
        json!(gen_b)
    );
    let done =
        x_data_mcp::data_supply::worker::run_once(&control, &backend, None, Some(&generation))
            .unwrap()
            .unwrap();
    assert_eq!(done.task.status, Status::Succeeded, "{done:?}");

    // The new head is a fresh immutable version bound to generation B,
    // carrying the appended record; the source binding proves the rebind
    // the ops path could not express.
    let new_id = backend.effective_head(&scope).unwrap().unwrap();
    assert_ne!(new_id, base_id);
    let head = backend.fixed(&new_id).unwrap();
    assert_eq!(head.version().records.len(), 4);
    let conn = rusqlite::Connection::open(dir.path().join("market.db")).unwrap();
    let bound: String = conn
        .query_row(
            "SELECT sb.generation_id FROM data_versions v \
             JOIN source_bindings sb ON sb.source_binding_digest = v.source_binding_digest \
             WHERE v.version_id = ?1",
            [new_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(bound, gen_b);
}
