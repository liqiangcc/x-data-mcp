use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, ChildStdout, Command, Stdio},
};

struct Session {
    child: Child,
    stdout: BufReader<ChildStdout>,
    sequence: u64,
}
impl Session {
    fn new(path: &Path) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mcp_data"));
        command.arg(path);
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdout,
            sequence: 0,
        }
    }
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.sequence += 1;
        let request = json!({"jsonrpc":"2.0","id":self.sequence,"method":method,"params":params});
        let input = self.child.stdin.as_mut().unwrap();
        writeln!(input, "{request}").unwrap();
        input.flush().unwrap();
        let mut line = String::new();
        assert!(self.stdout.read_line(&mut line).unwrap() > 0);
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], self.sequence);
        response
    }
    fn call(&mut self, name: &str, args: Value, error: bool) -> Value {
        let response = self.request("tools/call", json!({"name":name,"arguments":args}));
        assert_eq!(response["result"]["isError"], error, "{response}");
        let result = &response["result"];
        assert_eq!(result["content"][0]["type"], "text");
        let text: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text, result["structuredContent"]);
        text
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        assert!(self.child.wait().unwrap().success());
    }
}
#[test]
fn real_data_stdio_discovery_queries_and_control_schema_roundtrip() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("db");
    x_data_mcp::sqlite_tasks::SqliteTasks::new(&path)
        .initialize()
        .unwrap();
    let mut s = Session::new(&path);
    assert!(s.request("initialize", json!({}))["result"]["serverInfo"].is_object());
    let listing = s.request("tools/list", json!({}));
    let tools = listing["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), if cfg!(target_os = "linux") { 23 } else { 22 });
    let caps = s.call("get_data_capabilities", json!({}), false);
    let listed: std::collections::BTreeSet<_> =
        tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    let discovered: std::collections::BTreeSet<_> = caps["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["tool_name"].as_str().unwrap())
        .collect();
    assert_eq!(listed, discovered);
    let worker = s.call(
        "get_data_capabilities",
        json!({"operation":"run_worker_once"}),
        false,
    );
    assert_eq!(worker["items"].as_array().unwrap().len(), 1);
    assert!(
        worker["items"][0]["description"]
            .as_str()
            .unwrap()
            .contains("never downloads market data")
    );
    let worker_tool = tools
        .iter()
        .find(|t| t["name"] == "run_worker_once")
        .unwrap();
    assert_eq!(
        worker["items"][0]["input_schema"],
        worker_tool["inputSchema"]
    );
    assert_eq!(
        worker["items"][0]["output_schema"],
        worker_tool["outputSchema"]
    );
    assert_eq!(
        s.call("run_worker_once", json!({"extra":true}), true)["code"],
        "invalid_arguments"
    );
    // Single-action contract: republish advertises market.daily only, and a
    // wrong-dataset payload fails at decode with zero side effects.
    let republish = s.call(
        "get_data_capabilities",
        json!({"operation":"republish_data"}),
        false,
    );
    assert_eq!(republish["items"].as_array().unwrap().len(), 1);
    assert_eq!(republish["items"][0]["dataset"], "market.daily");
    let yearly_republish = json!({"request_id":"rp-yearly","dataset":"market.yearly","contract_version":"v1","items":[{"entity":{"exchange":"sh","code":"600006"},"base_daily_version_id":"x","years":[2025],"publication":{"mode":"historical_fixed_base"}}]});
    assert!(
        !jsonschema::validator_for(&republish["items"][0]["input_schema"])
            .unwrap()
            .is_valid(&yearly_republish)
    );
    let err = s.call("republish_data", yearly_republish, true);
    assert_eq!(err["code"], "invalid_arguments");
    assert_eq!(
        s.call(
            "get_request",
            json!({"contract_version":"v1","request_id":"rp-yearly"}),
            false
        )["bound"],
        false
    );
    // Facts only: no error payload carries workflow next actions.
    for e in [
        err,
        s.call("update_data", json!({"dataset":"market.yearly"}), true),
    ] {
        assert!(e.get("next_actions").is_none(), "{e}");
    }
    // production_ready=false must carry structured reasons.
    let reasons = republish["production_blocking_reasons"].as_array().unwrap();
    assert!(!reasons.is_empty());
    assert!(
        reasons
            .iter()
            .all(|r| r["code"].is_string() && r["detail"].is_string())
    );
    assert!(reasons.iter().any(|r| {
        r["code"] == "declared_scope_non_production"
            && r["detail"]
                .as_str()
                .is_some_and(|d| d.contains("external distribution is not authorized"))
    }));
    assert!(
        reasons
            .iter()
            .all(|r| !r["detail"].as_str().unwrap_or_default().contains("#225"))
    );
    assert_eq!(republish["capability_revision"], "generic-data-v7");
    let schema =
        |name: &str, field: &str| tools.iter().find(|t| t["name"] == name).unwrap()[field].clone();
    for entry in x_data_mcp::data_supply::registry::entries() {
        jsonschema::validator_for(&entry.input)
            .unwrap()
            .validate(&entry.example)
            .unwrap();
        if matches!(
            entry.handler,
            x_data_mcp::data_supply::registry::Handler::Compute
                | x_data_mcp::data_supply::registry::Handler::Progress
                | x_data_mcp::data_supply::registry::Handler::VerifyStopped
                | x_data_mcp::data_supply::registry::Handler::VerifyPublication
                | x_data_mcp::data_supply::registry::Handler::InspectVerification
                | x_data_mcp::data_supply::registry::Handler::VerifyGap
                | x_data_mcp::data_supply::registry::Handler::Collection
                | x_data_mcp::data_supply::registry::Handler::Range(_)
                | x_data_mcp::data_supply::registry::Handler::Republish
                | x_data_mcp::data_supply::registry::Handler::Retry
                | x_data_mcp::data_supply::registry::Handler::Task
                | x_data_mcp::data_supply::registry::Handler::Cancel
                | x_data_mcp::data_supply::registry::Handler::PlanMarketUpdate
                | x_data_mcp::data_supply::registry::Handler::RunMarketUpdate
        ) {
            continue;
        }
        let response = s.call(entry.name, entry.example, false);
        jsonschema::validator_for(&entry.output)
            .unwrap()
            .validate(&response)
            .unwrap();
    }
    let input = json!({"request_id":"r","dataset":"market.daily","contract_version":"v1","selection":{"kind":"explicit","securities":[{"exchange":"sh","code":"600006"}]}});
    jsonschema::validator_for(&schema("start_collection", "inputSchema"))
        .unwrap()
        .validate(&input)
        .unwrap();
    let accepted = s.call("start_collection", input.clone(), false);
    let task = accepted["task_id"].clone();
    let progress = s.call("get_task_progress", json!({"task_id":task}), false);
    jsonschema::validator_for(&schema("get_task_progress", "outputSchema"))
        .unwrap()
        .validate(&progress)
        .unwrap();
    assert_eq!(progress["total"], 1);
    let request = s.call(
        "get_request",
        json!({"contract_version":"v1","request_id":"r"}),
        false,
    );
    assert_eq!(request["bound"], true);
    let mut extra = input.clone();
    extra["symbols"] = json!(["sh.600006"]);
    assert!(
        jsonschema::validator_for(&schema("start_collection", "inputSchema"))
            .unwrap()
            .validate(&extra)
            .is_err()
    );
    assert_eq!(
        s.call("start_collection", extra, true)["code"],
        "invalid_arguments"
    );
    let executed = s.call("run_worker_once", json!({}), false);
    assert_eq!(executed["idle"], false);
    assert_eq!(executed["status"], "failed");
    jsonschema::validator_for(&worker["items"][0]["output_schema"])
        .unwrap()
        .validate(&executed)
        .unwrap();
    let p = s.call("get_task_progress", json!({"task_id":task}), false);
    assert_eq!(p["failed"], 1);
    let retry = json!({"contract_version":"v1","original_task_id":task,"new_request_id":"retry","item_ids":accepted["item_ids"]});
    jsonschema::validator_for(&schema("retry_task", "inputSchema"))
        .unwrap()
        .validate(&retry)
        .unwrap();
    let a = s.call("retry_task", retry, false);
    let view = s.call(
        "get_task",
        json!({"contract_version":"v1","task_id":a["task_id"]}),
        false,
    );
    let request = s.call(
        "get_request",
        json!({"contract_version":"v1","request_id":"retry"}),
        false,
    );
    assert_eq!(request["task"]["task_id"], a["task_id"]);
    let rev = view["state_revision"].as_u64().unwrap();
    s.call(
        "cancel_task",
        json!({"contract_version":"v1","task_id":a["task_id"],"expected_revision":rev}),
        false,
    );
    let view = s.call(
        "get_task",
        json!({"contract_version":"v1","task_id":a["task_id"]}),
        false,
    );
    let rev = view["state_revision"].as_u64().unwrap();
    assert!(rev > 0);
    // A stale expected_revision no longer rejects on a live task — cancel
    // applies at the committed revision; here it is idempotent because
    // the task is already cancel_requested.
    let cancelled = s.call(
        "cancel_task",
        json!({"contract_version":"v1","task_id":a["task_id"],"expected_revision":rev-1}),
        false,
    );
    assert_eq!(cancelled["status"], "cancel_requested");
    assert_eq!(cancelled["state_revision"], rev);
    s.call(
        "cancel_task",
        json!({"contract_version":"v1","task_id":a["task_id"],"expected_revision":rev}),
        false,
    );
    let detail = s.call(
        "get_data_capabilities",
        json!({"dataset":"market.daily","operation":"query_data"}),
        false,
    );
    assert_eq!(
        detail["items"][0]["input_schema"],
        schema("query_data", "inputSchema")
    );
    assert_eq!(detail["production_ready"], false);
}
#[test]
fn real_data_stdio_read_missing_database_does_not_initialize() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("missing");
    let mut s = Session::new(&p);
    s.call("list_tasks", json!({}), true);
    assert!(!p.exists());
}

use crate::support_market as range_fixture;
#[test]
fn real_range_stdio_acceptance_replay_conflicts_and_blocked_execution() {
    for operation in ["update_data", "backfill_data"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let tasks = x_data_mcp::sqlite_tasks::SqliteTasks::new(&path);
        tasks.initialize().unwrap();
        let market = x_data_mcp::sqlite_market::SqliteMarket::new(tasks);
        range_fixture::published_fixture(&market, &range_fixture::samples()[0]["record"]);
        let mut s = Session::new(&path);
        s.request("initialize", json!({}));
        let capabilities = s.call(
            "get_data_capabilities",
            json!({"operation":operation}),
            false,
        );
        let cap = &capabilities["items"][0];
        assert_eq!(capabilities["items"].as_array().unwrap().len(), 2);
        assert_eq!(cap["dataset"], "market.daily");
        assert_eq!(cap["runtime_readiness"], "blocked");
        assert_eq!(cap["blocking_reasons"][0]["code"], "verified_source_absent");
        assert!(
            cap["description"]
                .as_str()
                .unwrap()
                .contains("market.daily")
        );
        let mut input = json!({"request_id":"range-write","dataset":"market.daily","contract_version":"v1","items":[{"entity":{"exchange":"sh","code":"600006"},"base_version_id":"legacy-daily","start_date":"2025-01-02","end_date":"2025-01-02"}]});
        jsonschema::validator_for(&cap["input_schema"])
            .unwrap()
            .validate(&input)
            .unwrap();
        let accepted = s.call(operation, input.clone(), false);
        jsonschema::validator_for(&cap["output_schema"])
            .unwrap()
            .validate(&accepted)
            .unwrap();
        assert_eq!(accepted["runtime_readiness"], "blocked");
        assert_eq!(accepted["contract_version"], "explicit-range-task-v1");
        let duplicate = input["items"][0].clone();
        input["items"].as_array_mut().unwrap().push(duplicate);
        let replay = s.call(operation, input.clone(), false);
        assert_eq!(replay["task_id"], accepted["task_id"]);
        assert_eq!(replay["reused"], true);
        let other_operation = if operation == "update_data" {
            "backfill_data"
        } else {
            "update_data"
        };
        assert_eq!(
            s.call(other_operation, input.clone(), true)["code"],
            "request_conflict"
        );
        input["request_id"] = json!("range-competing");
        assert_eq!(
            s.call(operation, input.clone(), true)["code"],
            "range_conflict"
        );
        assert_eq!(
            s.call(
                "get_request",
                json!({"contract_version":"v1","request_id":"range-competing"}),
                false
            )["bound"],
            false
        );
        input["request_id"] = json!("bad-field");
        input["items"][0]["extra"] = json!(true);
        assert_eq!(s.call(operation, input, true)["code"], "invalid_arguments");
        assert_eq!(
            s.call(
                "get_request",
                json!({"contract_version":"v1","request_id":"range-write"}),
                false
            )["task"]["task_id"],
            accepted["task_id"]
        );
        let worker = s.call("run_worker_once", json!({}), false);
        assert_eq!(worker["task_id"], accepted["task_id"]);
        let progress = s.call(
            "get_task_progress",
            json!({"task_id":accepted["task_id"]}),
            false,
        );
        assert_eq!(progress["failed"], 1);
        assert_eq!(
            progress["frozen_daily_ranges"][0]["base_version_id"],
            "legacy-daily"
        );
        assert_eq!(
            progress["frozen_daily_ranges"][0]["start_date"],
            "2025-01-02"
        );
        assert_eq!(progress["range_results"], json!([]));
        assert!(
            progress["unfinished"][0]["error"]
                .as_str()
                .unwrap()
                .contains("SOURCE_NOT_VERIFIED")
        );
        let retried=s.call("retry_task",json!({"contract_version":"v1","original_task_id":accepted["task_id"],"new_request_id":"range-retry","item_ids":accepted["item_ids"]}),false);
        let view = s.call(
            "get_task",
            json!({"contract_version":"v1","task_id":retried["task_id"]}),
            false,
        );
        s.call("cancel_task",json!({"contract_version":"v1","task_id":retried["task_id"],"expected_revision":view["state_revision"]}),false);
        s.call("run_worker_once", json!({}), false);
        let versions: i64 = rusqlite::Connection::open(&path)
            .unwrap()
            .query_row("SELECT count(*) FROM data_versions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(versions, 1);
        assert!(
            s.call(
                "get_data_capabilities",
                json!({"operation":operation,"dataset":"securities.catalog"}),
                false
            )["items"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn real_yearly_update_stdio_has_closed_schema_frozen_inputs_and_blocked_worker() {
    for operation in ["update_data", "backfill_data"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let tasks = x_data_mcp::sqlite_tasks::SqliteTasks::new(&path);
        tasks.initialize().unwrap();
        let market = x_data_mcp::sqlite_market::SqliteMarket::new(tasks);
        range_fixture::published_fixture(&market, &range_fixture::samples()[0]["record"]);
        let mut s = Session::new(&path);
        s.request("initialize", json!({}));
        let caps = s.call(
            "get_data_capabilities",
            json!({"operation":operation,"dataset":"market.yearly"}),
            false,
        );
        assert_eq!(caps["items"].as_array().unwrap().len(), 1);
        assert_eq!(caps["items"][0]["runtime_readiness"], "ready");
        let schema = jsonschema::validator_for(&caps["items"][0]["input_schema"]).unwrap();
        schema.validate(&caps["items"][0]["example"]).unwrap();
        assert_eq!(caps["items"][0]["example"]["dataset"], "market.yearly");
        let input = json!({"request_id":"yearly-wire","dataset":"market.yearly","contract_version":"v1","items":[{"entity":{"exchange":"sh","code":"600006"},"base_daily_version_id":"legacy-daily","years":[2025],"publication":{"mode":"historical_fixed_base","comparison_version_id":null}}]});
        schema.validate(&input).unwrap();
        for location in [0, 1, 2] {
            let mut bad = input.clone();
            bad["request_id"] = json!(format!("bad-{location}"));
            match location {
                0 => bad["provider"] = json!("fixture"),
                1 => bad["items"][0]["coverage_verified"] = json!(true),
                _ => bad["items"][0]["publication"]["force"] = json!(true),
            };
            assert!(!schema.is_valid(&bad));
            assert_eq!(s.call(operation, bad, true)["code"], "invalid_arguments");
        }
        let accepted = s.call(operation, input.clone(), false);
        assert_eq!(accepted["contract_version"], "explicit-yearly-task-v1");
        assert_eq!(accepted["runtime_readiness"], "ready");
        assert_eq!(
            s.call(operation, input.clone(), false)["task_id"],
            accepted["task_id"]
        );
        let mut competing = input.clone();
        competing["request_id"] = json!("competing-year");
        competing["items"][0]["years"] = json!([2024]);
        assert_eq!(s.call(operation, competing, true)["code"], "range_conflict");
        assert_eq!(
            s.call(
                "get_request",
                json!({"contract_version":"v1","request_id":"competing-year"}),
                false
            )["bound"],
            false
        );
        let worker = s.call("run_worker_once", json!({}), false);
        assert_eq!(worker["task_id"], accepted["task_id"]);
        let progress = s.call(
            "get_task_progress",
            json!({"task_id":accepted["task_id"]}),
            false,
        );
        assert_eq!(progress["failed"], 1);
        assert_eq!(progress["yearly_results"], json!([]));
        assert_eq!(
            progress["frozen_yearly_inputs"][0]["base_daily_version_id"],
            "legacy-daily"
        );
        assert_eq!(progress["frozen_yearly_inputs"][0]["years"], json!([2025]));
        assert_eq!(
            progress["frozen_yearly_inputs"][0]["publication"]["mode"],
            "historical_fixed_base"
        );
        assert!(
            progress["unfinished"][0]["error"]
                .as_str()
                .unwrap()
                .contains("SOURCE_NOT_VERIFIED")
        );
        let retried=s.call("retry_task",json!({"contract_version":"v1","original_task_id":accepted["task_id"],"new_request_id":"yearly-retry","item_ids":accepted["item_ids"]}),false);
        let view = s.call(
            "get_task",
            json!({"contract_version":"v1","task_id":retried["task_id"]}),
            false,
        );
        s.call("cancel_task",json!({"contract_version":"v1","task_id":retried["task_id"],"expected_revision":view["state_revision"]}),false);
        s.call("run_worker_once", json!({}), false);
        assert!(
            market
                .versions(&x_data_mcp::market_storage::Scope {
                    symbol: "sh.600006".into(),
                    period: x_data_mcp::market_data::Period::Year
                })
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn real_daily_future_stdio_rejects_without_binding_and_accepts_today() {
    for operation in ["update_data", "backfill_data"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        let tasks = x_data_mcp::sqlite_tasks::SqliteTasks::new(&path);
        tasks.initialize().unwrap();
        let market = x_data_mcp::sqlite_market::SqliteMarket::new(tasks);
        range_fixture::published_fixture(&market, &range_fixture::samples()[0]["record"]);
        let mut s = Session::new(&path);
        s.request("initialize", json!({}));
        let today = x_data_mcp::calendar::shanghai_today();
        let future = today.succ_opt().unwrap();
        let mut input = json!({"request_id":"future-day","dataset":"market.daily","contract_version":"v1","items":[{"entity":{"exchange":"sh","code":"600006"},"base_version_id":"legacy-daily","start_date":"2025-01-02","end_date":future}]});
        let error = s.call(operation, input.clone(), true);
        assert_eq!(error["code"], "invalid_arguments");
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .contains(&future.to_string())
        );
        assert_eq!(
            s.call(
                "get_request",
                json!({"contract_version":"v1","request_id":"future-day"}),
                false
            )["bound"],
            false
        );
        input["request_id"] = json!("today-day");
        input["items"][0]["end_date"] = json!(today);
        let accepted = s.call(operation, input.clone(), false);
        assert_eq!(accepted["status"], "pending");
        assert_eq!(
            s.call(operation, input, false)["task_id"],
            accepted["task_id"]
        );
    }
}

#[test]
fn real_stop_verification_stdio_is_strict_discoverable_and_keeps_original_claims() {
    use x_data_mcp::task_control::{
        Contract, RequestOutcome, TaskRepository, legacy::LegacyCollection, service::TaskControl,
    };
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("db");
    let db = x_data_mcp::sqlite_tasks::SqliteTasks::new(&path);
    db.initialize().unwrap();
    let old = TaskControl::new(
        db.clone(),
        "old-process".into(),
        vec![Box::new(LegacyCollection)],
    )
    .unwrap();
    let RequestOutcome::Task(original) = old
        .accept(
            "original",
            LegacyCollection.key(),
            json!({"symbols":["sh.600006"],"periods":["day","year"]}),
        )
        .unwrap()
        .record
        .outcome
    else {
        panic!()
    };
    // A pending item with a prior execution start is not covered by the
    // stopped-writes proof: its earlier write outcome is genuinely unknown,
    // so the check stays insufficient and the original claims are kept.
    let mut original = db
        .transition(
            &original.task.task_id,
            0,
            "old-process",
            x_data_mcp::task_control::Change::Claim,
        )
        .unwrap();
    original = db
        .transition(
            &original.task.task_id,
            original.task.state_revision,
            "old-process",
            x_data_mcp::task_control::Change::StartItem(original.items[0].spec.item_id.clone()),
        )
        .unwrap();
    original = db
        .transition(
            &original.task.task_id,
            original.task.state_revision,
            "old-process",
            x_data_mcp::task_control::Change::FinishItem(
                original.items[0].spec.item_id.clone(),
                x_data_mcp::task_control::ItemOutcome {
                    success: false,
                    error: Some("prior run failed".into()),
                    evidence: None,
                },
            ),
        )
        .unwrap();
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute(
            "INSERT INTO task_item_execution_starts VALUES(?1,?2,'old-process',1,'2026-09-13T00:00:00Z')",
            rusqlite::params![original.task.task_id, original.items[1].spec.item_id],
        )
        .unwrap();
    let original = Box::new(original);
    let mut s = Session::new(&path);
    s.request("initialize", json!({}));
    let cap = s.call(
        "get_data_capabilities",
        json!({"operation":"verify_stopped_writes"}),
        false,
    );
    let descriptor = &cap["items"][0];
    assert_eq!(descriptor["runtime_readiness"], "ready");
    assert_eq!(descriptor["blocking_reasons"], json!([]));
    let input = json!({"contract_version":"v1","request_id":"inspection","target_task_id":original.task.task_id});
    jsonschema::validator_for(&descriptor["input_schema"])
        .unwrap()
        .validate(&input)
        .unwrap();
    for field in ["stopped", "force", "evidence_ref", "actor", "approved"] {
        let mut bad = input.clone();
        bad[field] = json!(true);
        assert_eq!(
            s.call("verify_stopped_writes", bad, true)["code"],
            "invalid_arguments"
        );
        assert!(db.read_request("inspection").unwrap().is_none());
    }
    assert_eq!(
        s.call(
            "verify_stopped_writes",
            json!({"contract_version":"v1","request_id":"missing","target_task_id":"missing"}),
            true
        )["code"],
        "task_not_found"
    );
    let accepted = s.call("verify_stopped_writes", input.clone(), false);
    jsonschema::validator_for(&descriptor["output_schema"])
        .unwrap()
        .validate(&accepted)
        .unwrap();
    assert_eq!(accepted["blocking_reasons"], json!([]));
    assert_eq!(accepted["runtime_readiness"], "ready");
    assert_eq!(
        s.call("verify_stopped_writes", input, false)["task_id"],
        accepted["task_id"]
    );
    assert_eq!(
        s.call(
            "get_request",
            json!({"contract_version":"v1","request_id":"inspection"}),
            false
        )["task"]["task_id"],
        accepted["task_id"]
    );
    let busy=s.call("verify_stopped_writes",json!({"contract_version":"v1","request_id":"other","target_task_id":original.task.task_id}),true);
    assert_eq!(busy["code"], "range_conflict");
    assert!(db.read_request("other").unwrap().is_none());
    assert_eq!(
        s.call("run_worker_once", json!({}), false)["task_id"],
        accepted["task_id"]
    );
    let progress = s.call(
        "get_task_progress",
        json!({"task_id":accepted["task_id"]}),
        false,
    );
    assert_eq!(progress["task"]["status"], "succeeded");
    assert_eq!(
        progress["stopped_write_check"]["conclusion"],
        "insufficient_evidence"
    );
    assert_eq!(
        progress["stopped_write_check"]["target_task_id"],
        original.task.task_id
    );
    assert_eq!(
        db.read_task(&original.task.task_id).unwrap().unwrap(),
        *original
    );
    assert_eq!(
        s.call(
            "get_task",
            json!({"contract_version":"v1","task_id":original.task.task_id}),
            false
        )["status"],
        "needs_verification"
    );
    let check=s.call("verify_stopped_writes",json!({"contract_version":"v1","request_id":"cancel-inspection","target_task_id":original.task.task_id}),false);
    s.call(
        "cancel_task",
        json!({"contract_version":"v1","task_id":check["task_id"],"expected_revision":0}),
        false,
    );
    s.call("run_worker_once", json!({}), false);
    assert_eq!(
        s.call(
            "get_task",
            json!({"contract_version":"v1","task_id":check["task_id"]}),
            false
        )["status"],
        "cancelled"
    );
    assert_eq!(
        db.read_task(&original.task.task_id).unwrap().unwrap(),
        *original
    );
}

#[test]
fn real_stopping_stdio_reuses_a_verified_conclusion_as_a_bound_request_terminal() {
    use x_data_mcp::task_control::{
        Contract, RequestOutcome, TaskRepository, legacy::LegacyCollection, service::TaskControl,
    };
    use x_data_mcp::task_verification::{
        StopVerification,
        decision::{Observation, StopInspector, snapshot_digest},
        runner::StopRunner,
    };
    struct Fixture;
    impl StopInspector for Fixture {
        fn inspect(
            &self,
            s: &x_data_mcp::task_control::TaskSnapshot,
        ) -> Result<Observation, x_data_mcp::task_control::Error> {
            Ok(Observation::Stopped {
                snapshot_sha256: snapshot_digest(s)?,
                evidence_ref: "offline fixture: known writer".into(),
            })
        }
    }
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("db");
    let db = x_data_mcp::sqlite_tasks::SqliteTasks::new(&path);
    db.initialize().unwrap();
    let old = TaskControl::new(db.clone(), "old".into(), vec![Box::new(LegacyCollection)]).unwrap();
    let RequestOutcome::Task(original) = old
        .accept(
            "original",
            LegacyCollection.key(),
            json!({"symbols":["sh.600006"],"periods":["day"]}),
        )
        .unwrap()
        .record
        .outcome
    else {
        panic!()
    };
    let contract = StopVerification::new(db.clone(), "fixture-run".into()).unwrap();
    let key = contract.key();
    let control =
        TaskControl::new(db.clone(), "fixture-run".into(), vec![Box::new(contract)]).unwrap();
    let RequestOutcome::Task(check) = control
        .accept(
            "fixture-check",
            key,
            json!({"target_task_id":original.task.task_id}),
        )
        .unwrap()
        .record
        .outcome
    else {
        panic!()
    };
    StopRunner {
        repository: db.clone(),
        run_id: "fixture-run".into(),
        actor: "fixture-owner".into(),
    }
    .run(&check.task.task_id, &Fixture)
    .unwrap();
    let unchanged = db.read_task(&original.task.task_id).unwrap().unwrap();
    let mut s = Session::new(&path);
    s.request("initialize", json!({}));
    let input = json!({"contract_version":"v1","request_id":"read-known-result","target_task_id":original.task.task_id});
    let result = s.call("verify_stopped_writes", input.clone(), false);
    assert_eq!(result["bound"], true);
    assert!(result["task"].is_null());
    assert_eq!(result["terminal_result"]["kind"], "already_verified");
    assert_eq!(
        result["terminal_result"]["receipt"]["verification_task_id"],
        check.task.task_id
    );
    assert_eq!(s.call("verify_stopped_writes", input, false), result);
    assert_eq!(
        s.call(
            "get_request",
            json!({"contract_version":"v1","request_id":"read-known-result"}),
            false
        ),
        result
    );
    assert_eq!(s.call("verify_stopped_writes",json!({"contract_version":"v1","request_id":"read-known-result","target_task_id":"changed"}),true)["code"],"request_conflict");
    assert_eq!(
        db.read_task(&original.task.task_id).unwrap().unwrap(),
        unchanged
    );
    assert_eq!(
        rusqlite::Connection::open(&path)
            .unwrap()
            .query_row("SELECT count(*) FROM tasks", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        2
    );
}

#[test]
fn real_two_live_stdio_processes_fence_never_started_public_collection() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("db");
    x_data_mcp::sqlite_tasks::SqliteTasks::new(&path)
        .initialize()
        .unwrap();
    let mut old = Session::new(&path);
    let mut current = Session::new(&path);
    old.request("initialize", json!({}));
    current.request("initialize", json!({}));
    for (dataset, selection) in [
        (
            "market.daily",
            json!({"kind":"explicit","securities":[{"exchange":"sh","code":"600006"}]}),
        ),
        ("securities.catalog", json!({"scope":"current_sh_sz_a"})),
    ] {
        let original = old.call("start_collection", json!({"contract_version":"v1","request_id":dataset,"dataset":dataset,"selection":selection}), false);
        let check = current.call("verify_stopped_writes", json!({"contract_version":"v1","request_id":format!("check-{dataset}"),"target_task_id":original["task_id"]}), false);
        assert_eq!(
            current.call("run_worker_once", json!({}), false)["task_id"],
            check["task_id"]
        );
        let progress = current.call(
            "get_task_progress",
            json!({"task_id":check["task_id"]}),
            false,
        );
        assert_eq!(progress["stopped_write_check"]["conclusion"], "interrupted");
        // The original process is STILL alive. Its lack of an eligible task is
        // a committed lifecycle fact, not a PID/dead-process assumption.
        assert!(old.child.try_wait().unwrap().is_none());
        assert_eq!(old.call("run_worker_once", json!({}), false)["idle"], true);
        assert_eq!(
            old.call(
                "get_task",
                json!({"contract_version":"v1","task_id":original["task_id"]}),
                false
            )["status"],
            "interrupted"
        );
        let known = current.call("verify_stopped_writes", json!({"contract_version":"v1","request_id":format!("known-{dataset}"),"target_task_id":original["task_id"]}), false);
        assert_eq!(known["terminal_result"]["kind"], "already_verified");
        assert!(known["task"].is_null());
    }
}

#[test]
fn real_publication_stdio_inspects_fixed_evidence_without_changing_original_task() {
    use x_data_mcp::{
        market_storage::{MarketRepository, Publication, PublishIntent, Publisher},
        sqlite_market::SqliteMarket,
        sqlite_tasks::SqliteTasks,
        task_control::{
            Change, Contract, RequestOutcome, TaskRepository, legacy::LegacyCollection,
            service::TaskControl,
        },
    };
    for published in [false, true] {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("db");
        let db = SqliteTasks::new(&path);
        db.initialize().unwrap();
        let old = TaskControl::new(
            db.clone(),
            "old-publisher".into(),
            vec![Box::new(LegacyCollection)],
        )
        .unwrap();
        let RequestOutcome::Task(original) = old
            .accept(
                "original",
                LegacyCollection.key(),
                json!({"symbols":["sh.600006"],"periods":["day"]}),
            )
            .unwrap()
            .record
            .outcome
        else {
            panic!()
        };
        let original = db
            .transition(&original.task.task_id, 0, "old-publisher", Change::Claim)
            .unwrap();
        let original = db
            .transition(
                &original.task.task_id,
                original.task.state_revision,
                "old-publisher",
                Change::StartItem(original.items[0].spec.item_id.clone()),
            )
            .unwrap();
        let mut s = Session::new(&path);
        s.request("initialize", json!({}));
        let cap = s.call(
            "get_data_capabilities",
            json!({"operation":"verify_publication_result"}),
            false,
        );
        let descriptor = &cap["items"][0];
        assert_eq!(descriptor["runtime_readiness"], "ready");
        assert_eq!(cap["production_ready"], false);
        let input = json!({"contract_version":"v1","request_id":"publication-check","target_task_id":original.task.task_id});
        for field in [
            "published",
            "force",
            "evidence",
            "actor",
            "approved",
            "provider",
            "clock",
        ] {
            let mut bad = input.clone();
            bad[field] = json!(true);
            assert_eq!(
                s.call("verify_publication_result", bad, true)["code"],
                "invalid_arguments"
            );
            assert!(db.read_request("publication-check").unwrap().is_none());
        }
        assert_eq!(
            s.call(
                "verify_publication_result",
                json!({"contract_version":"v1","request_id":"missing","target_task_id":"missing"}),
                true
            )["code"],
            "task_not_found"
        );
        let check = s.call("verify_publication_result", input.clone(), false);
        jsonschema::validator_for(&descriptor["input_schema"])
            .unwrap()
            .validate(&input)
            .unwrap();
        jsonschema::validator_for(&descriptor["output_schema"])
            .unwrap()
            .validate(&check)
            .unwrap();
        assert_eq!(check["runtime_readiness"], "ready");
        assert_eq!(check["blocking_reasons"], json!([]));
        assert_eq!(
            s.call("verify_publication_result", input, false)["task_id"],
            check["task_id"]
        );
        assert_eq!(s.call("verify_stopped_writes", json!({"contract_version":"v1","request_id":"conflicting-stop","target_task_id":original.task.task_id}), true)["code"], "range_conflict");
        assert!(db.read_request("conflicting-stop").unwrap().is_none());
        if published {
            let records: Value =
                serde_json::from_str(include_str!("../fixtures/issue79/legacy-records.json"))
                    .unwrap();
            let market = SqliteMarket::new(db.clone());
            market.stage(&records[0]["record"]).unwrap();
            let plan = (Publisher {
                repository: market.clone(),
            })
            .prepare(Publication {
                publication_id: "fixture-publication".into(),
                version_id: records[0]["record"]["version_id"].as_str().unwrap().into(),
                task_id: original.task.task_id.clone(),
                item_id: original.items[0].spec.item_id.clone(),
                run_id: "old-publisher".into(),
                expected_revision: original.task.state_revision,
                intent: PublishIntent::Daily {
                    expected_head: None,
                },
            })
            .unwrap();
            market.commit(&plan).unwrap();
        }
        let unchanged = db.read_task(&original.task.task_id).unwrap().unwrap();
        assert_eq!(
            s.call("run_worker_once", json!({}), false)["task_id"],
            check["task_id"]
        );
        #[cfg(target_os = "linux")]
        {
            let registration = db
                .runtime_registration(check["task_id"].as_str().unwrap())
                .unwrap()
                .expect("actual stdio worker must atomically register its runtime");
            x_data_mcp::verification_runtime::try_acquire(db.path(), &registration.descriptor)
                .expect("worker response must follow release of its execution guard");
        }
        let progress = s.call(
            "get_task_progress",
            json!({"task_id":check["task_id"]}),
            false,
        );
        assert_eq!(
            progress["publication_check"]["conclusion"],
            if published {
                "all_recorded"
            } else {
                "insufficient_evidence"
            }
        );
        assert!(
            !progress["publication_check"]["reason"]
                .as_str()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            progress["publication_check"]["missing_receipt_item_ids"],
            if published {
                json!([])
            } else {
                json!([original.items[0].spec.item_id])
            }
        );
        assert_eq!(progress["stopped_write_check"], Value::Null);
        assert_eq!(progress["task"]["status"], "succeeded");
        let listed = s.call(
            "list_tasks",
            json!({"operation":"verify_publication_result"}),
            false,
        );
        assert_eq!(listed["items"].as_array().unwrap().len(), 1);
        assert_eq!(
            db.read_task(&original.task.task_id).unwrap().unwrap(),
            unchanged
        );
        if published {
            let known = json!({"contract_version":"v1","request_id":"known-publication","target_task_id":original.task.task_id});
            let first = s.call("verify_publication_result", known.clone(), false);
            assert_eq!(
                first["terminal_result"]["receipt"],
                progress["publication_check"]
            );
            assert_eq!(first["task"], Value::Null);
            jsonschema::validator_for(&descriptor["output_schema"])
                .unwrap()
                .validate(&first)
                .unwrap();
            assert_eq!(s.call("verify_publication_result", known, false), first);
            assert_eq!(
                s.call(
                    "get_request",
                    json!({"contract_version":"v1","request_id":"known-publication"}),
                    false
                ),
                first
            );
            rusqlite::Connection::open(&path)
                .unwrap()
                .execute("UPDATE publication_checks SET audit_sha256='bad'", [])
                .unwrap();
            assert_eq!(
                s.call(
                    "get_task_progress",
                    json!({"task_id":check["task_id"]}),
                    true
                )["code"],
                "needs_verification"
            );
            assert_eq!(
                s.call(
                    "get_request",
                    json!({"contract_version":"v1","request_id":"known-publication"}),
                    true
                )["code"],
                "needs_verification"
            );
        } else {
            let cancelled = s.call("verify_publication_result", json!({"contract_version":"v1","request_id":"cancel-check","target_task_id":original.task.task_id}), false);
            s.call("cancel_task", json!({"contract_version":"v1","task_id":cancelled["task_id"],"expected_revision":0}), false);
            s.call("run_worker_once", json!({}), false);
            assert_eq!(
                s.call(
                    "get_task",
                    json!({"contract_version":"v1","task_id":cancelled["task_id"]}),
                    false
                )["status"],
                "cancelled"
            );
            assert_eq!(
                db.read_task(&original.task.task_id).unwrap().unwrap(),
                unchanged
            );
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn real_bounded_inspection_stdio_replays_history_without_recursive_tasks() {
    use x_data_mcp::{
        sqlite_tasks::SqliteTasks,
        task_control::{
            Contract, RequestOutcome, TaskRepository, legacy::LegacyCollection,
            service::TaskControl,
        },
        task_verification::StopVerification,
    };
    let dir = tempfile::tempdir().unwrap();
    let db = SqliteTasks::new(dir.path().join("inspect.sqlite"));
    db.initialize().unwrap();
    let old = TaskControl::new(db.clone(), "old".into(), vec![Box::new(LegacyCollection)]).unwrap();
    let RequestOutcome::Task(original) = old
        .accept(
            "original",
            LegacyCollection.key(),
            json!({"symbols":["sh.600006"],"periods":["day"]}),
        )
        .unwrap()
        .record
        .outcome
    else {
        panic!()
    };
    let control = TaskControl::new(
        db.clone(),
        "check-run".into(),
        vec![Box::new(
            StopVerification::new(db.clone(), "check-run".into()).unwrap(),
        )],
    )
    .unwrap();
    let RequestOutcome::Task(check) = control
        .accept(
            "check",
            x_data_mcp::task_verification::command_key(),
            json!({"target_task_id":original.task.task_id}),
        )
        .unwrap()
        .record
        .outcome
    else {
        panic!()
    };
    let input =
        json!({"contract_version":"v1","request_id":"inspect","target_task_id":check.task.task_id});
    let mut s = Session::new(db.path());
    s.request("initialize", json!({}));
    let caps = s.call(
        "get_data_capabilities",
        json!({"operation":"inspect_verification_task"}),
        false,
    );
    assert_eq!(caps["production_ready"], false);
    assert_eq!(caps["items"][0]["runtime_readiness"], "ready");
    for key in [
        "force", "stopped", "approved", "provider", "clock", "actor", "evidence",
    ] {
        let mut bad = input.clone();
        bad[key] = json!(true);
        assert_eq!(
            s.call("inspect_verification_task", bad, true)["code"],
            "invalid_arguments"
        );
        assert!(db.read_request("inspect").unwrap().is_none());
    }
    let result = s.call("inspect_verification_task", input.clone(), false);
    jsonschema::validator_for(&caps["items"][0]["input_schema"])
        .unwrap()
        .validate(&input)
        .unwrap();
    jsonschema::validator_for(&caps["items"][0]["output_schema"])
        .unwrap()
        .validate(&result)
        .unwrap();
    assert_eq!(result["task"], Value::Null);
    assert_eq!(result["bound"], true);
    assert_eq!(
        result["terminal_result"]["receipt"]["conclusion"],
        "interrupted"
    );
    assert_eq!(
        s.call(
            "get_request",
            json!({"contract_version":"v1","request_id":"inspect"}),
            false
        ),
        result
    );
    assert_eq!(
        s.call(
            "get_task",
            json!({"contract_version":"v1","task_id":check.task.task_id}),
            false
        )["status"],
        "interrupted"
    );
    assert_eq!(
        db.read_task(&original.task.task_id).unwrap().as_ref(),
        Some(original.as_ref())
    );
    assert_eq!(s.call("inspect_verification_task",json!({"contract_version":"v1","request_id":"inspect","target_task_id":original.task.task_id}),true)["code"],"request_conflict");
    assert_eq!(
        s.call(
            "inspect_verification_task",
            json!({"contract_version":"v1","request_id":"missing","target_task_id":"absent"}),
            true
        )["code"],
        "task_not_found"
    );
    drop(s);
    let mut restarted = Session::new(db.path());
    restarted.request("initialize", json!({}));
    assert_eq!(
        restarted.call("inspect_verification_task", input, false),
        result
    );
    let count: i64 = rusqlite::Connection::open(db.path())
        .unwrap()
        .query_row("SELECT count(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2);
}

/// v8 PR-1 (#387 §7): the friendly spellings decode end to end over the real
/// stdio surface, the still-rejected cases keep an actionable message, and a
/// replayed `request_id` is the same request in both spellings (identical
/// idempotency digest). Response payloads are otherwise unchanged.
#[test]
fn real_data_stdio_v8_input_normalization_accepts_friendly_spellings() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("db");
    x_data_mcp::sqlite_tasks::SqliteTasks::new(&path)
        .initialize()
        .unwrap();
    let mut s = Session::new(&path);
    // #1 entity text, #2 version alias, #3 compact date, #5 omitted
    // contract_version: one read decodes (the database is empty, so the page
    // is empty rather than an error).
    let page = s.call(
        "query_data",
        json!({
            "dataset": "market.daily",
            "selection": {"kind": "single", "entity": "sh.600000"},
            "filter": {"kind": "daily", "start_date": "20200101", "end_date": "20200131"},
            "version": {"kind": "latest"},
        }),
        false,
    );
    assert_eq!(page["dataset"], "market.daily");
    assert!(page["records"].as_array().unwrap().is_empty());
    // #4: a missing range field names the field and both accepted formats.
    let err = s.call(
        "query_data",
        json!({
            "dataset": "market.daily",
            "selection": {"kind": "single", "entity": "sh.600000"},
            "filter": {"kind": "daily", "end_date": "2020-01-31"},
        }),
        true,
    );
    assert_eq!(err["code"], "invalid_arguments");
    assert!(err["message"].as_str().unwrap().contains("start_date"));
    assert!(err["message"].as_str().unwrap().contains("YYYYMMDD"));
    // #6: a cross-dataset selection lists the kinds this dataset allows.
    let err = s.call(
        "query_data",
        json!({
            "dataset": "market.daily",
            "selection": {"kind": "catalog", "scope": "current_sh_sz_a"},
            "filter": {"kind": "daily", "start_date": "2020-01-01", "end_date": "2020-01-31"},
        }),
        true,
    );
    assert_eq!(err["code"], "invalid_arguments");
    assert!(err["message"].as_str().unwrap().contains("\"single\""));
    // #7: an extra field is still rejected — normalization is not "accept
    // everything".
    let err = s.call(
        "query_data",
        json!({
            "dataset": "market.daily",
            "selection": {"kind": "single", "entity": "sh.600000"},
            "filter": {"kind": "daily", "start_date": "2020-01-01", "end_date": "2020-01-31"},
            "limit": 10,
        }),
        true,
    );
    assert_eq!(err["code"], "invalid_arguments");
    assert!(
        err["message"]
            .as_str()
            .unwrap()
            .contains("unknown field `limit`")
    );
    // #8: capabilities still has no contract_version and names its fields.
    let err = s.call(
        "get_data_capabilities",
        json!({"contract_version":"v1"}),
        true,
    );
    assert_eq!(err["code"], "invalid_arguments");
    assert!(err["message"].as_str().unwrap().contains("dataset"));
    assert!(err["message"].as_str().unwrap().contains("operation"));
    // #9: an unregistered capability operation keeps its own error.
    let err = s.call("get_data_capabilities", json!({"operation":"query"}), true);
    assert_eq!(err["code"], "unsupported_operation");
    // #10: the operation filter accepts what production tasks carry.
    let tasks = s.call("list_tasks", json!({"operation":"update_data"}), false);
    assert!(tasks["items"].as_array().unwrap().is_empty());
    let err = s.call("list_tasks", json!({"operation":"run_market_update"}), true);
    assert_eq!(err["code"], "invalid_arguments");
    // #11/#12: the missing task is still task_not_found — which also proves
    // the omitted contract_version decoded, for both task reads.
    for (name, args) in [
        ("get_task", json!({"task_id":"missing"})),
        (
            "get_task_progress",
            json!({"task_id":"missing","contract_version":"v1"}),
        ),
        ("cancel_task", json!({"task_id":"missing"})),
    ] {
        assert_eq!(s.call(name, args, true)["code"], "task_not_found", "{name}");
    }
    // #13: the display name in `code` is still rejected with routing advice.
    let err = s.call(
        "query_data",
        json!({
            "dataset": "market.daily",
            "selection": {"kind": "single", "entity": {"exchange": "sh", "code": "sh.600000"}},
            "filter": {"kind": "daily", "start_date": "2020-01-01", "end_date": "2020-01-31"},
        }),
        true,
    );
    assert_eq!(err["code"], "invalid_arguments");
    assert!(err["message"].as_str().unwrap().contains("six digits"));
    // #14: market reads require a range; catalog reads default their filter.
    let err = s.call(
        "query_data",
        json!({"dataset": "market.daily", "selection": {"kind": "single", "entity": "sh.600000"}}),
        true,
    );
    assert_eq!(err["code"], "invalid_arguments");
    assert!(
        err["message"]
            .as_str()
            .unwrap()
            .contains("filter.kind=daily")
    );
    let catalog = s.call(
        "query_data",
        json!({
            "dataset": "securities.catalog",
            "selection": {"kind": "catalog", "scope": "current_sh_sz_a"},
        }),
        false,
    );
    assert_eq!(catalog["dataset"], "securities.catalog");
    // AC-V8-11: M2 point-in-time parameters are reserved, not ignored.
    let err = s.call(
        "query_data",
        json!({
            "dataset": "market.daily",
            "selection": {"kind": "single", "entity": "sh.600000"},
            "filter": {"kind": "daily", "start_date": "2020-01-01", "end_date": "2020-01-31"},
            "known_at": "2020-01-31",
        }),
        true,
    );
    assert_eq!(err["code"], "reserved_for_m2");
    // §7.3: the same request_id in the friendly and the contract spelling is
    // one request; the replay is `reused`, so the digest matched byte for
    // byte.
    let first = s.call(
        "start_collection",
        json!({
            "request_id": "v8-pr1-friendly",
            "dataset": "market.daily",
            "selection": {"kind": "explicit", "securities": ["sh.600000"]},
        }),
        false,
    );
    assert_eq!(first["reused"], false);
    let replay = s.call(
        "start_collection",
        json!({
            "request_id": "v8-pr1-friendly",
            "dataset": "market.daily",
            "contract_version": "v1",
            "selection": {"kind": "explicit", "securities": [{"exchange": "sh", "code": "600000"}]},
        }),
        false,
    );
    assert_eq!(replay["reused"], true);
    assert_eq!(replay["task_id"], first["task_id"]);
}
