//! v8 PR-2 (#387 §3.3/§6.2/§8.4): entry closure, tools/list budgets and the
//! single registry source. These assertions read only the registry and the
//! capability catalog; they need no published data.
use serde_json::{Value, json};
use std::collections::BTreeSet;
use x_data_mcp::{
    data_supply::{
        Access,
        mcp::DataApi,
        registry::{self, EntryKind, Handler},
    },
    mcp::ToolHandler,
    sqlite_tasks::SqliteTasks,
};

fn api(entry: EntryKind) -> (DataApi, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let api = DataApi::new(
        SqliteTasks::new(dir.path().join("db")),
        "v8-surface".into(),
        Access::local(),
    )
    .unwrap()
    .with_entry(entry);
    (api, dir)
}

fn names(tools: &Value) -> BTreeSet<String> {
    tools
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn entry_allowlists_cover_every_registered_tool_and_research_is_read_only() {
    let registered: BTreeSet<&str> = registry::entries().iter().map(|entry| entry.name).collect();
    let mut union = BTreeSet::new();
    for entry in [EntryKind::Research, EntryKind::Ops] {
        entry.validate().unwrap();
        for name in entry.allowlist().unwrap() {
            assert!(registered.contains(name), "{name}");
            union.insert(*name);
        }
    }
    #[cfg(target_os = "linux")]
    {
        EntryKind::Maint.validate().unwrap();
        for name in EntryKind::Maint.allowlist().unwrap() {
            assert!(registered.contains(name), "{name}");
            union.insert(*name);
        }
    }
    assert_eq!(union, registered, "every registered tool has an entry");

    assert_eq!(
        registry::RESEARCH_TOOLS.to_vec(),
        vec![
            "get_data_capabilities",
            "query_data",
            "get_data_availability",
            "list_data_versions",
            "list_catalog_facets",
        ]
    );
    // Bidirectional closure: every research tool is registered, and no
    // registered tool that writes, verifies, collects or controls tasks is in
    // the research allowlist.
    for entry in registry::entries() {
        if !registry::RESEARCH_TOOLS.contains(&entry.name) {
            continue;
        }
        assert!(
            !entry.requires_local(),
            "research tool {} requires local control",
            entry.name
        );
        assert!(
            !matches!(
                entry.handler,
                Handler::Collection
                    | Handler::Range(_)
                    | Handler::Republish
                    | Handler::Retry
                    | Handler::Task
                    | Handler::Request
                    | Handler::Cancel
                    | Handler::Worker
                    | Handler::VerifyStopped
                    | Handler::VerifyPublication
                    | Handler::InspectVerification
                    | Handler::VerifyGap
                    | Handler::PlanMarketUpdate
                    | Handler::RunMarketUpdate
            ),
            "research tool {} has a side-effecting handler",
            entry.name
        );
    }
}

#[test]
fn tools_list_stays_within_entry_budgets_without_output_schema() {
    let budgets = [
        (EntryKind::Research, 5usize, 11_000usize),
        (EntryKind::Ops, 15usize, 27_500usize),
        (
            EntryKind::Maint,
            if cfg!(target_os = "linux") { 9 } else { 8 },
            14_500usize,
        ),
    ];
    for (entry, expected, budget) in budgets {
        let (api, _dir) = api(entry);
        let tools = api.tools();
        let listed = tools.as_array().unwrap();
        assert_eq!(listed.len(), expected, "{}", entry.name());
        for tool in listed {
            let name = tool["name"].as_str().unwrap();
            assert!(tool.get("outputSchema").is_none(), "outputSchema {name}");
            assert!(
                tool["description"].as_str().unwrap().len() <= 240,
                "description {name} is {} B",
                tool["description"].as_str().unwrap().len()
            );
            assert_eq!(tool["inputSchema"]["type"], json!("object"), "input {name}");
        }
        let bytes = serde_json::to_vec(&tools).unwrap().len();
        assert!(
            bytes <= budget,
            "{} tools/list is {bytes} B (budget {budget}); tools: {:?}",
            entry.name(),
            listed
                .iter()
                .map(|tool| tool["name"].as_str().unwrap())
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn capabilities_follow_the_entry_and_match_the_tools_list() {
    for entry in [EntryKind::Research, EntryKind::Ops] {
        let (api, _dir) = api(entry);
        let tools = api.tools();
        let caps = api.call("get_data_capabilities", json!({})).unwrap();
        let catalog: BTreeSet<String> = caps["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["tool_name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(catalog, names(&tools), "{}", entry.name());
        for item in caps["items"].as_array().unwrap() {
            assert_eq!(item["entry"], entry.name());
            // Compact catalog: no detailed-only field.
            assert!(item.get("description").is_none(), "{item}");
            assert!(item.get("input_schema").is_none(), "{item}");
            assert!(item.get("output_schema").is_none(), "{item}");
            assert!(item.get("runtime_readiness").is_none(), "{item}");
        }
    }

    let (research, _dir) = api(EntryKind::Research);
    let compact = research.call("get_data_capabilities", json!({})).unwrap();
    assert!(
        serde_json::to_vec(&compact).unwrap().len() <= 1500,
        "research compact catalog is {} B",
        serde_json::to_vec(&compact).unwrap().len()
    );

    // Detailed view (dataset + operation) is byte-identical to the registry.
    let detail = research
        .call(
            "get_data_capabilities",
            json!({"dataset":"market.daily","operation":"query_data"}),
        )
        .unwrap();
    let item = &detail["items"][0];
    let entry = registry::entries()
        .into_iter()
        .find(|entry| entry.name == "query_data")
        .unwrap();
    assert_eq!(
        item["input_schema"],
        x_data_mcp::mcp::ensure_object_root(entry.input)
    );
    assert_eq!(
        item["output_schema"],
        x_data_mcp::mcp::ensure_object_root(entry.output)
    );
}

#[test]
fn research_entry_hides_control_tools_as_unknown() {
    let (api, _dir) = api(EntryKind::Research);
    for name in [
        "plan_market_update",
        "run_market_update",
        "start_collection",
        "update_data",
        "backfill_data",
        "republish_data",
        "retry_task",
        "cancel_task",
        "get_task",
        "get_task_progress",
        "list_tasks",
        "get_request",
        "get_yearly_base_years",
        "verify_publication_result",
        "verify_stopped_writes",
        "inspect_verification_task",
        "verify_data_gap",
        "run_worker_once",
        "compute_metrics",
    ] {
        let error = api.call(name, json!({})).unwrap_err();
        assert_eq!(error.code, "unknown_tool", "{name}");
    }
    assert_eq!(
        api.call("get_task", json!({"contract_version":"v1","task_id":"x"}))
            .unwrap_err()
            .code,
        "unknown_tool"
    );
}

#[test]
fn shared_tools_are_byte_identical_across_entries() {
    let (research, _r) = api(EntryKind::Research);
    let (ops, _o) = api(EntryKind::Ops);
    let pick = |tools: Value, name: &str| -> Value {
        tools
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap()
            .clone()
    };
    for name in ["query_data", "get_data_availability", "list_data_versions"] {
        assert_eq!(
            pick(research.tools(), name),
            pick(ops.tools(), name),
            "{name}"
        );
    }
}

#[test]
fn entry_names_parse_and_the_dev_surface_is_not_deployable() {
    assert_eq!(EntryKind::parse("research"), Some(EntryKind::Research));
    assert_eq!(EntryKind::parse("ops"), Some(EntryKind::Ops));
    assert_eq!(EntryKind::parse("maint"), Some(EntryKind::Maint));
    assert_eq!(EntryKind::parse("all"), None);
    assert_eq!(EntryKind::parse(""), None);
    assert_eq!(EntryKind::All.name(), "all");
    assert!(EntryKind::All.allows("not_a_tool"));
}
