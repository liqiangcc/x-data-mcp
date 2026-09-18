#!/usr/bin/env python3
"""Issue #214 — consumer read-path acceptance on the published real dataset.

Real data_service (unix socket) + mcp_connect client over the update5219
acceptance DB (read-only; no copy — C10 proves zero mutation): baseline
mechanical counts -> capabilities/gates -> full-range pagination for three
pinned securities + catalog -> version binding +/- -> cursor discipline ->
versions/availability -> compute_metrics rejection -> task introspection ->
boundary rejections -> post-run recount. Assertions C1-C12 per PLAN.md;
all expectations are frozen constants, never derived from the service.
"""
import datetime
import hashlib
import json
import os
import select
import sqlite3
import subprocess
import sys

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
HERE = os.path.dirname(os.path.abspath(__file__))
DB = "/home/box/acceptance-run-issue4-update/db.sqlite"
SOCK = "/home/box/preview-deploy/run-realdata/mcp.sock"
PROFILE = os.environ.get("BUILD_PROFILE", "release")
CLIENT = os.path.join(REPO, "target", PROFILE, "mcp_connect")
SERVICE_BIN = os.path.join(REPO, "target", PROFILE, "data_service")

BLOCKERS = [
    "GBBQ_QFQ_NOT_VERIFIED (#54: float32 business decimal, event and QFQ semantics)",
    "FILE_ACCESS_USE_NOT_VERIFIED (long-running server acquisition/use boundaries)",
    "FILE_SET_CONSISTENCY_NOT_VERIFIED (independent .day/gbbq hashes, update times and compatibility evidence)",
]
EXPECTED_TOOLS = [
    "backfill_data", "cancel_task", "compute_metrics",
    "get_data_availability", "get_data_capabilities", "get_request",
    "get_task", "get_task_progress", "inspect_verification_task",
    "list_data_versions", "list_tasks", "query_data", "retry_task",
    "run_worker_once", "start_collection", "update_data",
    "verify_data_gap", "verify_publication_result", "verify_stopped_writes",
]
EXPECTED_COUNTS = {
    "data_versions": 5219, "catalog_versions": 1, "daily_bars": 16629206,
    "effective_heads": 5219, "market_update_results": 5219,
    "tasks": 109, "yearly_bars": 0, "proven_gaps": 0,
}
TASK_STATUS = {"succeeded": 103, "partially_failed": 4, "failed": 2}
KNOWN_TASK = "task-fe036513920bb442a6c4c0b43b3337a9"
KNOWN_REQUEST = "update-052"
MANIFEST_SHA = "b8a0d7b9aec1a270549d1e53ba26c0ab04842a23c05d1d1f7e75dd44a9f444d3"

PINS = {
    "sz.000002": {
        "entity": {"exchange": "sz", "code": "000002"},
        "version_id": "collection-version-e75dbaa86f6913776c0ad7847c9d6e3fd222f67e5f8865dd7a6082859123e5b0",
        "bar_count": 8479,
        "first": {"date": "1991-01-29", "open": "-8.83", "high": "-8.83", "low": "-8.83", "close": "-8.83"},
        "last": {"date": "2026-09-14", "open": "3.05", "high": "3.07", "low": "3.03", "close": "3.05"},
        "anchor": {"date": "2000-01-10", "open": "-8.14", "high": "-8.06", "low": "-8.15", "close": "-8.06"},
    },
    "sh.600006": {
        "entity": {"exchange": "sh", "code": "600006"},
        "version_id": "collection-version-20312846826be3e15b3fbb413b9037d85a457bb9384f56c906fd7d9174f7dfbd",
        "bar_count": 6467,
        "first": {"date": "1999-07-27", "open": "1.16", "high": "1.17", "low": "0.92", "close": "0.94"},
        "last": {"date": "2026-09-14", "open": "5.19", "high": "5.39", "low": "5.19", "close": "5.36"},
    },
    "sh.600004": {
        "entity": {"exchange": "sh", "code": "600004"},
        "version_id": "collection-version-69e9b01833330f412909066752f35c4eb93be816be4f7dbaa8d6bacff8adc171",
        "bar_count": 5630,
        "first": {"date": "2003-04-28", "open": "0.96", "high": "1.56", "low": "0.94", "close": "1.55"},
        "last": {"date": "2026-09-14", "open": "7.71", "high": "7.79", "low": "7.7", "close": "7.76"},
    },
}
CATALOG_EXPECTED = 5224
NO_DAY_ENTITY = {"exchange": "sh", "code": "601091"}   # catalog member, no .day
UNKNOWN_ENTITY = {"exchange": "sh", "code": "999999"}  # not in catalog

logf = open(os.path.join(HERE, "run-log.jsonl"), "w")


def ts():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def record(kind, payload):
    logf.write(json.dumps({"ts": ts(), "kind": kind, "payload": payload},
                          ensure_ascii=False) + "\n")
    logf.flush()


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def counts(db):
    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    out = {t: con.execute(f"SELECT COUNT(*) FROM {t}").fetchone()[0]
           for t in EXPECTED_COUNTS}
    out["task_status"] = dict(con.execute(
        "SELECT status,COUNT(*) FROM tasks GROUP BY status").fetchall())
    con.close()
    return out


class Client:
    """One mcp_connect process; sequential JSON-RPC over the service socket."""

    def __init__(self):
        env = dict(os.environ)
        env["X_DATA_MCP_UNSAFE_NONLOCAL_FS"] = "1"
        self.proc = subprocess.Popen(
            [CLIENT, SOCK], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, env=env, text=True)
        self.next_id = 0

    def rpc(self, method, params=None):
        self.next_id += 1
        req = {"jsonrpc": "2.0", "id": self.next_id, "method": method}
        if params is not None:
            req["params"] = params
        record("request", req)
        self.proc.stdin.write(json.dumps(req) + "\n")
        self.proc.stdin.flush()
        assert select.select([self.proc.stdout], [], [], 120)[0], method
        resp = json.loads(self.proc.stdout.readline())
        record("response", resp)
        assert resp.get("id") == self.next_id, resp
        return resp

    def tool_raw(self, name, arguments):
        return self.rpc("tools/call", {"name": name, "arguments": arguments})

    def tool(self, name, arguments):
        resp = self.tool_raw(name, arguments)
        result = resp.get("result", {})
        if resp.get("error") or result.get("isError"):
            record("tool_error", {"name": name, "response": resp})
            raise SystemExit(f"tool {name} failed: {json.dumps(resp)[:2000]}")
        return result.get("structuredContent")

    def tool_failed(self, name, arguments):
        """True iff the call was explicitly rejected (rpc error or isError)."""
        resp = self.tool_raw(name, arguments)
        result = resp.get("result", {})
        return bool(resp.get("error")) or bool(result.get("isError"))

    def close(self):
        self.proc.stdin.close()
        self.proc.wait(timeout=15)
        record("client_exit", {"returncode": self.proc.returncode})


def main():
    results = {"assertions": [], "blocked_paths": []}

    def check(name, ok, detail):
        results["assertions"].append(
            {"name": name, "ok": bool(ok), "detail": detail})
        record("assertion", results["assertions"][-1])

    # ---- S0: environment fingerprint
    fp = {
        "git_head": subprocess.run(
            ["git", "-C", REPO, "rev-parse", "HEAD"],
            capture_output=True, text=True).stdout.strip(),
        "client_sha256": sha256_file(CLIENT),
        "service_sha256": sha256_file(SERVICE_BIN),
        "db": DB, "db_size": os.path.getsize(DB), "socket": SOCK,
        "fs_opt_out": "X_DATA_MCP_UNSAFE_NONLOCAL_FS (development only)",
    }
    record("fingerprint", fp)

    # ---- S1: mechanical baseline
    before = counts(DB)
    record("baseline", before)
    base_ok = all(before[k] == v for k, v in EXPECTED_COUNTS.items()) \
        and before["task_status"] == TASK_STATUS

    cli = Client()
    try:
        # ---- S2: C1 binding + C2 capabilities
        init = cli.rpc("initialize", {})["result"]
        tools = [t["name"] for t in
                 cli.rpc("tools/list", {})["result"]["tools"]]
        caps = cli.tool("get_data_capabilities", {})
        blocked_write = {}
        for item in caps.get("items", []):
            if item.get("operation") in ("start_collection", "update_data",
                                         "backfill_data"):
                blocked_write[item["operation"]] = item.get(
                    "blocking_reasons") or item.get("readiness")
        write_blocked = all(
            item.get("readiness") == "blocked"
            and all(b in json.dumps(item) for b in BLOCKERS)
            for item in caps.get("items", [])
            if item.get("operation") in ("start_collection", "update_data",
                                       "backfill_data"))
        check("C1_service_binding",
              init["serverInfo"]["version"] == "0.2.0-preview.12"
              and sorted(tools) == sorted(EXPECTED_TOOLS)
              and caps.get("production_ready") is False,
              {"version": init["serverInfo"]["version"],
               "tool_count": len(tools),
               "tool_drift": sorted(set(tools) ^ set(EXPECTED_TOOLS)),
               "production_ready": caps.get("production_ready")})
        check("C2_capabilities",
              caps.get("capability_revision") == "generic-data-v1"
              and caps.get("production_ready") is False
              and write_blocked
              and caps.get("max_page_size") == 1000,
              {"revision": caps.get("capability_revision"),
               "write_blocked": write_blocked,
               "blocked_ops": list(blocked_write)})

        def daily_query(entity, version, cursor=None, page_size=1000,
                        start="1900-01-01", end="2100-12-31"):
            return cli.tool("query_data", {
                "dataset": "market.daily", "contract_version": "v1",
                "selection": {"kind": "single", "entity": entity},
                "filter": {"kind": "daily", "start_date": start,
                           "end_date": end},
                "version": version, "page_size": page_size,
                "cursor": cursor})

        def paginate(entity, version, page_size=1000):
            recs, cursor, pages, last = [], None, 0, None
            while True:
                page = daily_query(entity, version, cursor=cursor,
                                   page_size=page_size)
                pages += 1
                recs.extend(page["records"])
                last = page
                cursor = page.get("next_cursor")
                if cursor is None:
                    break
                assert pages < 100, "pagination runaway"
            return recs, last, pages

        # ---- S3: C3 full-range pagination
        c3_detail, c3_ok = {}, True
        saved_page = {}
        for sym, pin in PINS.items():
            t0 = datetime.datetime.now()
            recs, last_page, pages = paginate(
                pin["entity"], {"kind": "latest_per_entity"})
            secs = (datetime.datetime.now() - t0).total_seconds()
            dates = [r["date"] for r in recs]
            ok = (len(recs) == pin["bar_count"]
                  and dates == sorted(set(dates))
                  and last_page.get("page_complete") is True
                  and last_page.get("next_cursor") is None
                  and all(r["version_ref"]["version_id"] == pin["version_id"]
                          for r in recs)
                  and {k: recs[0][k] for k in ("date", "open", "high", "low",
                                               "close")} == pin["first"]
                  and {k: recs[-1][k] for k in ("date", "open", "high", "low",
                                                "close")} == pin["last"])
            if "anchor" in pin:
                anchor = [r for r in recs
                          if r["date"] == pin["anchor"]["date"]]
                ok = ok and len(anchor) == 1 and \
                    {k: anchor[0][k] for k in
                     ("open", "high", "low", "close")} == \
                    {k: pin["anchor"][k] for k in
                     ("open", "high", "low", "close")}
            saved_page[sym] = last_page
            c3_detail[sym] = {"records": len(recs), "pages": pages,
                              "seconds": round(secs, 2), "ok": ok}
            c3_ok = c3_ok and ok
        check("C3_full_pagination", c3_ok, c3_detail)

        # ---- S3b: catalog pagination
        cat_recs, cursor, cat_last = [], None, None
        while True:
            page = cli.tool("query_data", {
                "dataset": "securities.catalog", "contract_version": "v1",
                "selection": {"kind": "catalog", "scope": "current_sh_sz_a"},
                "filter": {"kind": "catalog"},
                "version": {"kind": "latest_per_entity"},
                "page_size": 1000, "cursor": cursor})
            cat_recs.extend(page["records"])
            cat_last = page
            cursor = page.get("next_cursor")
            if cursor is None:
                break
        cat_entities = {json.dumps(r["entity"], sort_keys=True)
                        for r in cat_recs}
        check("C3b_catalog_pagination",
              len(cat_recs) == CATALOG_EXPECTED
              and len(cat_entities) == CATALOG_EXPECTED
              and all("name" in r and "industry" in r for r in cat_recs)
              and cat_last.get("page_complete") is True,
              {"records": len(cat_recs), "unique_entities": len(cat_entities)})

        # ---- S4: C4 version binding
        explicit = daily_query(
            PINS["sz.000002"]["entity"],
            {"kind": "explicit_version_map", "versions": [
                {"entity": PINS["sz.000002"]["entity"],
                 "version_id": PINS["sz.000002"]["version_id"]}]})
        explicit_ok = (explicit["records"]
                       and explicit["records"][0]["version_ref"]
                       ["version_id"] == PINS["sz.000002"]["version_id"]
                       and {k: explicit["records"][0][k] for k in
                            ("date", "open", "high", "low", "close")}
                       == PINS["sz.000002"]["first"])
        bogus = cli.tool_failed("query_data", {
            "dataset": "market.daily", "contract_version": "v1",
            "selection": {"kind": "single",
                          "entity": PINS["sz.000002"]["entity"]},
            "filter": {"kind": "daily", "start_date": "1900-01-01",
                       "end_date": "2100-12-31"},
            "version": {"kind": "explicit_version_map", "versions": [
                {"entity": PINS["sz.000002"]["entity"],
                 "version_id": "collection-version-bogus"}]},
            "page_size": 10, "cursor": None})
        empty_day = daily_query(NO_DAY_ENTITY, {"kind": "latest_per_entity"})
        empty_unknown = daily_query(UNKNOWN_ENTITY,
                                    {"kind": "latest_per_entity"})
        check("C4_version_binding",
              explicit_ok and bogus
              and empty_day["records"] == []
              and empty_unknown["records"] == [],
              {"explicit_ok": explicit_ok, "bogus_rejected": bogus,
               "no_day_records": len(empty_day["records"]),
               "unknown_records": len(empty_unknown["records"]),
               "no_day_manifest": [m.get("status") for m in
                                   empty_day.get("version_manifest", [])]})

        # ---- S5: C5 cursor discipline
        page1 = daily_query(PINS["sz.000002"]["entity"],
                            {"kind": "latest_per_entity"}, page_size=10)
        cur = page1.get("next_cursor")
        bad_cursor = cli.tool_failed("query_data", {
            "dataset": "market.daily", "contract_version": "v1",
            "selection": {"kind": "single",
                          "entity": PINS["sz.000002"]["entity"]},
            "filter": {"kind": "daily", "start_date": "1900-01-01",
                       "end_date": "2100-12-31"},
            "version": {"kind": "latest_per_entity"},
            "page_size": 10, "cursor": "forged-cursor"})
        cross_cursor = cli.tool_failed("query_data", {
            "dataset": "market.daily", "contract_version": "v1",
            "selection": {"kind": "single",
                          "entity": PINS["sh.600004"]["entity"]},
            "filter": {"kind": "daily", "start_date": "1900-01-01",
                       "end_date": "2100-12-31"},
            "version": {"kind": "latest_per_entity"},
            "page_size": 10, "cursor": cur})
        check("C5_cursor_discipline",
              cur is not None and bad_cursor and cross_cursor,
              {"cursor_issued": cur is not None,
               "forged_rejected": bad_cursor,
               "cross_selection_rejected": cross_cursor})

        # ---- S6: C6 versions + C7 availability
        vers = cli.tool("list_data_versions", {
            "dataset": "market.daily", "contract_version": "v1",
            "selection": {"kind": "single",
                          "entity": PINS["sz.000002"]["entity"]},
            "page_size": 10, "cursor": None})
        vitems = vers.get("items", [])
        vref = vitems[0]["version_ref"] if vitems else {}
        sha_ok = len(vref.get("integrity_sha256", "")) == 64 and all(
            ch in "0123456789abcdef" for ch in
            vref.get("integrity_sha256", ""))
        check("C6_version_listing",
              len(vitems) == 1
              and vref.get("version_id") == PINS["sz.000002"]["version_id"]
              and sha_ok
              and bool(vitems[0].get("published_at"))
              and vers.get("page_complete") is True,
              {"items": len(vitems), "version_id": vref.get("version_id"),
               "sha256_canonical": sha_ok})

        avail = cli.tool("get_data_availability", {
            "dataset": "market.daily", "contract_version": "v1",
            "selection": {"kind": "single",
                          "entity": PINS["sz.000002"]["entity"]},
            "filter": {"kind": "daily", "start_date": "1900-01-01",
                       "end_date": "2100-12-31"},
            "version": {"kind": "latest_per_entity"},
            "page_size": 10, "cursor": None})
        aitems = avail.get("items", [])
        a0 = aitems[0] if aitems else {}
        check("C7_availability",
              len(aitems) == 1
              and a0.get("availability") in ("unknown", "available", "partial")
              and a0.get("availability") != "complete"
              and MANIFEST_SHA in a0.get("coverage_evidence", "")
              and "NOT_PRODUCTION_READY" in a0.get("coverage_evidence", "")
              and a0.get("confirmed_gaps") == [],
              {"availability": a0.get("availability"),
               "confirmed_gaps": a0.get("confirmed_gaps"),
               "freshness": a0.get("freshness"),
               "evidence": a0.get("coverage_evidence", "")[:160]})

        # ---- S7: C8 compute_metrics rejection path
        compute_args = {
            "contract_version": "v1", "execution": "inline",
            "inputs": [{"input_id": "prices", "version_ref": {
                "dataset": "market.yearly", "contract_version": "v1",
                "entity": PINS["sz.000002"]["entity"],
                "version_id": "collection-version-nonexistent",
                "integrity_sha256": "0" * 64}}],
            "range": {"start_year": 2020, "end_year": 2023},
            "metrics": [{"metric_id": "m1", "name": "consecutive_direction",
                         "algorithm_version": "local-consecutive-direction-v1",
                         "input_ids": ["prices"],
                         "parameters": {"direction": "down",
                                        "min_periods": 3}}]}
        comp1 = cli.tool_raw("compute_metrics", compute_args)
        comp2 = cli.tool_raw("compute_metrics", compute_args)
        comp_failed = (bool(comp1.get("error"))
                       or comp1.get("result", {}).get("isError"))
        comp_body = comp1.get("result", {}).get("structuredContent") or {}
        comp_not_computed = comp_failed or (
            comp_body.get("result", {}).get("outcome") == "not_computable")
        bad_metric = cli.tool_failed("compute_metrics", {
            "contract_version": "v1", "execution": "inline",
            "inputs": [], "range": {"start_year": 2020, "end_year": 2023},
            "metrics": [{"metric_id": "m1", "name": "no_such_metric",
                         "algorithm_version": "local-consecutive-direction-v1",
                         "input_ids": ["x"], "parameters": {}}]})
        check("C8_compute_rejection",
              comp_not_computed
              and comp1.get("result") == comp2.get("result")
              and comp1.get("error") == comp2.get("error")
              and bad_metric,
              {"explicit_failure": comp_failed,
               "outcome": comp_body.get("result", {}).get("outcome"),
               "repeat_identical": comp1.get("result") == comp2.get("result")
               and comp1.get("error") == comp2.get("error"),
               "bad_metric_rejected": bad_metric})

        # ---- S8: C9 task introspection
        task_items, cursor = [], None
        while True:
            page = cli.tool("list_tasks", {"page_size": 1000,
                                           "cursor": cursor})
            task_items.extend(page["items"])
            cursor = page.get("next_cursor")
            if cursor is None:
                break
        status_dist = {}
        for t in task_items:
            status_dist[t["status"]] = status_dist.get(t["status"], 0) + 1
        gt = cli.tool("get_task", {"task_id": KNOWN_TASK})
        gp = cli.tool("get_task_progress", {"task_id": KNOWN_TASK,
                                            "page_size": 1000,
                                            "cursor": None})
        gr = cli.tool("get_request", {"request_id": KNOWN_REQUEST})
        check("C9_task_introspection",
              len(task_items) == EXPECTED_COUNTS["tasks"]
              and status_dist == TASK_STATUS
              and (gt.get("task") or gt).get("status") == "succeeded"
              and (gp.get("task") or {}).get("task_id") == KNOWN_TASK
              and json.dumps(gr),
              {"listed": len(task_items), "status_dist": status_dist,
               "get_task_status": (gt.get("task") or gt).get("status"),
               "progress_task": (gp.get("task") or {}).get("task_id"),
               "request_keys": list(gr.keys())[:8]})

        # ---- S9: C11 boundary rejections
        base_q = {"dataset": "market.daily", "contract_version": "v1",
                  "selection": {"kind": "single",
                                "entity": PINS["sz.000002"]["entity"]},
                  "filter": {"kind": "daily", "start_date": "1900-01-01",
                             "end_date": "2100-12-31"},
                  "version": {"kind": "latest_per_entity"},
                  "page_size": 10, "cursor": None}
        boundaries = {
            "unknown_field": {**base_q, "bogus_field": 1},
            "page_size_0": {**base_q, "page_size": 0},
            "page_size_1001": {**base_q, "page_size": 1001},
            "bad_contract": {**base_q, "contract_version": "v9"},
            "bad_dataset": {**base_q, "dataset": "bogus.dataset"},
            "bad_tool_args": {"task_id": 123},
        }
        c11 = {k: cli.tool_failed(
            "query_data" if k != "bad_tool_args" else "get_task", v)
            for k, v in boundaries.items()}
        c11["write_tool_schema"] = cli.tool_failed(
            "start_collection", {"dataset": "market.daily",
                                 "contract_version": "v1",
                                 "request_id": "consumer-boundary",
                                 "selection": {"kind": "bogus"}})
        check("C11_boundary_rejections", all(c11.values()), c11)

        # ---- S10: C12 honesty fields (from saved C3 page)
        page = saved_page["sz.000002"]
        manifest = page.get("version_manifest", [])
        meta = (manifest[0].get("metadata") or {}) if manifest else {}
        check("C12_honesty_fields",
              bool(page.get("snapshot_id")) and bool(page.get("query_time"))
              and isinstance(page.get("limitations"), list)
              and manifest and meta.get("coverage_status") is not None
              and meta.get("coverage_reason") is not None
              and isinstance(meta.get("units"), list),
              {"snapshot_id": page.get("snapshot_id"),
               "limitations": page.get("limitations"),
               "manifest_status": [m.get("status") for m in manifest],
               "coverage_status": meta.get("coverage_status"),
               "coverage_reason": str(meta.get("coverage_reason"))[:120]})
    finally:
        cli.close()

    # ---- S11: C10 read-only invariance
    after = counts(DB)
    record("post_counts", after)
    check("C10_read_only_invariance",
          before == after and base_ok,
          {"baseline_ok": base_ok, "before": before, "after": after})

    results["run_identity"] = fp
    results["baseline"] = {"before": before, "after": after}
    passed = sum(1 for a in results["assertions"] if a["ok"])
    results["summary"] = {"pass": passed,
                          "total": len(results["assertions"]),
                          "all_pass": passed == len(results["assertions"])}
    record("summary", results["summary"])
    with open(os.path.join(HERE, "results.json"), "w") as f:
        json.dump(results, f, ensure_ascii=False, indent=2)
    print(json.dumps(results["summary"]))
    sys.exit(0 if results["summary"]["all_pass"] else 1)


if __name__ == "__main__":
    main()
