#!/usr/bin/env python3
"""Issue #4 update branch — 5219-security same-generation update_data
no-change acceptance.

Real mcp_data stdio + official env on the retained gen-2 snapshot over a
COPY of the sealed v2b acceptance DB: baseline mechanical queries ->
update_data {"selection":{"kind":"default"}} verbatim budget rejection
(BLOCKED_ON_IMPL) -> 53 sharded explicit update_data
(update-000..update-052, <=100 items each, frozen base_version_id +
range per item) -> run_worker_once to idle -> no-change / zero-write
assertions -> env-absent refusal leg on the same copy -> gate verbatim.
Assertions U1-U12 per PLAN.md; mechanical expectations recomputed from
the sealed DB and retained bytes, never from the implementation under
test.
"""
import datetime
import hashlib
import json
import os
import shutil
import sqlite3
import subprocess
import sys

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
HERE = os.path.dirname(os.path.abspath(__file__))
RUN_DIR = "/home/box/acceptance-run-issue4-update"
SRC_DB = "/home/box/acceptance-run-issue4/db.sqlite"
SNAP_DIR = "/home/box/acceptance-run-issue3/snapshot"
MANIFEST = os.path.join(REPO, "evidence/issue-3-acceptance-run/snapshot-manifest.json")
PROFILE = os.environ.get("BUILD_PROFILE", "release")
MCP_DATA = os.path.join(REPO, "target", PROFILE, "mcp_data")
TASK_SQLITE = os.path.join(REPO, "target", PROFILE, "task_sqlite")
EXPECTED = {  # gen-2 pins, same as the first-collection acceptance
    "base.zip": "dcdf6b49898926675698263535021aed7ad92b34be82a1a9e8a2a8982bd68bc6",
    "gbbq.zip": "daad6290c4f1cd478d7436b1e7077260a3e3e52cefd7eae58e4f24878fa2d0e9",
    "hsjday.zip": "c82f852a7bc2e52c7e89347a1e0d1878b01ce997946c2745f7af323752270892",
    "manifest": "b8a0d7b9aec1a270549d1e53ba26c0ab04842a23c05d1d1f7e75dd44a9f444d3",
}
SPOT = {"sh.600004", "sh.600006", "sz.000002"}
EXPECTED_WORKSET = 5219
# Default-selection workset counts all 5224 published catalog securities:
# the budget guard fires before per-item head binding (measured verbatim).
EXPECTED_DEFAULT_WORKSET = 5224
EXPECTED_BARS = 16629206
NO_DAY = ["sh.601091", "sh.688837", "sz.001246", "sz.301686", "sz.301716"]

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


class Service:
    def __init__(self, db, env_mode="present"):
        env = dict(os.environ)
        env.pop("X_DATA_MCP_OFFICIAL_MANIFEST", None)
        env.pop("X_DATA_MCP_OFFICIAL_DIR", None)
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


def baseline(db):
    con = sqlite3.connect(db)
    heads = dict(con.execute(
        "SELECT symbol,version_id FROM effective_heads").fetchall())
    first_dates = dict(con.execute(
        "SELECT version_id,first_date FROM data_versions").fetchall())
    counts = {
        "data_versions": con.execute(
            "SELECT COUNT(*) FROM data_versions").fetchone()[0],
        "published": con.execute(
            "SELECT COUNT(*) FROM data_versions WHERE state='published'"
        ).fetchone()[0],
        "effective_heads": con.execute(
            "SELECT COUNT(*) FROM effective_heads").fetchone()[0],
        "daily_bars": con.execute(
            "SELECT COUNT(*) FROM daily_bars").fetchone()[0],
        "market_update_results": con.execute(
            "SELECT COUNT(*) FROM market_update_results").fetchone()[0],
    }
    spot_bars = {}
    for sym in SPOT:
        vid = heads.get(sym)
        spot_bars[sym] = con.execute(
            "SELECT trade_date,open,high,low,close,volume,amount "
            "FROM daily_bars WHERE version_id=? ORDER BY trade_date",
            (vid,)).fetchall() if vid else None
    con.close()
    return {"heads": heads, "first_dates": first_dates,
            "counts": counts, "spot_bars": spot_bars}


def outcomes(db, task_id):
    con = sqlite3.connect(db)
    rows = con.execute(
        "SELECT item_id,target_json,stage,outcome_json FROM task_items "
        "WHERE task_id=? ORDER BY item_id", (task_id,)).fetchall()
    con.close()
    return [{"item_id": r[0], "target": json.loads(r[1]), "stage": r[2],
             "outcome": json.loads(r[3]) if r[3] else None} for r in rows]


def update_request(request_id, items):
    return {"request_id": request_id, "dataset": "market.daily",
            "contract_version": "v1", "items": items}


def main():
    for name in ("base.zip", "gbbq.zip", "hsjday.zip"):
        if not os.path.isfile(os.path.join(SNAP_DIR, name)):
            raise SystemExit(f"snapshot member absent: {SNAP_DIR}/{name}")
    if not os.path.isfile(SRC_DB):
        raise SystemExit(f"sealed acceptance db absent: {SRC_DB}")
    os.makedirs(RUN_DIR, exist_ok=True)

    # ---- S0 environment + input integrity ----
    head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=REPO,
                          capture_output=True, text=True).stdout.strip()
    cargo = subprocess.run(
        [os.path.expanduser("~/.cargo/bin/cargo"), "--version"],
        capture_output=True, text=True).stdout.strip()
    arch = {n: sha256_file(os.path.join(SNAP_DIR, n))
            for n in ("base.zip", "gbbq.zip", "hsjday.zip")}
    manifest_sha = sha256_file(MANIFEST)
    today = shanghai_today()
    record("env", {"run_dir": RUN_DIR, "head": head, "cargo": cargo,
                   "mcp_data_sha256": sha256_file(MCP_DATA),
                   "task_sqlite_sha256": sha256_file(TASK_SQLITE),
                   "manifest_sha256": manifest_sha, "archive_sha256": arch,
                   "shanghai_today": today})

    results = {"assertions": [], "blocked_paths": {},
               "run_identity": {"head": head, "cargo": cargo,
                                "manifest_sha256": manifest_sha,
                                "archive_sha256": arch,
                                "shanghai_today": today}}

    def check(name, ok, detail):
        results["assertions"].append({"name": name, "ok": bool(ok),
                                      "detail": detail})

    check("U1_input_integrity",
          arch == {k: EXPECTED[k] for k in
                   ("base.zip", "gbbq.zip", "hsjday.zip")}
          and manifest_sha == EXPECTED["manifest"],
          {"archive_sha256": arch, "manifest_sha256": manifest_sha})

    # ---- S1 copy sealed db + mechanical baseline ----
    db = os.path.join(RUN_DIR, "db.sqlite")
    copy_db(SRC_DB, db)
    base = baseline(db)
    with open(os.path.join(RUN_DIR, "baseline.json"), "w") as f:
        json.dump({"counts": base["counts"], "heads": base["heads"],
                   "first_dates": base["first_dates"]}, f, indent=1)
    spot_baseline = {s: [tuple(r) for r in rows]
                     for s, rows in base["spot_bars"].items()}
    record("baseline", {"counts": base["counts"],
                        "distinct_head_symbols": len(set(base["heads"])),
                        "spot_bar_counts": {
                            s: len(v) for s, v in spot_baseline.items()}})
    check("U1b_baseline_workset",
          len(base["heads"]) == EXPECTED_WORKSET
          and len(set(base["heads"])) == EXPECTED_WORKSET
          and base["counts"]["data_versions"] == EXPECTED_WORKSET
          and base["counts"]["daily_bars"] == EXPECTED_BARS
          and base["counts"]["market_update_results"] == 0
          and all(sym not in base["heads"] for sym in NO_DAY)
          and all(base["first_dates"].get(vid) for vid in
                  base["heads"].values()),
          {"counts": base["counts"],
           "distinct_head_symbols": len(set(base["heads"]))})

    svc = Service(db, "present")
    svc.rpc("initialize")

    # ---- S3 default selection budget rejection (BLOCKED_ON_IMPL) ----
    default_resp = svc.tool_raw("update_data", {
        "request_id": "u-default", "dataset": "market.daily",
        "contract_version": "v1",
        "selection": {"kind": "default"}})
    record("S3_default_selection", default_resp)
    con = sqlite3.connect(db)
    s3_tasks = con.execute("SELECT COUNT(*) FROM tasks").fetchone()[0]
    con.close()
    s3_err = json.dumps(
        default_resp.get("result", {}).get("structuredContent")
        or default_resp.get("error") or {}, ensure_ascii=False)
    expected_budget = ("default security budget exceeded: "
                       f"{EXPECTED_DEFAULT_WORKSET} > 100")
    check("U2_default_budget",
          (default_resp.get("result", {}).get("isError")
           or default_resp.get("error"))
          and expected_budget in s3_err,
          {"response": default_resp, "task_count_before": s3_tasks})
    results["blocked_paths"]["S3_default_selection"] = default_resp.get(
        "result", {}).get("structuredContent") or default_resp.get("error")

    # ---- S4 build 53 sharded update items from the baseline ----
    symbols = sorted(base["heads"])
    items = [{
        "entity": {"exchange": sym.split(".")[0], "code": sym.split(".")[1]},
        "base_version_id": base["heads"][sym],
        "start_date": base["first_dates"][base["heads"][sym]],
        "end_date": today,
    } for sym in symbols]
    shards = [items[i:i + 100] for i in range(0, len(items), 100)]
    with open(os.path.join(RUN_DIR, "shards.json"), "w") as f:
        json.dump(shards, f, ensure_ascii=False, indent=1)
    record("shards", {"count": len(shards),
                      "sizes": [len(s) for s in shards]})

    # ---- S5 accept shards + drain worker ----
    accepts = []
    for i, shard in enumerate(shards):
        rid = f"update-{i:03d}"
        acc = svc.tool("update_data", update_request(rid, shard))
        accepts.append(acc)
        record("accepted_shard", {"request_id": rid, "acceptance": acc,
                                  "shard_size": len(shard)})
    total_items = sum(len(a.get("item_ids") or []) for a in accepts)
    check("U3_sharded_acceptance",
          len(accepts) == 53
          and all(len(a.get("item_ids") or []) <= 100 for a in accepts)
          and total_items == EXPECTED_WORKSET,
          {"tasks": len(accepts), "total_items": total_items})

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

    # ---- S6 assertions ----
    failed_items = []
    success_items = 0
    for a in accepts:
        for item in outcomes(db, a["task_id"]):
            o = item["outcome"] or {}
            if o.get("success") is True:
                success_items += 1
            else:
                failed_items.append(
                    {"task_id": a["task_id"], "item_id": item["item_id"],
                     "symbol": item["target"].get("symbol"),
                     "error": o.get("error")})
    record("item_outcome_summary",
           {"success": success_items, "failed": failed_items})

    con = sqlite3.connect(db)
    update_stats = con.execute(
        "SELECT COUNT(*), SUM(no_change), "
        " SUM(result_version_id != base_version_id) "
        "FROM market_update_results").fetchone()
    result_task_ids = {r[0] for r in con.execute(
        "SELECT DISTINCT task_id FROM market_update_results").fetchall()}
    audit_stats = con.execute(
        "SELECT COUNT(*), "
        " SUM(json_extract(audit_json,'$.no_change') != 1), "
        " SUM(json_extract(audit_json,'$.combined') IS NOT NULL), "
        " SUM(json_extract(audit_json,'$.evidence.base_version_id') IS NULL), "
        " SUM(json_extract(audit_json,'$.evidence.same_qfq_basis_ref') IS NULL) "
        "FROM market_update_results").fetchone()
    audit_spot = con.execute(
        "SELECT r.item_id, json_extract(r.audit_json,'$.evidence.base_version_id'),"
        " json_extract(r.audit_json,'$.evidence.same_qfq_basis_ref'),"
        " json_extract(r.audit_json,'$.evidence.revision_ref'),"
        " r.result_sha256 "
        "FROM market_update_results r LIMIT 3").fetchall()
    after = baseline(db)
    task_states = con.execute(
        "SELECT status, COUNT(*) FROM tasks "
        "WHERE task_id IN (SELECT DISTINCT task_id FROM market_update_results) "
        "GROUP BY status").fetchall()
    con.close()

    check("U4_item_outcomes",
          success_items == EXPECTED_WORKSET and not failed_items,
          {"success": success_items, "failed": failed_items[:10]})
    check("U5_no_change_semantics",
          update_stats[0] == EXPECTED_WORKSET
          and update_stats[1] == EXPECTED_WORKSET
          and update_stats[2] == 0
          and result_task_ids == {a["task_id"] for a in accepts},
          {"update_results": update_stats[0], "no_change": update_stats[1],
           "version_mismatch": update_stats[2]})
    check("U6_zero_new_versions",
          after["counts"]["data_versions"] == EXPECTED_WORKSET
          and after["counts"]["published"] == EXPECTED_WORKSET
          and after["heads"] == base["heads"],
          {"counts_before": base["counts"],
           "counts_after": after["counts"]})
    spot_after = {s: [tuple(r) for r in rows]
                  for s, rows in after["spot_bars"].items()}
    check("U7_data_unchanged",
          after["counts"]["daily_bars"] == EXPECTED_BARS
          and spot_after == spot_baseline
          and all(spot_baseline[s] for s in SPOT),
          {"daily_bars_after": after["counts"]["daily_bars"],
           "spot_bar_counts": {s: len(v) for s, v in spot_after.items()}})
    check("U8_provenance",
          audit_stats[0] == EXPECTED_WORKSET
          and all(v == 0 for v in audit_stats[1:])
          and all(EXPECTED["manifest"][:20] in (r[2] or "") for r in audit_spot)
          and all(r[3] is None for r in audit_spot),
          {"audit_rows": audit_stats[0],
           "no_change_not_1": audit_stats[1],
           "combined_not_null": audit_stats[2],
           "missing_base_version": audit_stats[3],
           "missing_qfq_basis": audit_stats[4],
           "audit_spot": audit_spot})
    check("U9_task_bookkeeping",
          task_states == [("succeeded", 53)],
          {"task_states": task_states})

    # ---- S7 env-absent refusal leg on the same copy ----
    svc.close()
    s1 = Service(db, "absent")
    s1.rpc("initialize")
    r_items = update_request("u-r1", [items[0]])
    a1 = s1.tool("update_data", r_items)
    record("R1_accept", a1)
    s1.tool("run_worker_once", {})
    r1_items = outcomes(db, a1["task_id"])
    r1_counts = baseline(db)["counts"]
    record("R1_refusal", {"items": r1_items, "counts": r1_counts})
    results["blocked_paths"]["R1_env_absent"] = [
        i["outcome"] for i in r1_items]
    check("U10_env_absent",
          len(r1_items) == 1
          and (r1_items[0]["outcome"] or {}).get("success") is False
          and "SOURCE_NOT_VERIFIED" in
          ((r1_items[0]["outcome"] or {}).get("error") or "")
          and "GBBQ_QFQ_NOT_VERIFIED" in
          ((r1_items[0]["outcome"] or {}).get("error") or "")
          and "FILE_ACCESS_USE_NOT_VERIFIED" in
          ((r1_items[0]["outcome"] or {}).get("error") or "")
          and "FILE_SET_CONSISTENCY_NOT_VERIFIED" in
          ((r1_items[0]["outcome"] or {}).get("error") or "")
          and r1_counts["data_versions"] == EXPECTED_WORKSET
          and r1_counts["market_update_results"] == EXPECTED_WORKSET,
          {"items": r1_items, "counts": r1_counts})

    # ---- S8 gates verbatim ----
    blockers = accepts[0].get("blocking_reasons") if accepts else None
    all_same = all(a.get("blocking_reasons") == blockers for a in accepts)
    caps = s1.tool("get_data_capabilities", {"dataset": "market.daily"})
    record("S8_gates", {"blocking_reasons": blockers,
                        "all_accepts_identical": all_same,
                        "capabilities_production_ready":
                            caps.get("production_ready")})
    check("U11_gates_verbatim",
          blockers is not None and len(blockers) == 3
          and all_same
          and any("GBBQ_QFQ_NOT_VERIFIED" in b for b in blockers)
          and any("FILE_ACCESS_USE_NOT_VERIFIED" in b for b in blockers)
          and any("FILE_SET_CONSISTENCY_NOT_VERIFIED" in b for b in blockers)
          and caps.get("production_ready") is False,
          {"blocking_reasons": blockers,
           "production_ready": caps.get("production_ready")})
    s1.close()

    results["summary"] = {
        "passed": sum(1 for a in results["assertions"] if a["ok"]),
        "total": len(results["assertions"]),
        "failed": [a["name"] for a in results["assertions"] if not a["ok"]],
        "update_tasks": [a["task_id"] for a in accepts],
        "counts_after": after["counts"],
        "non_idle_worker_runs": non_idle,
    }
    with open(os.path.join(HERE, "results.json"), "w") as f:
        json.dump(results, f, ensure_ascii=False, indent=1)
    print(json.dumps(results["summary"], ensure_ascii=False, indent=1))
    sys.exit(0 if not results["summary"]["failed"] else 1)


if __name__ == "__main__":
    main()
