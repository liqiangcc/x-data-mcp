#!/usr/bin/env python3
"""Issue #219 — yearly derive + compute_metrics positive-path acceptance.

Real mcp_data stdio + official env (gen-2 inspected snapshot) over a COPY of
the sealed update5219 acceptance DB: baseline -> single update_data
market.yearly (6 explicit items, current_head/null) -> run_worker_once to
idle -> per-bar independent SQL re-aggregation + lineage -> yearly consumer
surface -> compute_metrics Computed + NotComputable legs -> provenance ->
env-absent verbatim refusal -> final count reconciliation. Assertions
Y1-Y12 per PLAN.md; every expectation is frozen or independently
recomputed, never taken from the implementation under test.
"""
import datetime
import decimal
import hashlib
import json
import os
import shutil
import sqlite3
import subprocess
import sys

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
HERE = os.path.dirname(os.path.abspath(__file__))
RUN_DIR = "/home/box/acceptance-run-yearly"
SRC_DB = "/home/box/acceptance-run-issue4-update/db.sqlite"
SNAP_DIR = "/home/box/acceptance-run-issue3/snapshot"
MANIFEST = os.path.join(REPO, "evidence/issue-3-acceptance-run/snapshot-manifest.json")
PROFILE = os.environ.get("BUILD_PROFILE", "release")
MCP_DATA = os.path.join(REPO, "target", PROFILE, "mcp_data")
DB = os.path.join(RUN_DIR, "db.sqlite")

EXPECTED = {
    "base.zip": "dcdf6b49898926675698263535021aed7ad92b34be82a1a9e8a2a8982bd68bc6",
    "gbbq.zip": "daad6290c4f1cd478d7436b1e7077260a3e3e52cefd7eae58e4f24878fa2d0e9",
    "hsjday.zip": "c82f852a7bc2e52c7e89347a1e0d1878b01ce997946c2745f7af323752270892",
    "manifest": "b8a0d7b9aec1a270549d1e53ba26c0ab04842a23c05d1d1f7e75dd44a9f444d3",
}
BLOCKERS = [
    "GBBQ_QFQ_NOT_VERIFIED",
    "FILE_ACCESS_USE_NOT_VERIFIED",
    "FILE_SET_CONSISTENCY_NOT_VERIFIED",
]
BASELINE_COUNTS = {
    "data_versions": 5219, "daily_bars": 16629206, "effective_heads": 5219,
    "market_update_results": 5219, "yearly_bars": 0, "yearly_lineage": 0,
    "tasks": 109,
}
CURRENT_YEAR = 2026  # Shanghai year of the frozen run date 2026-09-18

SECURITIES = {
    "sz.000002": {
        "entity": {"exchange": "sz", "code": "000002"},
        "base": "collection-version-e75dbaa86f6913776c0ad7847c9d6e3fd222f67e5f8865dd7a6082859123e5b0",
        "years": list(range(1991, 2027)),
    },
    "sh.600006": {
        "entity": {"exchange": "sh", "code": "600006"},
        "base": "collection-version-20312846826be3e15b3fbb413b9037d85a457bb9384f56c906fd7d9174f7dfbd",
        "years": list(range(1999, 2027)),
    },
    "sh.600004": {
        "entity": {"exchange": "sh", "code": "600004"},
        "base": "collection-version-69e9b01833330f412909066752f35c4eb93be816be4f7dbaa8d6bacff8adc171",
        "years": list(range(2003, 2027)),
    },
    "sz.000001": {
        "entity": {"exchange": "sz", "code": "000001"},
        "base": "collection-version-eac2c3f1543d0213af7ef4388773269f148b09610abaa4d17b015eb4418ed5a1",
        "years": list(range(1991, 2027)),
    },
    "sz.000035": {
        "entity": {"exchange": "sz", "code": "000035"},
        "base": "collection-version-31370cfe4b048be5d054af8e6ae1592c1c0db954c97bc42dd2ef07d8c4624d65",
        "years": [y for y in range(1994, 2027) if y != 2012],
    },
    "sh.688318": {
        "entity": {"exchange": "sh", "code": "688318"},
        "base": "collection-version-19111da225f71cfb898afdfa724bd47cd269df94c0a07d35dcfdc49527f1e54c",
        "years": list(range(2020, 2027)),
    },
}
EXPECTED_YEARLY_BARS = sum(len(s["years"]) for s in SECURITIES.values())  # 163

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


def shanghai_today():
    return (datetime.datetime.now(datetime.timezone.utc)
            + datetime.timedelta(hours=8)).date().isoformat()


def counts(db):
    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    out = {t: con.execute(f"SELECT COUNT(*) FROM {t}").fetchone()[0]
           for t in BASELINE_COUNTS}
    con.close()
    return out


def reaggregate(db, base_version_id):
    """Independent yearly re-aggregation of a daily version. Returns
    {year: (trade_date, open, high, low, close, first_td, last_td)}."""
    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
    rows = con.execute(
        "SELECT trade_date,open,high,low,close FROM daily_bars "
        "WHERE version_id=? ORDER BY trade_date", (base_version_id,)
    ).fetchall()
    con.close()
    years = {}
    for d, o, h, l, c in rows:
        y = int(d[:4])
        e = years.setdefault(y, {"first_d": d, "last_d": d, "open": o,
                                 "close": c, "high": decimal.Decimal(h),
                                 "low": decimal.Decimal(l)})
        e["last_d"] = d
        e["close"] = c
        e["high"] = max(e["high"], decimal.Decimal(h))
        e["low"] = min(e["low"], decimal.Decimal(l))
    return {y: (e["last_d"], decimal.Decimal(e["open"]), e["high"],
                e["low"], decimal.Decimal(e["close"]),
                e["first_d"], e["last_d"]) for y, e in years.items()}


class Service:
    def __init__(self, db, env_mode="present"):
        env = dict(os.environ)
        env.pop("X_DATA_MCP_OFFICIAL_MANIFEST", None)
        env.pop("X_DATA_MCP_OFFICIAL_DIR", None)
        # dev-only: overlayfs bypass for any per-task fs guard (PR #213)
        env["X_DATA_MCP_UNSAFE_NONLOCAL_FS"] = "1"
        if env_mode == "present":
            env["X_DATA_MCP_OFFICIAL_MANIFEST"] = MANIFEST
            env["X_DATA_MCP_OFFICIAL_DIR"] = SNAP_DIR
        self.proc = subprocess.Popen(
            [MCP_DATA, db], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
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
        return resp

    def tool(self, name, arguments):
        resp = self.tool_raw(name, arguments)
        result = resp.get("result", {})
        if resp.get("error") or result.get("isError"):
            record("tool_error", {"name": name, "response": resp})
            raise SystemExit(f"tool {name} failed: {json.dumps(resp)[:2000]}")
        return result.get("structuredContent")

    def close(self):
        self.proc.stdin.close()
        stderr = self.proc.stderr.read()
        self.proc.wait()
        record("service_exit", {"returncode": self.proc.returncode,
                                "stderr": stderr})


def copy_db(src, dst):
    for stale in (dst, dst + "-wal", dst + "-shm"):
        if os.path.exists(stale):
            os.remove(stale)
    shutil.copyfile(src, dst)
    for suffix in ("-wal", "-shm"):
        if os.path.exists(src + suffix):
            shutil.copyfile(src + suffix, dst + suffix)
    record("db_copied", {"src": src, "dst": dst,
                         "src_sha256": sha256_file(src)})


def main():
    for name in ("base.zip", "gbbq.zip", "hsjday.zip"):
        if not os.path.isfile(os.path.join(SNAP_DIR, name)):
            raise SystemExit(f"snapshot member absent: {SNAP_DIR}/{name}")
    if not os.path.isfile(SRC_DB):
        raise SystemExit(f"sealed acceptance db absent: {SRC_DB}")
    os.makedirs(RUN_DIR, exist_ok=True)

    results = {"assertions": [], "blocked_paths": {}}

    def check(name, ok, detail):
        results["assertions"].append({"name": name, "ok": bool(ok),
                                      "detail": detail})
        record("assertion", results["assertions"][-1])

    # ---- S0 environment + input integrity ----
    head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=REPO,
                          capture_output=True, text=True).stdout.strip()
    arch = {n: sha256_file(os.path.join(SNAP_DIR, n))
            for n in ("base.zip", "gbbq.zip", "hsjday.zip")}
    manifest_sha = sha256_file(MANIFEST)
    today = shanghai_today()
    fp = {"head": head, "mcp_data_sha256": sha256_file(MCP_DATA),
          "manifest_sha256": manifest_sha, "archive_sha256": arch,
          "shanghai_today": today, "src_db": SRC_DB}
    record("env", fp)
    check("Y1_input_integrity",
          arch == {k: EXPECTED[k] for k in
                   ("base.zip", "gbbq.zip", "hsjday.zip")}
          and manifest_sha == EXPECTED["manifest"]
          and today.startswith(f"{CURRENT_YEAR}-"),
          {"manifest_sha256": manifest_sha, "shanghai_today": today})

    # ---- S1 copy sealed db + baseline ----
    copy_db(SRC_DB, DB)
    before = counts(DB)
    record("baseline", before)
    check("Y1b_baseline", before == BASELINE_COUNTS, before)

    # ---- S2 verify frozen year sets against actual daily coverage ----
    con = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
    coverage = {}
    for sym, spec in SECURITIES.items():
        coverage[sym] = sorted(int(r[0]) for r in con.execute(
            "SELECT DISTINCT substr(trade_date,1,4) FROM daily_bars "
            "WHERE version_id=? ORDER BY 1", (spec["base"],)).fetchall())
    con.close()
    check("Y1c_frozen_years",
          all(coverage[sym] == spec["years"]
              for sym, spec in SECURITIES.items())
          and EXPECTED_YEARLY_BARS == 163
          and 2012 not in coverage["sz.000035"],
          {s: len(y) for s, y in coverage.items()})

    svc = Service(DB, "present")
    try:
        svc.rpc("initialize")

        # ---- S3 single yearly update_data, 6 explicit items ----
        items = [{"entity": s["entity"],
                  "base_daily_version_id": s["base"],
                  "years": s["years"],
                  "publication": {"mode": "current_head",
                                  "expected_yearly_head": None}}
                 for s in SECURITIES.values()]
        acc = svc.tool("update_data", {
            "request_id": "yearly-001", "dataset": "market.yearly",
            "contract_version": "v1", "items": items})
        record("accepted", acc)
        item_ids = acc.get("item_ids") or []
        check("Y2_acceptance",
              bool(acc.get("task_id")) and len(item_ids) == 6,
              {"task_id": acc.get("task_id"), "item_ids": len(item_ids)})

        # ---- S4 drain worker ----
        non_idle = 0
        while True:
            w = svc.tool("run_worker_once", {})
            record("worker_once", {"seq": non_idle, "result": w})
            if w.get("idle"):
                break
            non_idle += 1
            if non_idle > 60:
                raise SystemExit("worker did not reach idle after 60 runs")
        record("worker_drained", {"non_idle_runs": non_idle})

        # ---- S5 item outcomes + version growth ----
        con = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
        outcomes = con.execute(
            "SELECT item_id,target_json,outcome_json FROM task_items "
            "WHERE task_id=? ORDER BY item_id", (acc["task_id"],)).fetchall()
        succeeded = [o for o in outcomes
                     if o[2] and json.loads(o[2]).get("success") is True]
        yearly_vers = con.execute(
            "SELECT version_id,symbol,record_count,state,source,compat_json,"
            "coverage_status,coverage_reason,first_date,last_date "
            "FROM data_versions WHERE period='year'").fetchall()
        after_counts = counts(DB)
        con.close()
        ver_by_sym = {r[1]: r for r in yearly_vers}
        check("Y3_execution",
              len(succeeded) == 6
              and len(yearly_vers) == 6
              and set(ver_by_sym) == set(SECURITIES)
              and all(r[2] == len(SECURITIES[r[1]]["years"]) for r in yearly_vers)
              and all(r[3] == "published" for r in yearly_vers)
              and after_counts["yearly_bars"] == EXPECTED_YEARLY_BARS
              and after_counts["data_versions"]
              == BASELINE_COUNTS["data_versions"] + 6,
              {"succeeded": len(succeeded), "versions": len(yearly_vers),
               "yearly_bars": after_counts["yearly_bars"]})

        # ---- S6 per-bar independent re-aggregation ----
        con = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
        mismatches = []
        checked_bars = 0
        for sym, spec in SECURITIES.items():
            vid = ver_by_sym[sym][0]
            expected = reaggregate(DB, spec["base"])
            got = {r[0]: r for r in con.execute(
                "SELECT year,trade_date,open,high,low,close,volume,amount,"
                "first_trade_date,last_trade_date,is_finished "
                "FROM yearly_bars WHERE version_id=? ORDER BY year",
                (vid,)).fetchall()}
            for y in spec["years"]:
                if y not in got:
                    mismatches.append((sym, y, "missing"))
                    continue
                (_y, td, o, h, l, c, vol, amt, ftd, ltd, fin) = got[y]
                e = expected[y]
                checked_bars += 1
                if not (td == e[0] and decimal.Decimal(o) == e[1]
                        and decimal.Decimal(h) == e[2]
                        and decimal.Decimal(l) == e[3]
                        and decimal.Decimal(c) == e[4]
                        and ftd == e[5] and ltd == e[6]
                        and vol is None and amt is None):
                    mismatches.append((sym, y, "value", {
                        "got": [td, o, h, l, c, ftd, ltd],
                        "want": [e[0], str(e[1]), str(e[2]), str(e[3]),
                                 str(e[4]), e[5], e[6]]}))
                if fin != (0 if y == CURRENT_YEAR else 1):
                    mismatches.append((sym, y, "is_finished", fin))
            if set(got) - set(spec["years"]):
                mismatches.append((sym, "extra",
                                   sorted(set(got) - set(spec["years"]))))
        con.close()
        check("Y4_per_bar_reaggregation",
              not mismatches and checked_bars == EXPECTED_YEARLY_BARS,
              {"checked": checked_bars, "mismatches": mismatches[:8]})

        # ---- S7 lineage ----
        vid2sym = {r[0]: r[1] for r in yearly_vers}
        con = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
        lin = {vid2sym[r[0]]: r for r in con.execute(
            "SELECT version_id,base_daily_version_id,algorithm_version,"
            "first_trade_date,last_trade_date,as_of "
            "FROM yearly_lineage").fetchall() if r[0] in vid2sym}
        con.close()
        lin_bad = []
        for sym, spec in SECURITIES.items():
            row = lin.get(sym)
            exp = reaggregate(DB, spec["base"])
            if not row:
                lin_bad.append((sym, "absent"))
                continue
            firsts = sorted(e[5] for e in exp.values())
            lasts = sorted(e[6] for e in exp.values())
            if not (row[1] == spec["base"]
                    and row[2] == "local-natural-year-v1"
                    and row[3] == firsts[0] and row[4] == lasts[-1]
                    and row[5] == today):
                lin_bad.append((sym, row))
        check("Y5_lineage",
              len(lin) == 6 and not lin_bad
              and after_counts["yearly_lineage"] == 6,
              {"lineage_rows": len(lin), "bad": lin_bad[:4]})
    finally:
        svc.close()

    # ---- S8-S10 need the service; restart for consumer/compute legs ----
    svc = Service(DB, "present")
    try:
        svc.rpc("initialize")
        sym2vid = {r[1]: r[0] for r in yearly_vers}

        def yearly_query(entity, start_year, end_year, version, cursor=None):
            return svc.tool("query_data", {
                "dataset": "market.yearly", "contract_version": "v1",
                "selection": {"kind": "single", "entity": entity},
                "filter": {"kind": "yearly", "start_year": start_year,
                           "end_year": end_year},
                "version": version, "page_size": 1000, "cursor": cursor})

        # ---- S8 consumer surface ----
        spec = SECURITIES["sz.000002"]
        page = yearly_query(spec["entity"], 1991, 2026,
                            {"kind": "latest_per_entity"})
        recs = page["records"]
        dates = [r.get("date") or r.get("trade_date") for r in recs]
        exp = reaggregate(DB, spec["base"])
        vers = svc.tool("list_data_versions", {
            "dataset": "market.yearly", "contract_version": "v1",
            "selection": {"kind": "single", "entity": spec["entity"]},
            "page_size": 10, "cursor": None})
        avail = svc.tool("get_data_availability", {
            "dataset": "market.yearly", "contract_version": "v1",
            "selection": {"kind": "single", "entity": spec["entity"]},
            "filter": {"kind": "yearly", "start_year": 1991,
                       "end_year": 2026},
            "version": {"kind": "latest_per_entity"},
            "page_size": 10, "cursor": None})
        vitems = vers.get("items", [])
        aitems = avail.get("items", [])
        a0 = aitems[0] if aitems else {}
        check("Y6_consumer_surface",
              len(recs) == 36
              and dates == sorted(dates)
              and dates[0] == exp[1991][0] and dates[-1] == exp[2026][0]
              and all(r["version_ref"]["version_id"] == sym2vid["sz.000002"]
                      for r in recs)
              and len(vitems) == 1
              and vitems[0]["version_ref"]["version_id"]
              == sym2vid["sz.000002"]
              and len(vitems[0]["version_ref"].get(
                  "integrity_sha256", "")) == 64
              and len(aitems) >= 1
              and a0.get("availability") != "complete",
              {"records": len(recs), "first": dates[0] if dates else None,
               "last": dates[-1] if dates else None,
               "version_items": len(vitems),
               "availability": a0.get("availability"),
               "evidence": str(a0.get("coverage_evidence"))[:140]})

        # ---- S9 compute legs ----
        def compute(sym, start, end, direction="down", min_periods=3,
                    completed_only=True):
            con = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
            sha = con.execute(
                "SELECT content_digest FROM data_versions WHERE version_id=?",
                (sym2vid[sym],)).fetchone()[0]
            con.close()
            return svc.tool_raw("compute_metrics", {
                "contract_version": "v1", "execution": "inline",
                "inputs": [{"input_id": "prices", "version_ref": {
                    "dataset": "market.yearly", "contract_version": "v1",
                    "entity": SECURITIES[sym]["entity"],
                    "version_id": sym2vid[sym],
                    "integrity_sha256": sha}}],
                "range": {"start_year": start, "end_year": end},
                "metrics": [{"metric_id": "m1",
                             "name": "consecutive_direction",
                             "algorithm_version":
                                 "local-consecutive-direction-v1",
                             "input_ids": ["prices"],
                             "parameters": {
                                 "direction": direction,
                                 "min_periods": min_periods,
                                 "completed_only": completed_only}}]})

        def outcome(resp):
            # not_computable arrives as isError with details.result;
            # computed would arrive as plain structuredContent.result.
            sc = resp.get("result", {}).get("structuredContent") or {}
            return ((sc.get("details") or {}).get("result")
                    or sc.get("result") or {})

        # Architectural fact this acceptance verifies: `computed` is
        # unreachable on production-source data. Completion evidence comes
        # only from `analysis_coverage_v1`, written solely by fixture/demo
        # code and hard-rejected on non-fixture sources ("fixture coverage
        # cannot certify a production source"). Real yearly versions yield
        # Completion::Unknown per observation, so the metric honestly
        # refuses. The legs below verify that refusal boundary plus the
        # check ordering that still yields the more-specific reasons where
        # they precede the completion check.

        # (a) honest refusal: sz.000002 2019-2025 -> unknown_completion,
        #     deterministic, fixture-only limitation verbatim
        r1 = compute("sz.000002", 2019, 2025)
        r1b = compute("sz.000002", 2019, 2025)
        o1 = outcome(r1)
        lim1 = o1.get("limitations") or []
        check("Y7_compute_honest_refusal",
              o1.get("outcome") == "not_computable"
              and o1.get("reason") == "unknown_completion"
              and any("fixture-only" in s for s in lim1)
              and o1 == outcome(r1b),
              {"outcome": o1,
               "repeat_identical": o1 == outcome(r1b)})

        # (b) non_positive_price precedes completion check: 2005-2008
        o2 = outcome(compute("sz.000002", 2005, 2008))
        # (c) missing_year at range end precedes observation checks:
        #     sz.000035 range 2010-2012 (2012 has no yearly bar)
        o3 = outcome(compute("sz.000035", 2010, 2012))
        # (d) completed_only still honored inside the refusal:
        #     2019-2026 -> excluded_tail carries 2026
        o4 = outcome(compute("sz.000002", 2019, 2026))
        check("Y8_compute_edges",
              o2.get("outcome") == "not_computable"
              and o2.get("reason") == "non_positive_price"
              and o3.get("outcome") == "not_computable"
              and o3.get("reason") == "missing_year"
              and o4.get("outcome") == "not_computable"
              and o4.get("reason") == "unknown_completion"
              and o4.get("excluded_tail") == [2026],
              {"nonpositive": o2, "missing_year": o3,
               "completed_only_tail": o4.get("excluded_tail")})

        # (e) version/digest binding: bogus version_id + sha rejected,
        #     never falls back to latest
        bogus = svc.tool_raw("compute_metrics", {
            "contract_version": "v1", "execution": "inline",
            "inputs": [{"input_id": "prices", "version_ref": {
                "dataset": "market.yearly", "contract_version": "v1",
                "entity": SECURITIES["sz.000002"]["entity"],
                "version_id": "yearly-version-bogus",
                "integrity_sha256": "0" * 64}}],
            "range": {"start_year": 2019, "end_year": 2025},
            "metrics": [{"metric_id": "m1", "name": "consecutive_direction",
                         "algorithm_version":
                             "local-consecutive-direction-v1",
                         "input_ids": ["prices"],
                         "parameters": {"direction": "down",
                                        "min_periods": 3}}]})
        bogus_failed = bool(bogus.get("error")) or bool(
            bogus.get("result", {}).get("isError"))
        check("Y8b_compute_binding", bogus_failed,
              {"bogus_rejected": bogus_failed})

        # ---- S10 provenance verbatim ----
        prov_bad = []
        for sym, spec in SECURITIES.items():
            compat = json.loads(ver_by_sym[sym][5] or "{}")
            blob = json.dumps(compat)
            row = ver_by_sym[sym]
            if not (spec["base"] in blob
                    and "NOT_PRODUCTION_READY" in blob
                    and row[6] is not None and row[7] is not None):
                prov_bad.append((sym, str(compat)[:200]))
        check("Y9_provenance", not prov_bad,
              {"bad": prov_bad,
               "coverage": [r[6] for r in yearly_vers]})
    finally:
        svc.close()

    # ---- S11 env-absent verbatim refusal on the same copy ----
    svc = Service(DB, "absent")
    try:
        svc.rpc("initialize")
        acc2 = svc.tool_raw("update_data", {
            "request_id": "yearly-envabsent", "dataset": "market.yearly",
            "contract_version": "v1",
            "items": [{"entity": SECURITIES["sh.600006"]["entity"],
                       "base_daily_version_id":
                           SECURITIES["sh.600006"]["base"],
                       "years": [2020],
                       "publication": {"mode": "current_head",
                                       "expected_yearly_head": None}}]})
        err_blob = json.dumps(acc2, ensure_ascii=False)
        task_err = None
        if not acc2.get("error") and not acc2.get("result", {}).get("isError"):
            sc = acc2.get("result", {}).get("structuredContent") or {}
            tid = sc.get("task_id")
            while True:
                w = svc.tool("run_worker_once", {})
                if w.get("idle"):
                    break
            if tid:
                con = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
                task_err = json.dumps(con.execute(
                    "SELECT outcome_json FROM task_items WHERE task_id=?",
                    (tid,)).fetchall())
                con.close()
        refused = ("official-file source not installed"
                   in (err_blob + (task_err or "")))
        record("env_absent", {"acceptance": acc2, "task_outcomes": task_err})
        check("Y10_env_absent_refusal", refused,
              {"verbatim_present": refused,
               "task_outcomes": (task_err or "")[:400]})
    finally:
        svc.close()

    # ---- S12 read-side invariance + gates ----
    final = counts(DB)
    con = sqlite3.connect(f"file:{DB}?mode=ro", uri=True)
    task_delta = final["tasks"] - BASELINE_COUNTS["tasks"]
    heads_by_period = dict(con.execute(
        "SELECT period,COUNT(*) FROM effective_heads GROUP BY period"
    ).fetchall())
    con.close()
    svc = Service(DB, "present")
    try:
        svc.rpc("initialize")
        caps = svc.tool("get_data_capabilities", {})
    finally:
        svc.close()
    check("Y11_invariance_gates",
          final["daily_bars"] == BASELINE_COUNTS["daily_bars"]
          and heads_by_period.get("day") == 5219
          and heads_by_period.get("year") == 6
          and final["market_update_results"]
          == BASELINE_COUNTS["market_update_results"]
          and final["data_versions"] == BASELINE_COUNTS["data_versions"] + 6
          and caps.get("production_ready") is False
          and all(b in json.dumps(caps) for b in BLOCKERS),
          {"final": final, "heads_by_period": heads_by_period,
           "task_delta": task_delta,
           "production_ready": caps.get("production_ready")})

    results["run_identity"] = fp
    results["baseline"] = {"before": before, "after": final}
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
