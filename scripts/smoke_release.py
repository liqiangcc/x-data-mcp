#!/usr/bin/env python3
"""Exercise installed binaries against an explicitly synthetic NEW database."""
import argparse
import hashlib
import importlib.util
import json
import pathlib
import subprocess
import sys
import tempfile
import socket
import time
import select


def runtime_smoke(binaries):
    """Actual packaged service/proxy, isolated source-refusal data only."""
    with tempfile.TemporaryDirectory(prefix="x-data-mcp-runtime-smoke-") as tmp:
        root = pathlib.Path(tmp)
        db, endpoint = root / "db.sqlite", root / "mcp.sock"
        subprocess.run([binaries / "task_sqlite", "init", db], check=True, capture_output=True)
        db.chmod(0o600)
        service = subprocess.Popen([binaries / "data_service", db, endpoint], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
        client = None
        def rpc(method, params):
            client.stdin.write(json.dumps(dict(jsonrpc="2.0", id=1, method=method, params=params)) + "\n")
            client.stdin.flush()
            assert select.select([client.stdout], [], [], 10)[0], "IPC response deadline"
            response = json.loads(client.stdout.readline())
            assert response["id"] == 1 and "error" not in response, response
            return response["result"]
        def call(name, arguments):
            result = rpc("tools/call", dict(name=name, arguments=arguments))
            assert not result["isError"], result
            return result["structuredContent"]
        try:
            deadline = time.monotonic() + 10
            while True:
                assert service.poll() is None, service.stderr.read()
                with socket.socket(socket.AF_UNIX) as probe:
                    try:
                        probe.connect(str(endpoint))
                        break
                    except (FileNotFoundError, ConnectionRefusedError):
                        assert time.monotonic() < deadline, "service startup deadline"
                        time.sleep(0.02)
            client = subprocess.Popen([binaries / "mcp_connect", endpoint], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
            assert rpc("initialize", {})["serverInfo"]["name"] == "x-data-mcp"
            tools = rpc("tools/list", {})["tools"]
            cap = call("get_data_capabilities", {})
            assert len(tools) == 15 and not cap["production_ready"]
            assert cap["capability_revision"] == "generic-data-v8"
            # Keep the same actual proxy stdin open beyond the default 30s
            # read budget. This caught the preview.5 deployment regression.
            time.sleep(32)
            assert client.poll() is None, "idle MCP proxy exited while stdin remained open"
            assert call("get_data_capabilities", {})["production_ready"] is False
            assert {t["name"] for t in tools} == {c["tool_name"] for c in cap["items"]}
            args = dict(request_id="runtime-smoke", dataset="market.daily", contract_version="v1", selection=dict(kind="explicit", securities=[dict(exchange="sh", code="600000")]))
            accepted = call("start_collection", args)
            client.stdin.close()
            assert client.wait(timeout=10) == 0
            client = subprocess.Popen([binaries / "mcp_connect", endpoint], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
            deadline = time.monotonic() + 10
            while True:
                task = call("get_task", dict(contract_version="v1", task_id=accepted["task_id"]))
                if task["status"] == "failed":
                    break
                assert time.monotonic() < deadline, task
                time.sleep(0.02)
            progress = call("get_task_progress", dict(task_id=accepted["task_id"]))
            assert "SOURCE_NOT_VERIFIED" in json.dumps(progress)
            replay = call("start_collection", args)
            assert replay["reused"] and replay["task_id"] == accepted["task_id"]
        finally:
            if client is not None:
                if not client.stdin.closed:
                    client.stdin.close()
                try:
                    client.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    client.kill()
                    client.wait()
            service.terminate()
            try:
                status = service.wait(timeout=15)
            except subprocess.TimeoutExpired:
                service.kill()
                service.wait()
                raise
            assert status == 0, service.stderr.read()


def acquire_smoke(binaries):
    """Packaged acquire service: generation store + owner-only socket."""
    with tempfile.TemporaryDirectory(prefix="x-data-mcp-acquire-smoke-") as tmp:
        root = pathlib.Path(tmp)
        generations, endpoint = root / "generations", root / "acquire.sock"
        service = subprocess.Popen([binaries / "acquire_service", generations, endpoint], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
        client = None
        def rpc(method, params):
            client.stdin.write(json.dumps(dict(jsonrpc="2.0", id=1, method=method, params=params)) + "\n")
            client.stdin.flush()
            assert select.select([client.stdout], [], [], 10)[0], "IPC response deadline"
            response = json.loads(client.stdout.readline())
            assert response["id"] == 1 and "error" not in response, response
            return response["result"]
        try:
            deadline = time.monotonic() + 10
            while True:
                assert service.poll() is None, service.stderr.read()
                with socket.socket(socket.AF_UNIX) as probe:
                    try:
                        probe.connect(str(endpoint))
                        break
                    except (FileNotFoundError, ConnectionRefusedError):
                        assert time.monotonic() < deadline, "acquire service startup deadline"
                        time.sleep(0.02)
            client = subprocess.Popen([binaries / "mcp_connect", endpoint], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
            assert rpc("initialize", {})["serverInfo"]["name"] == "x-data-mcp"
            tools = rpc("tools/list", {})["tools"]
            names = {t["name"] for t in tools}
            assert names == {"check_official_updates", "acquire_generation", "get_acquisition_task",
                             "get_acquisition_request", "verify_generation", "activate_generation",
                             "retire_generation", "sync_status", "run_worker_once",
                             "get_acquisition_capabilities"}, names
            cap = rpc("tools/call", dict(name="get_acquisition_capabilities",
                                       arguments={"contract_version": "v1"}))
            assert not cap["isError"], cap
            client.stdin.close()
            assert client.wait(timeout=10) == 0
        finally:
            if client is not None and client.poll() is None:
                client.kill()
                client.wait()
            service.terminate()
            try:
                service.wait(timeout=15)
            except subprocess.TimeoutExpired:
                service.kill()
                service.wait()


def export_fixture(db):
    """Minimal schema-23 database the packaged exporter can read.

    Only the rows the export plan needs: one source generation, one stock
    daily series in both 口径, and three bars. The fixture is synthetic and
    lives in the smoke's temporary directory; it is never a real market
    database. The schema itself comes from the packaged `task_sqlite init21`,
    so the smoke exercises the schema the release actually ships.
    """
    import sqlite3
    from datetime import date

    epoch = date(1970, 1, 1)
    days = [(date(2026, 10, 20) - epoch).days + offset for offset in range(3)]
    generation_evidence = bytes([0x21]) * 32
    version_evidence = bytes([0x22]) * 32
    binding = bytes([0x23]) * 32
    raw_digest, qfq_digest = bytes([0x25]) * 32, bytes([0x26]) * 32
    created_at = "2026-10-26T00:00:00Z"
    connection = sqlite3.connect(db)
    try:
        connection.execute("PRAGMA foreign_keys=ON")
        connection.execute(
            "INSERT INTO evidence_bundles(evidence_digest,evidence_kind,canonical_json) VALUES(?,?,?)",
            (generation_evidence, "generation", '{"generation_id":"smoke-generation"}'),
        )
        connection.execute(
            "INSERT INTO evidence_bundles(evidence_digest,evidence_kind,canonical_json) VALUES(?,?,?)",
            (version_evidence, "version", "{}"),
        )
        connection.execute(
            "INSERT INTO source_bindings(source_binding_digest,generation_id,"
            "generation_manifest_sha256,day_member_sha256,security_event_sha256,"
            "evidence_digest) VALUES(?,?,?,?,?,?)",
            (
                binding,
                "smoke-generation",
                bytes([0x24]) * 32,
                bytes([0x27]) * 32,
                bytes([0x28]) * 32,
                generation_evidence,
            ),
        )
        # adjustment 0 = raw (2dp), 1 = qfq (6dp); both are stock daily series.
        for series_key, adjustment, price_scale, digest in (
            (1, 0, 2, raw_digest),
            (2, 1, 6, qfq_digest),
        ):
            connection.execute(
                "INSERT INTO market_series(series_key,entity,period,adjustment,"
                "instrument_class,price_scale,version_id,content_digest,digest_format,"
                "record_count,first_day,last_day,coverage_status,source_binding_digest,"
                "evidence_digest,last_event_seq,created_at,updated_at) "
                "VALUES(?,?,1,?,0,?,?,?,'series-fold-v2',3,?,?,1,?,?,1,?,?)",
                (
                    series_key,
                    "sh.600000",
                    adjustment,
                    price_scale,
                    digest.hex(),
                    digest,
                    days[0],
                    days[-1],
                    binding,
                    version_evidence,
                    created_at,
                    created_at,
                ),
            )
            for index, trade_day in enumerate(days):
                close = 1000 + index
                connection.execute(
                    "INSERT INTO series_bars(series_key,trade_day,open_units,high_units,"
                    "low_units,close_units,volume,amount,r7_raw,volume_quantum) "
                    "VALUES(?,?,?,?,?,?,100,'100500.00',7,NULL)",
                    (series_key, trade_day, close - 10, close + 10, close - 20, close),
                )
        connection.commit()
        connection.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    finally:
        connection.close()


def packaged_script(name):
    """Locate a Python helper both in the repo and in the unpacked package."""
    here = pathlib.Path(__file__).resolve()
    for candidate in (
        here.parent / name,
        here.parent / "scripts" / name,
        here.parent.parent / "scripts" / name,
    ):
        if candidate.is_file():
            return candidate
    return None


def export_smoke(binaries):
    """Run the packaged exporter on a synthetic schema-23 database.

    Verifies the parts a package can verify offline with the standard library:
    the command exists, it produces a snapshot directory, and every file's
    size and sha256 match `manifest.json`. The pyarrow/duckdb read-back needs
    third-party wheels, which the package CI job does not install; when they
    are importable this runs `scripts/export_smoke.py --snapshot-dir` too and
    reports it, otherwise the caller records it as NOT_RUN.
    """
    with tempfile.TemporaryDirectory(prefix="x-data-mcp-export-smoke-") as tmp:
        root = pathlib.Path(tmp)
        db, out = root / "export.sqlite", root / "exports"
        subprocess.run([binaries / "task_sqlite", "init21", db], check=True, capture_output=True, text=True)
        export_fixture(db)
        run = subprocess.run(
            [
                binaries / "export_research",
                "--db", db,
                "--out", out,
                "--dataset", "daily",
                "--entities", "sh.600000",
            ],
            check=True,
            capture_output=True,
            text=True,
        )
        summary = json.loads(run.stdout)
        directory = pathlib.Path(summary["directory"])
        manifest = json.loads((directory / "manifest.json").read_text())
        assert manifest["schema"] == "x-data-mcp.research-snapshot.v2", manifest["schema"]
        assert manifest["snapshot_id"] == summary["snapshot_id"] == directory.name, summary
        files = {entry["path"]: entry for entry in manifest["files"]}
        assert set(files) == {"daily_raw.parquet", "daily_qfq.parquet"}, files
        total = 0
        for name, entry in files.items():
            data = (directory / name).read_bytes()
            assert entry["rows"] == 3 and entry["bytes"] == len(data), (name, entry, len(data))
            assert entry["sha256"] == hashlib.sha256(data).hexdigest(), name
            total += len(data)
        assert total == summary["total_bytes"], (total, summary["total_bytes"])
        assert {entry["path"]: entry["sha256"] for entry in summary["files"]} == {
            name: entry["sha256"] for name, entry in files.items()
        }
        readers = "NOT_RUN (pyarrow/duckdb are installed only in the export CI job; " \
                  "scripts/export_smoke.py --snapshot-dir covers the read-back there)"
        if importlib.util.find_spec("pyarrow") and importlib.util.find_spec("duckdb"):
            script = packaged_script("export_smoke.py")
            if script is not None:
                subprocess.run(
                    [sys.executable, script, "--snapshot-dir", directory],
                    check=True,
                    capture_output=True,
                    text=True,
                )
                readers = "pyarrow+duckdb"
        return readers


def package_manifest_smoke(binaries):
    """The package manifest must say the export feature is on.

    Only runs when `manifest.json` sits next to the bin directory (the
    unpacked package); the repo's `target/release` has none.
    """
    manifest_path = binaries.parent / "manifest.json"
    if not manifest_path.is_file():
        return False
    manifest = json.loads(manifest_path.read_text())
    assert "export" in manifest.get("features", []), manifest.get("features")
    assert manifest["binaries"]["export_research"]["bytes"] == (
        binaries / "export_research"
    ).stat().st_size, manifest["binaries"]["export_research"]
    return True


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
            assert len(tools) == 24
            capabilities = call("get_data_capabilities", {})
            assert capabilities["production_ready"] is False
            assert capabilities["capability_revision"] == "generic-data-v8"
            checks.append("initialize_discovery_24_tools_generic_data_v8")
            catalog = call("query_data", dict(dataset="securities.catalog", contract_version="v1", selection=dict(kind="catalog", scope="current_sh_sz_a"), filter=dict(kind="catalog"), version=dict(kind="latest_per_entity")))
            entity = catalog["records"][0]["entity"]
            assert catalog["records"][0]["industry"]["status"] == "missing"
            assert catalog["version_manifest"][0]["metadata"]["adjustment"] is None
            query = dict(dataset="market.yearly", contract_version="v1", selection=dict(kind="single", entity=entity), filter=dict(kind="yearly", start_year=2020, end_year=2023), version=dict(kind="latest_per_entity"), page_size=2)
            first = call("query_data", query)
            adjustment = first["version_manifest"][0]["metadata"]["adjustment"]
            assert adjustment["field"] == "price_adjustment"
            assert adjustment["status"] == "confirmed"
            assert adjustment["value"] == "qfq"
            assert adjustment["reason"]
            query["cursor"] = first["next_cursor"]
            second = call("query_data", query)
            assert first["version_manifest"] == second["version_manifest"] and second["page_complete"]
            assert len(first["records"]) + len(second["records"]) == 4
            checks.append("catalog_null_and_yearly_confirmed_qfq_fixed_paging")
            computation = dict(contract_version="v1", execution="inline", inputs=[dict(input_id="prices", version_ref=fixture["version_ref"])], range=dict(start_year=2020, end_year=2023), metrics=[dict(metric_id="down", name="consecutive_direction", algorithm_version="local-consecutive-direction-v1", input_ids=["prices"], parameters=dict(direction="down", min_periods=3))])
            computed = call("compute_metrics", computation)
            assert computed["result"]["cumulative_change"] == "-0.300000000000"
            assert computed == call("compute_metrics", computation)
            checks.append("fixed_compute_deterministic_fixture_vector")
            accepted = call("start_collection", dict(request_id="smoke-write", dataset="market.daily", contract_version="v1", selection=dict(kind="explicit", securities=[entity])))
            assert accepted["runtime_readiness"] == "blocked"
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
            assert len(yearly_caps["items"]) == 1 and yearly_caps["items"][0]["runtime_readiness"] == "ready"
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
            publication_caps = call("get_data_capabilities", dict(operation="verify_publication_result"))
            assert len(publication_caps["items"]) == 1 and publication_caps["items"][0]["runtime_readiness"] == "ready"
            assert publication_caps["production_ready"] is False
            publication_input = dict(contract_version="v1", request_id="smoke-publication-check", target_task_id=original["task_id"])
            assert call("verify_publication_result", dict(publication_input, published=True), error=True)["code"] == "invalid_arguments"
            rejected = call("verify_publication_result", publication_input, error=True)
            assert rejected["code"] == "invalid_arguments" and "verification_not_applicable" in rejected["message"]
            assert call("get_request", dict(contract_version="v1", request_id="smoke-publication-check"))["bound"] is False
            checks.append("publication_inspection_discovery_and_never_started_refusal_without_binding")
            check = call("verify_stopped_writes", dict(contract_version="v1", request_id="smoke-stopping-check", target_task_id=original["task_id"]))
            assert check["blocking_reasons"] == []
            assert call("run_worker_once", {})["task_id"] == check["task_id"]
            checked = call("get_task_progress", dict(task_id=check["task_id"]))
            assert checked["stopped_write_check"]["conclusion"] == "interrupted"
            assert call("get_task", dict(contract_version="v1", task_id=original["task_id"]))["status"] == "interrupted"
            checks.append("stopping_inspection_proves_never_started_without_source_execution")
            inspection_input = dict(contract_version="v1", request_id="smoke-bounded-inspection", target_task_id=check["task_id"])
            assert call("inspect_verification_task", dict(inspection_input, force=True), error=True)["code"] == "invalid_arguments"
            inspection = call("inspect_verification_task", inspection_input)
            assert inspection["task"] is None and inspection["bound"] is True
            assert inspection["terminal_result"]["receipt"]["conclusion"] == "already_finished"
            assert call("get_request", dict(contract_version="v1", request_id="smoke-bounded-inspection")) == inspection
            assert call("inspect_verification_task", inspection_input) == inspection
            unchanged_check = call("get_task_progress", dict(task_id=check["task_id"]))
            assert unchanged_check["task"] == checked["task"]
            assert unchanged_check["stopped_write_check"] == checked["stopped_write_check"]
            checks.append("bounded_inspection_historical_request_without_recursive_task_or_rewrite")
        finally:
            child.stdin.close()
            if child.wait(timeout=20) != 0:
                raise RuntimeError("stdio process exited unsuccessfully")
    runtime_smoke(binaries)
    checks.append("persistent_service_proxy_discovery_and_reconnect")
    checks.append("same_proxy_idle_over_30_seconds_then_calls_again")
    acquire_smoke(binaries)
    checks.append("acquire_service_socket_discovery_and_capabilities")
    # `export_research` exists only when the package was built with
    # `--features export` (scripts/build_release.sh). A default `target/debug`
    # build has no such binary, so the repo-side smoke skips this section;
    # the package CI job always runs it.
    if (binaries / "export_research").is_file():
        export_readers = export_smoke(binaries)
        checks.append("export_research_synthetic_snapshot_manifest_and_file_sha256")
        if package_manifest_smoke(binaries):
            checks.append("package_manifest_records_export_feature_and_binary_bytes")
    else:
        export_readers = (
            "NOT_RUN (export_research is not built; the release package enables "
            "--features export)"
        )
    report = dict(status="PASS", version=version, binary_sha256=hashlib.sha256((binaries / "mcp_data").read_bytes()).hexdigest(), checks=checks, export_readers=export_readers, live_source="NOT_RUN", production="NOT_PRODUCTION_READY", data="SYNTHETIC_FIXTURE_ONLY")
    args.report.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(report, ensure_ascii=False))

if __name__ == "__main__":
    main()
