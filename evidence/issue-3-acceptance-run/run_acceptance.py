#!/usr/bin/env python3
"""Issue #3 acceptance run: real mcp_data stdio service against the retained
gen-2 official-file snapshot.

Drives the production service binary (target/debug/mcp_data) over JSON-RPC
stdio with X_DATA_MCP_OFFICIAL_MANIFEST / X_DATA_MCP_OFFICIAL_DIR configured,
collects market.daily for sz.000002 + sh.600006 + sh.600004, reads oracle bars
back through query_data, attempts the gated market.yearly path, then
mechanically re-verifies the published store directly (sqlite).

Every request/response is logged verbatim to run-log.jsonl; the mechanical
verdict lands in results.json. This is an acceptance-evidence run, not a
production deployment: no scheduler, no ecs, no gate changes.
"""
import datetime
import hashlib
import json
import os
import sqlite3
import subprocess
import sys

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
HERE = os.path.dirname(os.path.abspath(__file__))
RUN_DIR = "/home/box/acceptance-run-issue3"
SNAP_DIR = os.path.join(RUN_DIR, "snapshot")
MANIFEST = os.path.join(HERE, "snapshot-manifest.json")
DB = os.path.join(RUN_DIR, "acceptance.sqlite")
MCP_DATA = os.path.join(REPO, "target/debug/mcp_data")
TASK_SQLITE = os.path.join(REPO, "target/debug/task_sqlite")
RETAINED = "/home/box/retained-inputs"
SECURITIES = ["sz.000002", "sh.600006", "sh.600004"]

# Frozen oracle values from evidence/issue-54-qfq-contract-regression/result.json
# (client_oracle_bars): client-observed forward-adjusted bars.
ORACLES = [
    ("sz.000002", "2000-01-10", {"O": "-8.14", "H": "-8.06", "L": "-8.15", "C": "-8.06"}),
    ("sz.000002", "1999-12-29", {"O": "-8.28", "H": "-8.27", "L": "-8.29", "C": "-8.27"}),
    ("sh.600006", "2025-07-16", {"O": "7.25", "H": "7.27", "L": "7.22", "C": "7.25"}),
    ("sh.600006", "2025-07-17", {"O": "7.24", "H": "7.32", "L": "7.23", "C": "7.32"}),
    ("sh.600004", "2026-07-23", {"O": "7.64", "H": "7.71", "L": "7.59", "C": "7.68"}),
    ("sh.600004", "2026-07-24", {"O": "7.67", "H": "7.74", "L": "7.65", "C": "7.67"}),
]

log_path = os.path.join(HERE, "run-log.jsonl")
logf = open(log_path, "w")


def ts():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def record(kind, payload):
    logf.write(json.dumps({"ts": ts(), "kind": kind, "payload": payload},
                          ensure_ascii=False) + "\n")
    logf.flush()


class Service:
    def __init__(self):
        env = dict(os.environ)
        env["X_DATA_MCP_OFFICIAL_MANIFEST"] = MANIFEST
        env["X_DATA_MCP_OFFICIAL_DIR"] = SNAP_DIR
        self.proc = subprocess.Popen(
            [MCP_DATA, DB], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
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
        line = self.proc.stdout.readline()
        resp = json.loads(line)
        record("response", resp)
        return resp

    def tool(self, name, arguments):
        resp = self.rpc("tools/call", {"name": name, "arguments": arguments})
        result = resp.get("result", {})
        if result.get("isError"):
            record("tool_error", {"name": name, "result": result})
            raise SystemExit(f"tool {name} failed: {json.dumps(result)[:2000]}")
        return result.get("structuredContent")


def main():
    for name in ("official-base.zip", "official-gbbq.zip", "official-hsjday.zip"):
        if not os.path.isfile(os.path.join(RETAINED, name)):
            raise SystemExit(f"retained input absent: {RETAINED}/{name}")
    for name in ("base.zip", "gbbq.zip", "hsjday.zip"):
        if not os.path.isfile(os.path.join(SNAP_DIR, name)):
            raise SystemExit(f"snapshot member absent: {SNAP_DIR}/{name}")
    for stale in (DB, DB + "-wal", DB + "-shm"):
        if os.path.exists(stale):
            os.remove(stale)
    record("env", {
        "run_dir": RUN_DIR, "db": DB, "manifest": MANIFEST, "snapshot_dir": SNAP_DIR,
        "binaries": {"mcp_data": MCP_DATA, "task_sqlite": TASK_SQLITE},
        "securities": SECURITIES,
    })
    init = subprocess.run([TASK_SQLITE, "init", DB], capture_output=True, text=True)
    record("task_sqlite_init", {"stdout": init.stdout, "stderr": init.stderr,
                              "returncode": init.returncode})
    if init.returncode != 0:
        raise SystemExit("database init failed")

    svc = Service()
    svc.rpc("initialize")

    accepted = svc.tool("start_collection", {
        "request_id": "acceptance-daily-1",
        "dataset": "market.daily",
        "contract_version": "v1",
        "selection": {"kind": "explicit", "securities": [
            {"exchange": s.split(".")[0], "code": s.split(".")[1]} for s in SECURITIES
        ]},
    })
    record("accepted_collection", accepted)
    task_id = accepted["task_id"]

    # One-shot manual scheduling: drive the worker until the task reaches a
    # terminal state; each tick processes one candidate task.
    # One-shot manual scheduling: each run_worker_once tick processes one
    # candidate task; the tick response reports the terminal status.
    tick = svc.tool("run_worker_once", {})
    status = svc.tool("get_task_progress", {"task_id": task_id})
    record("final_task_status", {"worker_tick": tick, "progress": status})

    # Oracle read-back through the public query surface.
    for symbol, date, _fields in ORACLES:
        exchange, code = symbol.split(".")
        page = svc.tool("query_data", {
            "dataset": "market.daily", "contract_version": "v1",
            "selection": {"kind": "single",
                          "entity": {"exchange": exchange, "code": code}},
            "filter": {"kind": "daily", "start_date": date, "end_date": date},
            "version": {"kind": "latest_per_entity"},
            "page_size": 10, "cursor": None})
        record("oracle_query", {"symbol": symbol, "date": date, "page": page})

    # Service-level yearly is expected to fail closed (OfficialYearlyVerifier
    # is unimplemented); record the refusal verbatim.
    con = sqlite3.connect(DB)
    head = con.execute(
        "SELECT version_id FROM effective_heads WHERE symbol='sz.000002' AND period='day'"
    ).fetchone()[0]
    gated = svc.tool("update_data", {
        "dataset": "market.yearly", "request_id": "acceptance-yearly-1",
        "contract_version": "v1",
        "items": [{"entity": {"exchange": "sz", "code": "000002"},
                   "base_daily_version_id": head, "years": [2000],
                   "publication": {"mode": "current_head",
                                   "expected_yearly_head": None}}]})
    record("accepted_yearly", gated)
    svc.tool("run_worker_once", {})
    yearly_status = svc.tool("get_task_progress", {"task_id": gated["task_id"]})
    record("yearly_task_status", yearly_status)

    svc.proc.stdin.close()
    stderr = svc.proc.stderr.read()
    svc.proc.wait()
    record("service_exit", {"returncode": svc.proc.returncode, "stderr": stderr})

    # ---- mechanical re-verification of the published store ----
    results = {"assertions": [], "versions": {}, "provenance": {}}

    def check(name, ok, detail):
        results["assertions"].append({"name": name, "ok": bool(ok), "detail": detail})

    rows = con.execute(
        "SELECT symbol, version_id, state, record_count, first_date, last_date,"
        " coverage_status, compat_json FROM data_versions WHERE period='day'"
        " ORDER BY symbol").fetchall()
    check("daily_versions_published",
          len(rows) == 3 and all(r[2] == "published" for r in rows),
          [(r[0], r[1], r[2], r[3], r[4], r[5], r[6]) for r in rows])

    for symbol, version_id, _state, count, first, last, _cov, compat in rows:
        n, f, l = con.execute(
            "SELECT COUNT(*), MIN(trade_date), MAX(trade_date) FROM daily_bars"
            " WHERE version_id=?", (version_id,)).fetchone()
        nulls = con.execute(
            "SELECT COUNT(*) FROM daily_bars WHERE version_id=? AND"
            " (volume IS NOT NULL OR amount IS NOT NULL)", (version_id,)).fetchone()[0]
        check(f"{symbol}_bar_count_consistent", n == count and f == first and l == last,
              {"record_count": count, "bars": n, "first": f, "last": l})
        check(f"{symbol}_volume_amount_not_promoted", nulls == 0,
              f"{nulls} bars carry volume/amount")
        prov = json.loads(compat)
        inputs = prov.get("batch", {}).get("provenance", {}).get("snapshot_inputs", [])
        results["provenance"][symbol] = {
            "version_id": version_id,
            "snapshot_inputs": inputs,
            "note": prov.get("batch", {}).get("provenance", {}).get("transform"),
        }
        for inp in inputs:
            retained_file = {
                "base_industry": "official-base.zip",
                "gbbq": "official-gbbq.zip",
                "day": "official-hsjday.zip",
            }.get(inp.get("role", ""))
            actual = hashlib.sha256(
                open(os.path.join(RETAINED, retained_file), "rb").read()).hexdigest()
            check(f"{symbol}_provenance_{inp.get('role')}_sha",
                  inp.get("archive_sha256") == actual
                  and len(inp.get("member_sha256", "")) == 64,
                  {"archive_sha256": inp.get("archive_sha256"),
                   "retained_sha256": actual,
                   "member_sha256": inp.get("member_sha256"),
                   "member": inp.get("member")})

    for symbol, date, fields in ORACLES:
        row = con.execute(
            "SELECT b.open,b.high,b.low,b.close FROM daily_bars b"
            " JOIN data_versions v ON b.version_id=v.version_id"
            " WHERE v.symbol=? AND b.trade_date=? AND v.state='published'",
            (symbol, date)).fetchone()
        got = dict(zip(("O", "H", "L", "C"), row)) if row else None
        check(f"oracle_{symbol}_{date}", got == fields, {"expected": fields, "got": got})

    # Yearly derivation lineage check (mechanical, not a publication):
    # re-aggregate the published daily bars per Asia/Shanghai natural year and
    # confirm the derived values are computable; the service-level publish is
    # gated and recorded above.
    for symbol in SECURITIES:
        bars = con.execute(
            "SELECT b.trade_date,b.open,b.high,b.low,b.close FROM daily_bars b"
            " JOIN data_versions v ON b.version_id=v.version_id"
            " WHERE v.symbol=? AND v.period='day' AND v.state='published'"
            " ORDER BY b.trade_date", (symbol,)).fetchall()
        yearly = {}
        for d, o, h, l, c in bars:
            y = int(d[:4])
            agg = yearly.setdefault(y, {"open": o, "high": h, "low": l,
                                       "close": c, "first": d, "last": d})
            agg["high"] = max(agg["high"], h)
            agg["low"] = min(agg["low"], l)
            agg["close"] = c
            agg["last"] = d
        check(f"{symbol}_yearly_derivable", len(yearly) > 0,
              {"years": len(yearly),
               "first_year": min(yearly), "last_year": max(yearly)})
    results["yearly_service_gate"] = yearly_status

    # Publication receipts bound to the collection task.
    receipts = con.execute(
        "SELECT publication_id, version_id, item_id FROM publication_batches"
        " WHERE task_id=?", (task_id,)).fetchall()
    results["publication_receipts"] = [
        {"publication_id": p, "version_id": v, "item_id": i}
        for p, v, i in receipts]
    check("receipts_per_security", len(receipts) == 3, results["publication_receipts"])

    out = os.path.join(HERE, "results.json")
    with open(out, "w") as f:
        json.dump(results, f, ensure_ascii=False, indent=2)
    failed = [a for a in results["assertions"] if not a["ok"]]
    print(f"{len(results['assertions'])} assertions, {len(failed)} failed")
    for a in failed:
        print("FAIL", a["name"], json.dumps(a["detail"], ensure_ascii=False)[:400])
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
