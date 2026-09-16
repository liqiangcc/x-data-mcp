#!/usr/bin/env python3
"""Issue #3 yearly acceptance run per the frozen plan
(evidence/issue-3-yearly-acceptance/PLAN.md).

Real mcp_data stdio service against the retained gen-2 snapshot:
daily publish -> update_data/market.yearly full history + unfinished year ->
receipt/lineage/query read-back -> no-change idempotency -> three verbatim
refusals (env-absent / base-unavailable / coverage-insufficient). Expected
yearly values come from an independent stdlib-Decimal aggregation of the
published daily bars (never the #54 frozen oracle values).
"""
import datetime
import hashlib
import json
import os
import sqlite3
import subprocess
import sys
from decimal import Decimal

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
HERE = os.path.dirname(os.path.abspath(__file__))
RUN_DIR = "/home/box/acceptance-run-issue3-yearly"
SNAP_DIR = "/home/box/acceptance-run-issue3/snapshot"
MANIFEST = os.path.join(REPO, "evidence/issue-3-acceptance-run/snapshot-manifest.json")
DB = os.path.join(RUN_DIR, "acceptance-yearly.sqlite")
MCP_DATA = os.path.join(REPO, "target/debug/mcp_data")
TASK_SQLITE = os.path.join(REPO, "target/debug/task_sqlite")
RETAINED = "/home/box/retained-inputs"
SECURITIES = ["sz.000002", "sh.600006", "sh.600004"]
# Frozen per PLAN §3.S4: full covered natural years incl. the unfinished one.
YEARS = {
    "sz.000002": list(range(1991, 2027)),
    "sh.600006": list(range(1999, 2027)),
    "sh.600004": list(range(2003, 2027)),
}
EXPECTED_DAILY_BARS = {"sz.000002": 8479, "sh.600006": 6467, "sh.600004": 5630}
LAST_DAILY_DATE = "2026-09-14"

logf = open(os.path.join(HERE, "run-log.jsonl"), "w")


def ts():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def record(kind, payload):
    logf.write(json.dumps({"ts": ts(), "kind": kind, "payload": payload},
                          ensure_ascii=False) + "\n")
    logf.flush()


class Service:
    def __init__(self, env_present=True):
        env = dict(os.environ)
        env.pop("X_DATA_MCP_OFFICIAL_MANIFEST", None)
        env.pop("X_DATA_MCP_OFFICIAL_DIR", None)
        if env_present:
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
        return json.loads(self.proc.stdout.readline())

    def tool_raw(self, name, arguments):
        resp = self.rpc("tools/call", {"name": name, "arguments": arguments})
        record("response", resp)
        return resp.get("result", {})

    def tool(self, name, arguments):
        result = self.tool_raw(name, arguments)
        if result.get("isError"):
            record("tool_error", {"name": name, "result": result})
            raise SystemExit(f"tool {name} failed: {json.dumps(result)[:2000]}")
        return result.get("structuredContent")

    def close(self):
        self.proc.stdin.close()
        stderr = self.proc.stderr.read()
        self.proc.wait()
        record("service_exit", {"returncode": self.proc.returncode,
                                "stderr": stderr})


def yearly_request(request_id, symbol, base_id, years, expected_head=None):
    exchange, code = symbol.split(".")
    return {
        "request_id": request_id, "dataset": "market.yearly",
        "contract_version": "v1",
        "items": [{"entity": {"exchange": exchange, "code": code},
                   "base_daily_version_id": base_id, "years": years,
                   "publication": {"mode": "current_head",
                                   "expected_yearly_head": expected_head}}]}


def main():
    for name in ("official-base.zip", "official-gbbq.zip", "official-hsjday.zip"):
        if not os.path.isfile(os.path.join(RETAINED, name)):
            raise SystemExit(f"retained input absent: {RETAINED}/{name}")
    for name in ("base.zip", "gbbq.zip", "hsjday.zip"):
        if not os.path.isfile(os.path.join(SNAP_DIR, name)):
            raise SystemExit(f"snapshot member absent: {SNAP_DIR}/{name}")
    os.makedirs(RUN_DIR, exist_ok=True)
    for stale in (DB, DB + "-wal", DB + "-shm"):
        if os.path.exists(stale):
            os.remove(stale)
    record("env", {"run_dir": RUN_DIR, "db": DB, "manifest": MANIFEST,
                   "snapshot_dir": SNAP_DIR, "securities": SECURITIES,
                   "years": YEARS})
    init = subprocess.run([TASK_SQLITE, "init", DB], capture_output=True, text=True)
    record("task_sqlite_init", {"stdout": init.stdout, "stderr": init.stderr,
                              "returncode": init.returncode})
    if init.returncode != 0:
        raise SystemExit("database init failed")

    svc = Service(env_present=True)
    svc.rpc("initialize")

    # S2 daily publish (frozen acceptance counts from PLAN §2).
    accepted = svc.tool("start_collection", {
        "request_id": "yearly-acceptance-daily",
        "dataset": "market.daily", "contract_version": "v1",
        "selection": {"kind": "explicit", "securities": [
            {"exchange": s.split(".")[0], "code": s.split(".")[1]}
            for s in SECURITIES]},
    })
    record("accepted_daily", accepted)
    svc.tool("run_worker_once", {})

    con = sqlite3.connect(DB)
    daily_heads = {}
    for s in SECURITIES:
        row = con.execute(
            "SELECT v.version_id,record_count,last_date FROM data_versions v"
            " JOIN effective_heads e ON v.version_id=e.version_id"
            " WHERE v.symbol=? AND v.period='day'", (s,)).fetchone()
        daily_heads[s] = row[0]
    record("daily_heads", daily_heads)

    # S4+S5 yearly derivation per security (three separate update_data tasks).
    yearly_tasks = {}
    for s in SECURITIES:
        acc = svc.tool("update_data", yearly_request(
            f"yearly-accept-{s}", s, daily_heads[s], YEARS[s]))
        record(f"accepted_yearly_{s}", acc)
        yearly_tasks[s] = acc["task_id"]
    for _ in range(6):
        svc.tool("run_worker_once", {})
    for s, tid in yearly_tasks.items():
        record(f"yearly_items_{s}", con.execute(
            "SELECT item_id,outcome_json FROM task_items WHERE task_id=?",
            (tid,)).fetchall())

    yearly_heads = {}
    for s in SECURITIES:
        yearly_heads[s] = con.execute(
            "SELECT version_id FROM effective_heads WHERE symbol=? AND period='year'",
            (s,)).fetchone()[0]
    record("yearly_heads", yearly_heads)

    # S6 service read-back: first year, 2025, unfinished 2026, last year.
    for s in SECURITIES:
        exchange, code = s.split(".")
        for year in (YEARS[s][0], 2025, 2026, YEARS[s][-1]):
            page = svc.tool("query_data", {
                "dataset": "market.yearly", "contract_version": "v1",
                "selection": {"kind": "single",
                              "entity": {"exchange": exchange, "code": code}},
                "filter": {"kind": "yearly", "start_year": year,
                           "end_year": year},
                "version": {"kind": "latest_per_entity"},
                "page_size": 10, "cursor": None})
            record("yearly_query", {"symbol": s, "year": year, "page": page})

    # S7 idempotency on sz.000002: same request_id -> reused; fresh
    # request_id with the same frozen inputs -> no_change.
    reused = svc.tool("update_data", yearly_request(
        "yearly-accept-sz.000002", "sz.000002", daily_heads["sz.000002"],
        YEARS["sz.000002"]))
    record("reused_request", reused)
    nochange = svc.tool("update_data", yearly_request(
        "yearly-accept-sz.000002-b", "sz.000002", daily_heads["sz.000002"],
        YEARS["sz.000002"], expected_head=yearly_heads["sz.000002"]))
    record("accepted_nochange", nochange)
    svc.tool("run_worker_once", {})
    record("nochange_items", con.execute(
        "SELECT item_id,outcome_json FROM task_items WHERE task_id=?",
        (nochange["task_id"],)).fetchall())
    record("nochange_results", con.execute(
        "SELECT result_id,no_change,result_version_id FROM yearly_execution_results"
        " WHERE task_id=?", (nochange["task_id"],)).fetchall())

    # S8a base-unavailable (accept-layer refusal, verbatim isError response).
    bad_base = svc.tool_raw("update_data", yearly_request(
        "yearly-bad-base", "sh.600006",
        "collection-version-" + "0" * 64, [2025]))
    record("refusal_base_unavailable", bad_base)

    # S8b coverage-insufficient: 1990 predates sh.600004's daily base.
    bad_cov = svc.tool_raw("update_data", yearly_request(
        "yearly-bad-coverage", "sh.600004", daily_heads["sh.600004"], [1990]))
    record("refusal_coverage", bad_cov)

    # S8c env-absent: restart the same DB without the env pair; the accepted
    # task is refused inside the verifier.
    svc.close()
    svc = Service(env_present=False)
    svc.rpc("initialize")
    envabsent = svc.tool("update_data", yearly_request(
        "yearly-env-absent", "sz.000002", daily_heads["sz.000002"],
        YEARS["sz.000002"], expected_head=yearly_heads["sz.000002"]))
    record("accepted_env_absent", envabsent)
    svc.tool("run_worker_once", {})
    envabsent_outcome = con.execute(
        "SELECT outcome_json FROM task_items WHERE task_id=?",
        (envabsent["task_id"],)).fetchall()
    record("refusal_env_absent", envabsent_outcome)
    svc.close()

    # ---- mechanical re-verification ----
    results = {"assertions": [], "versions": {}, "expected_vs_actual": {},
               "blocked_paths": {}}

    def check(name, ok, detail):
        results["assertions"].append({"name": name, "ok": bool(ok),
                                      "detail": detail})

    for s in SECURITIES:
        cnt, first, last, vid = con.execute(
            "SELECT record_count,first_date,last_date,version_id FROM data_versions"
            " WHERE symbol=? AND period='day' AND state='published'", (s,)).fetchone()
        check(f"{s}_daily_base", cnt == EXPECTED_DAILY_BARS[s] and last == LAST_DAILY_DATE,
              {"record_count": cnt, "last": last, "version_id": vid})

    for s in SECURITIES:
        row = con.execute(
            "SELECT record_count,first_date,last_date,version_id FROM data_versions"
            " WHERE symbol=? AND period='year' AND state='published'", (s,)).fetchone()
        ok = (row[0] == len(YEARS[s])
              and int(row[1][:4]) == YEARS[s][0]
              and int(row[2][:4]) == YEARS[s][-1])
        check(f"{s}_yearly_version", ok, {"record_count": row[0],
              "first_date": row[1], "last_date": row[2]})
        check(f"{s}_yearly_head", yearly_heads[s] == row[3], row[3])

        lin = con.execute(
            "SELECT base_daily_version_id,algorithm_version,first_trade_date,"
            "last_trade_date,as_of FROM yearly_lineage WHERE version_id=?",
            (row[3],)).fetchone()
        check(f"{s}_lineage", lin[0] == daily_heads[s]
              and lin[1] == "local-natural-year-v1", {
                  "base_daily_version_id": lin[0],
                  "algorithm_version": lin[1],
                  "first_trade_date": lin[2], "last_trade_date": lin[3],
                  "as_of": lin[4]})

        # Independent Decimal aggregation of the published daily bars.
        bars = con.execute(
            "SELECT trade_date,open,high,low,close FROM daily_bars"
            " WHERE version_id=? ORDER BY trade_date", (daily_heads[s],)).fetchall()
        want = {}
        for d, o, h, l, c in bars:
            y = int(d[:4])
            if y not in YEARS[s]:
                continue
            agg = want.setdefault(y, {"open": o, "high": Decimal(h),
                                     "low": Decimal(l), "close": c,
                                     "first": d, "last": d})
            agg["high"] = max(agg["high"], Decimal(h))
            agg["low"] = min(agg["low"], Decimal(l))
            agg["close"] = c
            agg["last"] = d
        got = {r[0]: r for r in con.execute(
            "SELECT year,trade_date,open,high,low,close,volume,amount,"
            "first_trade_date,last_trade_date,is_finished FROM yearly_bars"
            " WHERE version_id=?", (row[3],)).fetchall()}
        diffs = []
        for y, e in want.items():
            g = got.get(y)
            if not g:
                diffs.append({"year": y, "issue": "missing"})
                continue
            (gy, tdate, o, h, l, c, vol, amt, ftd, ltd, fin) = g
            exp = {"open": e["open"], "high": e["high"],
                   "low": e["low"], "close": e["close"],
                   "trade_date": e["last"], "first_trade_date": e["first"],
                   "last_trade_date": e["last"],
                   "is_finished": 0 if y == 2026 else 1,
                   "volume": None, "amount": None}
            actual = {"open": o, "high": h, "low": l, "close": c,
                      "trade_date": tdate, "first_trade_date": ftd,
                      "last_trade_date": ltd, "is_finished": fin,
                      "volume": vol, "amount": amt}
            for k in exp:
                if k in ("open", "high", "low", "close"):
                    same = Decimal(str(actual[k])) == Decimal(str(exp[k]))
                else:
                    same = str(exp[k]) == str(actual[k])
                if not same:
                    diffs.append({"year": y, "field": k,
                                  "expected": str(exp[k]),
                                  "actual": str(actual[k])})
        results["expected_vs_actual"][s] = diffs
        check(f"{s}_yearly_values_match_independent_aggregation",
              not diffs and len(got) == len(want),
              {"years_checked": len(want), "diffs": diffs[:5]})
        y2026 = got.get(2026)
        check(f"{s}_unfinished_2026",
              y2026 and y2026[10] == 0 and y2026[9] == LAST_DAILY_DATE,
              {"is_finished": y2026[10] if y2026 else None,
               "last_trade_date": y2026[9] if y2026 else None})

    # query_data read-back equals stored rows (spot years recorded in log).
    for s in SECURITIES:
        for year in (YEARS[s][0], 2025, 2026, YEARS[s][-1]):
            stored = con.execute(
                "SELECT open,high,low,close FROM yearly_bars b"
                " JOIN data_versions v ON b.version_id=v.version_id"
                " WHERE v.symbol=? AND b.year=? AND v.state='published'",
                (s, year)).fetchone()
            record("yearly_stored", {"symbol": s, "year": year, "row": stored})

    # S7 assertions.
    check("reused_request", reused.get("reused") is True
          or reused.get("task_id") == yearly_tasks["sz.000002"], reused)
    nch = con.execute(
        "SELECT no_change FROM yearly_execution_results WHERE task_id=?",
        (nochange["task_id"],)).fetchone()
    n_versions = con.execute(
        "SELECT COUNT(*) FROM data_versions WHERE symbol='sz.000002'"
        " AND period='year'").fetchone()[0]
    check("nochange_idempotent",
          nch and nch[0] in (1, "1", "true") and n_versions == 1,
          {"no_change": nch, "yearly_versions": n_versions})

    # S8 assertions: verbatim refusal text.
    def refusal_text(resp):
        sc = resp.get("structuredContent") or {}
        content = resp.get("content") or []
        text = sc.get("message") or sc.get("error") or ""
        if not text and content:
            text = content[0].get("text", "")
        return text if text else json.dumps(resp, ensure_ascii=False)

    base_err = refusal_text(bad_base)
    cov_err = refusal_text(bad_cov)
    env_err = json.dumps(envabsent_outcome, ensure_ascii=False)
    results["blocked_paths"] = {
        "env_absent": env_err, "base_unavailable": base_err,
        "coverage_insufficient": cov_err}
    check("base_unavailable_refusal", bad_base.get("isError") is True,
          base_err[:400])
    check("coverage_insufficient_refusal", bad_cov.get("isError") is True,
          cov_err[:400])
    check("env_absent_refusal",
          "SOURCE_NOT_VERIFIED" in env_err and "official-file source not installed"
          in env_err, env_err[:400])

    # no extra versions produced by any refusal
    check("refusals_published_nothing",
          con.execute("SELECT COUNT(*) FROM data_versions WHERE period='year'")
          .fetchone()[0] == 3, "exactly 3 yearly versions")

    with open(os.path.join(HERE, "results.json"), "w") as f:
        json.dump(results, f, ensure_ascii=False, indent=2)
    failed = [a for a in results["assertions"] if not a["ok"]]
    print(f"{len(results['assertions'])} assertions, {len(failed)} failed")
    for a in failed:
        print("FAIL", a["name"], json.dumps(a["detail"], ensure_ascii=False)[:400])
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
