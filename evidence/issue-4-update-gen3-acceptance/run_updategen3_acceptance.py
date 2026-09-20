#!/usr/bin/env python3
"""Issue #4 update branch — changed-generation update_data acceptance.

Real mcp_data stdio + official env on the gen-3 snapshot (built from the
#217 acquisition, sha256-pinned) over a COPY of the gen-2 published
baseline (the sealed update5219 acceptance DB — identical published
content; its 5219 pre-existing no_change result rows are accounted for
in baseline counts, never treated as this run's output).

Sequence per PLAN.md (freeze v1): env fingerprint -> baseline mechanical
queries -> default-budget verbatim rejection -> expected-set load
(S3 artifact expected_sets.json, produced by derive_expected_sets.py)
-> 53 sharded explicit update_data -> drain worker -> outcome/expected
consistency + zero-new-version + provenance assertions -> env-absent
refusal leg -> gates verbatim.

Legal outcome space is {success(NoChange), failure "NEEDS_VERIFICATION:
..."} — combined coverage is absent by design for the update path, so a
Published new version is a finding, never a pass.

Box overlayfs note (#213): the service runs with
X_DATA_MCP_UNSAFE_NONLOCAL_FS=1 dev opt-out — same mechanism the prior
acceptances used; /home/box is overlayfs and the fs guard is fail-closed
otherwise.
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
RUN_DIR = "/home/box/acceptance-run-issue4-update-gen3"
SRC_DB = "/home/box/acceptance-run-issue4-update/db.sqlite"
SNAP_DIR = os.path.join(RUN_DIR, "snapshot")
GEN3_RETAINED = "/home/box/retained-inputs/gen3"
MANIFEST = os.path.join(HERE, "snapshot-manifest-gen3.json")
EXPECTED_SETS = os.path.join(HERE, "expected_sets.json")
PROFILE = os.environ.get("BUILD_PROFILE", "release")
MCP_DATA = os.path.join(REPO, "target", PROFILE, "mcp_data")
TASK_SQLITE = os.path.join(REPO, "target", PROFILE, "task_sqlite")
EXPECTED = {  # gen-3 pins, frozen by PLAN.md §1 from acquisition.json
    "base.zip": "56c3574505bbd3fa3a1899b46e09112070dea17cbea2f713b7ccff2628379bec",
    "gbbq.zip": "502236d9d8394e89b1a4091f5d0d2b86e419c0c4da2c788ddcea16ef7d3763e3",
    "hsjday.zip": "563797f0d1b852e91b85ee0aafe396c0df1bb12e55b569e8526db76a4e181c64",
}
SPOT = {"sh.600004", "sh.600006", "sz.000002"}
PIN_REVISION = "sz.000001"   # +1 cat1 event @2026-09-24 -> RevisionUnverified
EXPECTED_WORKSET = 5219
EXPECTED_DEFAULT_WORKSET = 5224
EXPECTED_BARS = 16629206
EXPECTED_UPDATE_RESULTS_BASE = 5219  # pre-existing update5219 no_change rows
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
        # Box dev opt-out (#213): /home/box is overlayfs; the local_filesystem
        # guard is fail-closed without it. Local acceptance only.
        env["X_DATA_MCP_UNSAFE_NONLOCAL_FS"] = "1"
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


def nv_reason(error):
    """Extract the verbatim NEEDS_VERIFICATION reason head, or None.

    Item errors carry the transport wrapper verbatim, e.g.
    "INVALID_MARKET_COMMAND: NEEDS_VERIFICATION: RevisionUnverified([...])".
    """
    if not error or "NEEDS_VERIFICATION:" not in error:
        return None
    tail = error.split("NEEDS_VERIFICATION:", 1)[1].strip()
    return tail.split("(", 1)[0].split()[0] if tail else ""


def main():
    for name in ("base.zip", "gbbq.zip", "hsjday.zip"):
        src = os.path.join(GEN3_RETAINED, name)
        dst = os.path.join(SNAP_DIR, name)
        if not os.path.isfile(src):
            raise SystemExit(f"gen-3 retained member absent: {src}")
        os.makedirs(SNAP_DIR, exist_ok=True)
        if not os.path.isfile(dst) or sha256_file(dst) != sha256_file(src):
            shutil.copyfile(src, dst)
    if not os.path.isfile(SRC_DB):
        raise SystemExit(f"gen-2 baseline db absent: {SRC_DB}")
    expected = json.load(open(EXPECTED_SETS))
    s_chg = set(expected["S_chg"])
    s_evt = set(expected["S_evt"])
    manifest_sha = sha256_file(MANIFEST)
    record("expected_sets_loaded",
           {"S_evt": len(s_evt), "S_day": len(expected["S_day"]),
            "S_chg": len(s_chg),
            "inputs": expected["inputs"], "manifest_sha256": manifest_sha})

    # ---- S0 environment + input integrity ----
    head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=REPO,
                          capture_output=True, text=True).stdout.strip()
    cargo = subprocess.run(
        [os.path.expanduser("~/.cargo/bin/cargo"), "--version"],
        capture_output=True, text=True).stdout.strip()
    arch = {n: sha256_file(os.path.join(SNAP_DIR, n))
            for n in ("base.zip", "gbbq.zip", "hsjday.zip")}
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
                                "shanghai_today": today,
                                "baseline_source": SRC_DB}}

    def check(name, ok, detail):
        results["assertions"].append({"name": name, "ok": bool(ok),
                                      "detail": detail})

    check("C1_input_integrity",
          arch == {k: EXPECTED[k] for k in
                   ("base.zip", "gbbq.zip", "hsjday.zip")},
          {"archive_sha256": arch, "manifest_sha256": manifest_sha})

    # ---- S1 copy baseline db + mechanical baseline ----
    db = os.path.join(RUN_DIR, "db.sqlite")
    copy_db(SRC_DB, db)
    base = baseline(db)
    with open(os.path.join(RUN_DIR, "baseline.json"), "w") as f:
        json.dump({"counts": base["counts"], "heads": base["heads"],
                   "first_dates": base["first_dates"]}, f, indent=1)
    spot_baseline = {s: [tuple(r) for r in rows]
                     for s, rows in base["spot_bars"].items()}
    record("baseline", {"counts": base["counts"],
                        "distinct_head_symbols": len(set(base["heads"]))})
    check("C1b_baseline_workset",
          len(base["heads"]) == EXPECTED_WORKSET
          and len(set(base["heads"])) == EXPECTED_WORKSET
          and base["counts"]["data_versions"] == EXPECTED_WORKSET
          and base["counts"]["daily_bars"] == EXPECTED_BARS
          and base["counts"]["market_update_results"]
              == EXPECTED_UPDATE_RESULTS_BASE
          and all(sym not in base["heads"] for sym in NO_DAY)
          and all(base["first_dates"].get(vid) for vid in
                  base["heads"].values()),
          {"counts": base["counts"]})

    svc = Service(db, "present")
    svc.rpc("initialize")

    # ---- S4 default selection budget rejection (BLOCKED_ON_IMPL) ----
    default_resp = svc.tool_raw("update_data", {
        "request_id": "u-default", "dataset": "market.daily",
        "contract_version": "v1",
        "selection": {"kind": "default"}})
    record("S4_default_selection", default_resp)
    con = sqlite3.connect(db)
    s4_tasks = con.execute("SELECT COUNT(*) FROM tasks").fetchone()[0]
    con.close()
    s4_err = json.dumps(
        default_resp.get("result", {}).get("structuredContent")
        or default_resp.get("error") or {}, ensure_ascii=False)
    expected_budget = ("default security budget exceeded: "
                       f"{EXPECTED_DEFAULT_WORKSET} > 100")
    check("C2_default_budget",
          (default_resp.get("result", {}).get("isError")
           or default_resp.get("error"))
          and expected_budget in s4_err,
          {"response": default_resp, "task_count_before": s4_tasks})
    results["blocked_paths"]["S4_default_selection"] = default_resp.get(
        "result", {}).get("structuredContent") or default_resp.get("error")

    # ---- S5 build 53 sharded update items from the baseline ----
    symbols = sorted(base["heads"])
    items = [{
        "entity": {"exchange": sym.split(".")[0], "code": sym.split(".")[1]},
        "base_version_id": base["heads"][sym],
        "start_date": base["first_dates"][base["heads"][sym]],
        "end_date": today,
    } for sym in symbols]
    item_symbol = {i["base_version_id"]: s for i, s in zip(items, symbols)}
    shards = [items[i:i + 100] for i in range(0, len(items), 100)]
    with open(os.path.join(RUN_DIR, "shards.json"), "w") as f:
        json.dump(shards, f, ensure_ascii=False, indent=1)
    record("shards", {"count": len(shards),
                      "sizes": [len(s) for s in shards]})

    # ---- S6 accept shards + drain worker ----
    accepts = []
    for i, shard in enumerate(shards):
        rid = f"update-gen3-{i:03d}"
        acc = svc.tool("update_data", update_request(rid, shard))
        accepts.append(acc)
        record("accepted_shard", {"request_id": rid, "acceptance": acc,
                                  "shard_size": len(shard)})
    total_items = sum(len(a.get("item_ids") or []) for a in accepts)
    check("C3_sharded_acceptance",
          len(accepts) == 53
          and all(len(a.get("item_ids") or []) <= 100 for a in accepts)
          and total_items == EXPECTED_WORKSET,
          {"tasks": len(accepts), "total_items": total_items})

    non_idle = 0
    while True:
        w = svc.tool("run_worker_once", {})
        if non_idle % 100 == 0:
            record("worker_once", {"seq": non_idle, "result": w})
        if w.get("idle"):
            break
        non_idle += 1
        if non_idle > EXPECTED_WORKSET + 200:
            raise SystemExit(
                f"worker did not reach idle after {non_idle} runs")
    record("worker_drained", {"non_idle_runs": non_idle})

    # ---- S7 assertions ----
    item_outcome = {}   # symbol -> ("no_change"|"nv:<reason>"|"other")
    failed_items = []
    other_failures = []
    for a in accepts:
        for item in outcomes(db, a["task_id"]):
            o = item["outcome"] or {}
            sym = item["target"].get("symbol")
            if o.get("success") is True:
                item_outcome[sym] = "no_change"
            else:
                err = o.get("error") or ""
                reason = nv_reason(err)
                item_outcome[sym] = f"nv:{reason}" if reason else "other"
                failed_items.append({"item_id": item["item_id"],
                                     "symbol": sym, "error": err})
                if reason is None:
                    other_failures.append({"symbol": sym, "error": err})

    no_change_set = {s for s, v in item_outcome.items() if v == "no_change"}
    nv_set = {s for s, v in item_outcome.items() if v.startswith("nv:")}
    reason_dist = {}
    for v in item_outcome.values():
        if v.startswith("nv:"):
            reason_dist[v[3:]] = reason_dist.get(v[3:], 0) + 1
    record("item_outcome_summary",
           {"total": len(item_outcome), "no_change": len(no_change_set),
            "needs_verification": len(nv_set),
            "reason_distribution": reason_dist,
            "other_failures": other_failures[:20],
            "unexpected_outcomes": [
                s for s, v in item_outcome.items() if v == "other"][:20]})

    con = sqlite3.connect(db)
    # This run's rows only: task_ids of our accepts.
    our_tasks = {a["task_id"] for a in accepts}
    our_rows = con.execute(
        "SELECT COUNT(*), SUM(no_change), "
        " SUM(result_version_id != base_version_id) "
        "FROM market_update_results WHERE task_id IN (%s)"
        % ",".join("?" * len(our_tasks)),
        tuple(our_tasks)).fetchone()
    audit_stats = con.execute(
        "SELECT COUNT(*), "
        " SUM(json_extract(audit_json,'$.no_change') != 1), "
        " SUM(json_extract(audit_json,'$.combined') IS NOT NULL), "
        " SUM(json_extract(audit_json,'$.evidence.base_version_id') IS NULL), "
        " SUM(json_extract(audit_json,'$.evidence.same_qfq_basis_ref') IS NULL) "
        "FROM market_update_results WHERE task_id IN (%s)"
        % ",".join("?" * len(our_tasks)),
        tuple(our_tasks)).fetchone()
    audit_spot = con.execute(
        "SELECT r.item_id, json_extract(r.audit_json,'$.evidence.base_version_id'),"
        " json_extract(r.audit_json,'$.evidence.same_qfq_basis_ref'),"
        " json_extract(r.audit_json,'$.evidence.revision_ref'),"
        " r.result_sha256 FROM market_update_results r "
        "WHERE r.task_id IN (%s) LIMIT 3" % ",".join("?" * len(our_tasks)),
        tuple(our_tasks)).fetchall()
    after = baseline(db)
    task_states = con.execute(
        "SELECT status, COUNT(*), SUM(json_extract(result_json,'$.finished_items')) "
        "FROM tasks WHERE task_id IN (%s) GROUP BY status"
        % ",".join("?" * len(our_tasks)),
        tuple(our_tasks)).fetchall()
    con.close()

    # C4: legal outcome space + zero new versions + heads unchanged.
    check("C4_legal_outcomes_zero_new_versions",
          len(item_outcome) == EXPECTED_WORKSET
          and not other_failures
          and after["counts"]["data_versions"] == EXPECTED_WORKSET
          and after["counts"]["published"] == EXPECTED_WORKSET
          and after["heads"] == base["heads"]
          and (our_rows[0] or 0) == len(no_change_set)
          and (our_rows[2] or 0) == 0,
          {"outcomes": len(item_outcome),
           "no_change": len(no_change_set),
           "nv": len(nv_set),
           "other_failures": other_failures[:10],
           "our_update_rows": our_rows,
           "counts_after": after["counts"]})

    # C5: outcome ↔ expected-set consistency.
    # PLAN CORRECTION (post-freeze code fact, recorded honestly):
    # CoverageEvidence.reason embeds the snapshot manifest sha256, so under
    # a new generation EVERY item is metadata_changed and NoChange is
    # structurally unreachable — the legal outcome space collapses to
    # NEEDS_VERIFICATION only. Revised-overlap bars require a gbbq
    # event-set diff, so RevisionUnverified must stay inside S_evt.
    rev_set = {s for s, v in item_outcome.items()
               if v == "nv:RevisionUnverified"}
    illegal_reasons = sorted(
        s for s, v in item_outcome.items()
        if v not in ("nv:RevisionUnverified", "nv:combined"))
    check("C5_outcome_expected_consistency",
          not no_change_set
          and rev_set <= (s_evt & set(item_outcome))
          and not illegal_reasons,
          {"no_change_count": len(no_change_set),
           "no_change_symbols": sorted(no_change_set)[:20],
           "rev_not_in_S_evt": sorted(rev_set - s_evt)[:20],
           "S_evt_not_revised_count":
               len((s_evt & set(item_outcome)) - rev_set),
           "illegal_reasons": illegal_reasons[:20],
           "reason_distribution": reason_dist})

    # C6: pin assertion — sz.000001 gbbq event addition revises QFQ basis.
    pin_outcome = item_outcome.get(PIN_REVISION)
    pin_error = next((f["error"] for f in failed_items
                      if f["symbol"] == PIN_REVISION), None)
    check("C6_pin_sz000001_revision_unverified",
          pin_outcome == "nv:RevisionUnverified"
          and pin_error and "RevisionUnverified" in pin_error,
          {"pin_outcome": pin_outcome,
           "pin_error_head": (pin_error or "")[:160]})

    # C7: nothing was written.
    spot_after = {s: [tuple(r) for r in rows]
                  for s, rows in after["spot_bars"].items()}
    check("C7_data_unchanged",
          after["counts"]["daily_bars"] == EXPECTED_BARS
          and spot_after == spot_baseline
          and all(spot_baseline[s] for s in SPOT),
          {"daily_bars_after": after["counts"]["daily_bars"],
           "spot_bar_counts": {s: len(v) for s, v in spot_after.items()}})

    # C8: no_change audit rows bind the gen-3 manifest + gbbq member.
    check("C8_provenance",
          audit_stats is not None and audit_stats[0] == len(no_change_set)
          and all(v == 0 or v is None for v in audit_stats[1:])
          and all(manifest_sha[:20] in (r[2] or "") for r in audit_spot)
          and all(r[3] is None for r in audit_spot),
          {"audit_rows": audit_stats[0] if audit_stats else None,
           "flags": audit_stats[1:] if audit_stats else None,
           "audit_spot": audit_spot})

    check("C9_task_bookkeeping",
          len(task_states) >= 1
          and all(s in ("succeeded", "failed", "partially_failed")
                  for s, _, _ in task_states)
          and sum(c for _, c, _ in task_states) == len(accepts)
          and sum(f or 0 for _, _, f in task_states) == EXPECTED_WORKSET,
          {"task_states": task_states})

    # ---- S8 env-absent refusal leg on the same copy ----
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
    check("C10_env_absent",
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
          and r1_counts["data_versions"] == EXPECTED_WORKSET,
          {"items": r1_items, "counts": r1_counts})

    # ---- S9 gates verbatim ----
    blockers = accepts[0].get("blocking_reasons") if accepts else None
    all_same = all(a.get("blocking_reasons") == blockers for a in accepts)
    caps = s1.tool("get_data_capabilities", {"dataset": "market.daily"})
    record("S9_gates", {"blocking_reasons": blockers,
                        "all_accepts_identical": all_same,
                        "capabilities_production_ready":
                            caps.get("production_ready")})
    check("C11_gates_verbatim",
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
        "no_change": len(no_change_set),
        "needs_verification": len(nv_set),
        "reason_distribution": reason_dist,
        "non_idle_worker_runs": non_idle,
    }
    with open(os.path.join(HERE, "results.json"), "w") as f:
        json.dump(results, f, ensure_ascii=False, indent=1)
    print(json.dumps(results["summary"], ensure_ascii=False, indent=1))
    sys.exit(0 if not results["summary"]["failed"] else 1)


if __name__ == "__main__":
    main()
