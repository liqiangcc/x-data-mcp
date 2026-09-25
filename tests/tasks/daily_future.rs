use chrono::{Datelike, Utc};
use serde_json::{Value, json};
use x_data_mcp::{
    data_supply::{Access, mcp::DataApi},
    mcp::ToolHandler,
    sqlite_market::SqliteMarket,
    sqlite_tasks::SqliteTasks,
};
#[path = "../support/market.rs"]
mod fixture;
fn setup() -> (tempfile::TempDir, SqliteMarket, DataApi) {
    let dir = tempfile::tempdir().unwrap();
    let tasks = SqliteTasks::new(dir.path().join("db"));
    tasks.initialize().unwrap();
    let market = SqliteMarket::new(tasks.clone());
    fixture::published_fixture(&market, &fixture::samples()[0]["record"]);
    let api = DataApi::new(tasks, "future-run".into(), Access::local()).unwrap();
    (dir, market, api)
}
fn request(id: &str, start: &str, end: &str) -> Value {
    json!({"request_id":id,"dataset":"market.daily","contract_version":"v1","items":[{"entity":{"exchange":"sh","code":"600006"},"base_version_id":"legacy-daily","start_date":start,"end_date":end}]})
}
#[test]
fn daily_future_batch_is_rejected_without_request_task_or_occupancy() {
    let (_dir, market, api) = setup();
    let today = Utc::now()
        .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap())
        .date_naive();
    assert!(today.year() < 9999);
    let tomorrow = today.succ_opt().unwrap().to_string();
    for operation in ["update_data", "backfill_data"] {
        let mut input = request(operation, "2025-01-02", "2025-01-02");
        input["items"].as_array_mut().unwrap().push(json!({"entity":{"exchange":"sh","code":"600007"},"base_version_id":"not-looked-up","start_date":"2025-01-01","end_date":tomorrow}));
        let rejected = api.call(operation, input).unwrap_err();
        assert_eq!(rejected.code, "invalid_arguments");
        assert!(
            rejected.message.contains("600007") && rejected.message.contains(&tomorrow),
            "{rejected:?}"
        );
        assert_eq!(
            api.call(
                "get_request",
                json!({"contract_version":"v1","request_id":operation})
            )
            .unwrap()["bound"],
            false
        );
    }
    let c = rusqlite::Connection::open(market.tasks.path()).unwrap();
    let n: i64 = c
        .query_row("SELECT count(*) FROM scope_occupancy", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
    let n: i64 = c
        .query_row("SELECT count(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1); // Only the initial fixture publication.
}
#[test]
fn daily_future_single_request_must_not_be_accepted() {
    let (_dir, market, api) = setup();
    let tomorrow = Utc::now()
        .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap())
        .date_naive()
        .succ_opt()
        .unwrap()
        .to_string();
    let rejected = api.call("update_data", request("future", "2025-01-02", &tomorrow));
    assert!(rejected.is_err(), "future range was accepted: {rejected:?}");
    assert_eq!(
        api.call(
            "get_request",
            json!({"contract_version":"v1","request_id":"future"})
        )
        .unwrap()["bound"],
        false
    );
    assert_eq!(
        market
            .versions(&x_data_mcp::market_storage::Scope {
                symbol: "sh.600006".into(),
                period: x_data_mcp::market_data::Period::Day
            })
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn daily_today_is_allowed_and_accepted_replay_does_not_reconsult_clock_or_base() {
    use x_data_mcp::{
        market_update::tasks::{Operation, RangeContract},
        task_control::{Contract, RequestOutcome, service::TaskControl},
    };
    let (_dir, market, _api) = setup();
    fn today() -> chrono::NaiveDate {
        "2026-09-13".parse().unwrap()
    }
    let contract = RangeContract {
        operation: Operation::Update,
        repository: market.clone(),
        today,
    };
    let key = contract.key();
    let control = TaskControl::new(
        market.tasks.clone(),
        "future-run".into(),
        vec![Box::new(contract)],
    )
    .unwrap();
    let input = json!({"items":[{"symbol":"sh.600006","base_version_id":"legacy-daily","start_date":"2025-01-02","end_date":"2026-09-13"}]});
    let accepted = control.accept("today", key.clone(), input.clone()).unwrap();
    let RequestOutcome::Task(original) = accepted.record.outcome else {
        panic!()
    };
    assert_eq!(
        original.items[0].spec.frozen_inputs["range"]["end"],
        "2026-09-13"
    );
    rusqlite::Connection::open(market.tasks.path())
        .unwrap()
        .execute_batch("UPDATE segment_bars SET bar_json=json_set(bar_json,'$.close','0')")
        .unwrap();
    let earlier = TaskControl::new(
        market.tasks.clone(),
        "future-run".into(),
        vec![Box::new(RangeContract {
            operation: Operation::Update,
            repository: market.clone(),
            today: || "2026-09-12".parse().unwrap(),
        })],
    )
    .unwrap();
    let replay = earlier.accept("today", key.clone(), input.clone()).unwrap();
    assert!(replay.reused);
    let RequestOutcome::Task(replayed) = replay.record.outcome else {
        panic!()
    };
    assert_eq!(replayed.task.task_id, original.task.task_id);
    assert!(earlier.accept("new-future", key, input).is_err());
    assert!(earlier.read_request("new-future").unwrap().is_none());
}
