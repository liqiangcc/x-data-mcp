#!/usr/bin/env python3
"""Exercise installed binaries against an explicitly synthetic NEW database."""
import argparse
import hashlib
import json
import pathlib
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--bin-dir", type=pathlib.Path, required=True)
    parser.add_argument("--report", type=pathlib.Path, required=True)
    args = parser.parse_args()
    binaries = args.bin_dir.resolve()
    version = subprocess.check_output([binaries / "mcp_data", "--version"], text=True).strip()
    checks = []
    with tempfile.TemporaryDirectory(prefix="x-data-mcp-smoke-") as tmp:
        db = pathlib.Path(tmp) / "fixture.sqlite"
        fixture = json.loads(subprocess.check_output([binaries / "create_demo", db], text=True))
        assert fixture["status"] == "FIXTURE_ONLY"
        assert subprocess.run([binaries / "create_demo", db], capture_output=True).returncode != 0
        checks.append("explicit_fixture_init_and_no_clobber")
        child = subprocess.Popen([binaries / "mcp_data", db], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        sequence = 0
        def request(method, params):
            nonlocal sequence
            sequence += 1
            child.stdin.write(json.dumps(dict(jsonrpc="2.0", id=sequence, method=method, params=params)) + "\n")
            child.stdin.flush()
            result = json.loads(child.stdout.readline())
            assert result["id"] == sequence
            return result["result"]
        def call(name, arguments, error=False):
            result = request("tools/call", dict(name=name, arguments=arguments))
            assert result["isError"] == error, result
            assert json.loads(result["content"][0]["text"]) == result["structuredContent"]
            return result["structuredContent"]
        try:
            assert request("initialize", {})["serverInfo"]["name"] == "x-data-mcp"
            tools = request("tools/list", {})["tools"]
            assert len(tools) == 16
            assert call("get_data_capabilities", {})["production_ready"] is False
            checks.append("initialize_discovery_16_tools")
            catalog = call("query_data", dict(dataset="securities.catalog", contract_version="v1", selection=dict(kind="catalog", scope="current_sh_sz_a"), filter=dict(kind="catalog"), version=dict(kind="latest_per_entity")))
            entity = catalog["records"][0]["entity"]
            assert catalog["records"][0]["industry"]["status"] == "missing"
            query = dict(dataset="market.yearly", contract_version="v1", selection=dict(kind="single", entity=entity), filter=dict(kind="yearly", start_year=2020, end_year=2023), version=dict(kind="latest_per_entity"), page_size=2)
            first = call("query_data", query)
            query["cursor"] = first["next_cursor"]
            second = call("query_data", query)
            assert first["version_manifest"] == second["version_manifest"] and second["page_complete"]
            assert len(first["records"]) + len(second["records"]) == 4
            checks.append("catalog_to_fixed_yearly_paging")
            computation = dict(contract_version="v1", execution="inline", inputs=[dict(input_id="prices", version_ref=fixture["version_ref"])], range=dict(start_year=2020, end_year=2023), metrics=[dict(metric_id="down", name="consecutive_direction", algorithm_version="local-consecutive-direction-v1", input_ids=["prices"], parameters=dict(direction="down", min_periods=3))])
            computed = call("compute_metrics", computation)
            assert computed["result"]["cumulative_change"] == "-0.300000000000"
            assert computed == call("compute_metrics", computation)
            checks.append("fixed_compute_deterministic_fixture_vector")
            accepted = call("start_collection", dict(request_id="smoke-write", dataset="market.daily", contract_version="v1", selection=dict(kind="explicit", securities=[entity])))
            assert accepted["readiness"] == "blocked"
            assert call("get_request", dict(contract_version="v1", request_id="smoke-write"))["task"]["task_id"] == accepted["task_id"]
            call("run_worker_once", {})
            progress = call("get_task_progress", dict(task_id=accepted["task_id"]))
            assert progress["failed"] == 1 and "SOURCE_NOT_VERIFIED" in progress["unfinished"][0]["error"]
            retry = call("retry_task", dict(contract_version="v1", original_task_id=accepted["task_id"], new_request_id="smoke-retry", item_ids=accepted["item_ids"]))
            view = call("get_task", dict(contract_version="v1", task_id=retry["task_id"]))
            call("cancel_task", dict(contract_version="v1", task_id=retry["task_id"], expected_revision=view["state_revision"]))
            checks.append("request_task_retry_cancel_and_source_gate")
            call("run_worker_once", {})
            yearly_caps = call("get_data_capabilities", dict(operation="update_data", dataset="market.yearly"))
            assert len(yearly_caps["items"]) == 1 and yearly_caps["items"][0]["readiness"] == "blocked"
            assert yearly_caps["items"][0]["example"]["items"][0]["publication"]["mode"] == "historical_fixed_base"
            yearly_input = dict(request_id="smoke-yearly-update", dataset="market.yearly", contract_version="v1", items=[dict(entity=entity, base_daily_version_id="demo-daily", years=[2020], publication=dict(mode="historical_fixed_base", comparison_version_id=None))])
            yearly_task = call("update_data", yearly_input)
            assert call("update_data", yearly_input)["task_id"] == yearly_task["task_id"]
            assert call("run_worker_once", {})["task_id"] == yearly_task["task_id"]
            yearly_progress = call("get_task_progress", dict(task_id=yearly_task["task_id"]))
            assert yearly_progress["failed"] == 1 and yearly_progress["yearly_results"] == []
            assert yearly_progress["frozen_yearly_inputs"][0]["years"] == [2020]
            assert "SOURCE_NOT_VERIFIED" in yearly_progress["unfinished"][0]["error"]
            computed_after = call("compute_metrics", computation)
            assert computed_after == computed
            checks.append("yearly_update_frozen_input_and_blocked_worker_preserves_fixture")
            original = call("start_collection", dict(request_id="smoke-stopping-target", dataset="market.daily", contract_version="v1", selection=dict(kind="explicit", securities=[entity])))
            child.stdin.close()
            assert child.wait(timeout=20) == 0
            child = subprocess.Popen([binaries / "mcp_data", db], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
            request("initialize", {})
            check = call("verify_stopped_writes", dict(contract_version="v1", request_id="smoke-stopping-check", target_task_id=original["task_id"]))
            assert check["blocking_reasons"] == ["STOP_EVIDENCE_NOT_VERIFIED"]
            assert call("run_worker_once", {})["task_id"] == check["task_id"]
            checked = call("get_task_progress", dict(task_id=check["task_id"]))
            assert checked["stopped_write_check"]["conclusion"] == "interrupted"
            assert call("get_task", dict(contract_version="v1", task_id=original["task_id"]))["status"] == "interrupted"
            checks.append("stopping_inspection_proves_never_started_without_source_execution")
        finally:
            child.stdin.close()
            if child.wait(timeout=20) != 0:
                raise RuntimeError("stdio process exited unsuccessfully")
    report = dict(status="PASS", version=version, binary_sha256=hashlib.sha256((binaries / "mcp_data").read_bytes()).hexdigest(), checks=checks, live_source="NOT_RUN", production="NOT_PRODUCTION_READY", data="SYNTHETIC_FIXTURE_ONLY")
    args.report.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(report, ensure_ascii=False))

if __name__ == "__main__":
    main()
