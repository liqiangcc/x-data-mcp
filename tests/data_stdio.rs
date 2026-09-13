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
    assert_eq!(tools.len(), if cfg!(target_os = "linux") { 18 } else { 17 });
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
            .contains("never downloads or publishes")
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
                | x_data_mcp::data_supply::registry::Handler::Collection
                | x_data_mcp::data_supply::registry::Handler::Range(_)
                | x_data_mcp::data_supply::registry::Handler::Retry
                | x_data_mcp::data_supply::registry::Handler::Task
                | x_data_mcp::data_supply::registry::Handler::Cancel
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
    let request = s.call("get_request", json!({"request_id":"r"}), false);
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
    s.call("cancel_task", json!({"task_id":a["task_id"]}), false);
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
    assert!(rev > 0);
    assert_eq!(
        s.call(
            "cancel_task",
            json!({"contract_version":"v1","task_id":a["task_id"],"expected_revision":rev-1}),
            true
        )["code"],
        "precondition_conflict"
    );
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

#[path = "support/market.rs"]
mod range_fixture;
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
        assert_eq!(cap["readiness"], "blocked");
        assert!(
            cap["description"]
                .as_str()
                .unwrap()
                .contains("Explicit market.daily")
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
        assert_eq!(accepted["readiness"], "blocked");
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
        assert_eq!(caps["items"][0]["readiness"], "blocked");
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
        assert_eq!(accepted["readiness"], "blocked");
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
    // A claimed old task is not covered by the never-started proof.
    let original = Box::new(
        db.transition(
            &original.task.task_id,
            0,
            "old-process",
            x_data_mcp::task_control::Change::Claim,
        )
        .unwrap(),
    );
    let mut s = Session::new(&path);
    s.request("initialize", json!({}));
    let cap = s.call(
        "get_data_capabilities",
        json!({"operation":"verify_stopped_writes"}),
        false,
    );
    let descriptor = &cap["items"][0];
    assert_eq!(descriptor["readiness"], "blocked");
    assert_eq!(
        descriptor["blocking_reasons"],
        json!(["STOP_EVIDENCE_NOT_VERIFIED"])
    );
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
    assert_eq!(
        accepted["blocking_reasons"],
        json!(["STOP_EVIDENCE_NOT_VERIFIED"])
    );
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
        assert_eq!(descriptor["readiness"], "ready");
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
        assert_eq!(check["readiness"], "ready");
        assert_eq!(check["blocking_reasons"], json!([]));
        assert_eq!(
            s.call("verify_publication_result", input, false)["task_id"],
            check["task_id"]
        );
        assert_eq!(s.call("verify_stopped_writes", json!({"contract_version":"v1","request_id":"conflicting-stop","target_task_id":original.task.task_id}), true)["code"], "range_conflict");
        assert!(db.read_request("conflicting-stop").unwrap().is_none());
        if published {
            let records: Value =
                serde_json::from_str(include_str!("fixtures/issue79/legacy-records.json")).unwrap();
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
    assert_eq!(caps["items"][0]["readiness"], "ready");
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
