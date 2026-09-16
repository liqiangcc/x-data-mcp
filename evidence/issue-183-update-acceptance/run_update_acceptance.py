#!/usr/bin/env python3
"""Issue #183 Phase C update acceptance run per the frozen plan
(evidence/issue-183-update-acceptance/PLAN.md).

Real mcp_data stdio service against the retained gen-2 snapshot:
publish base daily (sz.000002 + sh.600006) -> update_data explicit-range
full refresh -> successful no-change -> request_id replay / fresh-request
idempotency -> read-back invariance -> four independent-scratch refusals
(env-absent / uninspectable manifest / base missing / base identity
mismatch). No-change expectation: receipt version_id == base version,
no second market.daily version, execution trail in market_update_results.
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
RUN_DIR = "/home/box/acceptance-run-issue183"
SNAP_DIR = "/home/box/acceptance-run-issue3/snapshot"
MANIFEST = os.path.join(REPO, "evidence/issue-3-acceptance-run/snapshot-manifest.json")
BAD_MANIFEST = os.path.join(RUN_DIR, "bad-manifest.json")
MCP_DATA = os.path.join(REPO, "target/debug/mcp_data")
TASK_SQLITE = os.path.join(REPO, "target/debug/task_sqlite")
RETAINED = "/home/box/retained-inputs"
EXPECTED = {  # frozen PLAN §1/§2
    "base.zip": "dcdf6b49898926675698263535021aed7ad92b34be82a1a9e8a2a8982bd68bc6",
    "gbbq.zip": "daad6290c4f1cd478d7436b1e7077260a3e3e52cefd7eae58e4f24878fa2d0e9",
    "hsjday.zip": "c82f852a7bc2e52c7e89347a1e0d1878b01ce997946c2745f7af323752270892",
    "manifest": "b8a0d7b9aec1a270549d1e53ba26c0ab04842a23c05d1d1f7e75dd44a9f444d3",
}
BASE_SECURITIES = ["sz.000002", "sh.600006"]
EXPECTED_DAILY = {  # frozen PLAN §4.S2
    "sz.000002": (8479, "1991-01-29", "2026-09-14"),
    "sh.600006": (6467, "1999-07-27", "2026-09-14"),
}
UPDATE_SYMBOL = "sz.000002"
# Frozen PLAN §3 oracle spot bars for read-back invariance.
ORACLE_20000110 = {"open": "-8.14", "high": "-8.06", "low": "-8.15",
                   "close": "-8.06"}
ORACLE_DATES = ["2000-01-10", "2026-09-14"]

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
    tz = datetime.timezone(datetime.timedelta(hours=8))
    return datetime.datetime.now(tz).date().isoformat()


class Service:
    """One mcp_data stdio process; env_mode: present|absent|bad-manifest."""

    def __init__(self, db, env_mode="present"):
        env = dict(os.environ)
        env.pop("X_DATA_MCP_OFFICIAL_MANIFEST", None)
        env.pop("X_DATA_MCP_OFFICIAL_DIR", None)
        if env_mode == "present":
            env["X_DATA_MCP_OFFICIAL_MANIFEST"] = MANIFEST
            env["X_DATA_MCP_OFFICIAL_DIR"] = SNAP_DIR
        elif env_mode == "bad-manifest":
            env["X_DATA_MCP_OFFICIAL_MANIFEST"] = BAD_MANIFEST
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


def entity(symbol):
    exchange, code = symbol.split(".")
    return {"exchange": exchange, "code": code}


def update_request(request_id, symbol, base_id, start, end):
    return {"request_id": request_id, "dataset": "market.daily",
            "contract_version": "v1",
            "items": [{"entity": entity(symbol), "base_version_id": base_id,
                       "start_date": start, "end_date": end}]}


def fresh_db(path):
    for stale in (path, path + "-wal", path + "-shm"):
        if os.path.exists(stale):
            os.remove(stale)
    init = subprocess.run([TASK_SQLITE, "init", path],
                          capture_output=True, text=True)
    record("task_sqlite_init", {"db": path, "stdout": init.stdout,
                                "stderr": init.stderr,
                                "returncode": init.returncode})
    if init.returncode != 0:
        raise SystemExit("database init failed")


def publish_base(svc, db, securities, request_id):
    """start_collection explicit selection -> worker -> day head ids."""
    accepted = svc.tool("start_collection", {
        "request_id": request_id, "dataset": "market.daily",
        "contract_version": "v1",
        "selection": {"kind": "explicit",
                      "securities": [entity(s) for s in securities]}})
    record("accepted_daily", {"db": db, "accepted": accepted})
    svc.tool("run_worker_once", {})
    con = sqlite3.connect(db)
    heads = {}
    for s in securities:
        heads[s] = con.execute(
            "SELECT v.version_id FROM data_versions v"
            " JOIN effective_heads e ON v.version_id=e.version_id"
            " WHERE v.symbol=? AND v.period='day'", (s,)).fetchone()[0]
    con.close()
    return heads


def bar_hash(con, version_id):
    """sha256 over canonical daily_bars rows of one version."""
    rows = con.execute(
        "SELECT trade_date,open,high,low,close,volume,amount FROM daily_bars"
        " WHERE version_id=? ORDER BY trade_date", (version_id,)).fetchall()
    return hashlib.sha256(json.dumps(rows).encode()).hexdigest()


def day_version_rows(con, symbol):
    return con.execute(
        "SELECT version_id,record_count,first_date,last_date FROM data_versions"
        " WHERE symbol=? AND period='day' AND state='published'",
        (symbol,)).fetchall()


def query_bar(svc, symbol, date):
    page = svc.tool("query_data", {
        "dataset": "market.daily", "contract_version": "v1",
        "selection": {"kind": "single", "entity": entity(symbol)},
        "filter": {"kind": "daily", "start_date": date, "end_date": date},
        "version": {"kind": "latest_per_entity"},
        "page_size": 10, "cursor": None})
    record("daily_query", {"symbol": symbol, "date": date, "page": page})
    return page


def refusal_text(resp):
    if resp.get("error"):
        return json.dumps(resp["error"], ensure_ascii=False)
    result = resp.get("result") or {}
    sc = result.get("structuredContent") or {}
    content = result.get("content") or []
    text = sc.get("message") or sc.get("error") or ""
    if not text and content:
        text = content[0].get("text", "")
    return text if text else json.dumps(resp, ensure_ascii=False)


def main():
    for name in ("official-base.zip", "official-gbbq.zip",
                 "official-hsjday.zip"):
        if not os.path.isfile(os.path.join(RETAINED, name)):
            raise SystemExit(f"retained input absent: {RETAINED}/{name}")
    for name in ("base.zip", "gbbq.zip", "hsjday.zip"):
        if not os.path.isfile(os.path.join(SNAP_DIR, name)):
            raise SystemExit(f"snapshot member absent: {SNAP_DIR}/{name}")
    os.makedirs(RUN_DIR, exist_ok=True)
    with open(BAD_MANIFEST, "w") as f:
        f.write('{"files":[]}')

    # ---- S0 environment/identity records ----
    head = subprocess.run(["git", "rev-parse", "HEAD"], cwd=REPO,
                          capture_output=True, text=True).stdout.strip()
    cargo = subprocess.run(
        [os.path.expanduser("~/.cargo/bin/cargo"), "--version"],
        capture_output=True, text=True).stdout.strip()
    hashes = {n: sha256_file(os.path.join(SNAP_DIR, n))
              for n in ("base.zip", "gbbq.zip", "hsjday.zip")}
    manifest_sha = sha256_file(MANIFEST)
    record("env", {"run_dir": RUN_DIR, "head": head, "cargo": cargo,
                   "mcp_data_sha256": sha256_file(MCP_DATA),
                   "task_sqlite_sha256": sha256_file(TASK_SQLITE),
                   "manifest": MANIFEST, "manifest_sha256": manifest_sha,
                   "snapshot_dir": SNAP_DIR, "zip_sha256": hashes})

    results = {"assertions": [], "versions": {}, "blocked_paths": {},
               "run_identity": {"head": head, "cargo": cargo,
                                "manifest_sha256": manifest_sha,
                                "zip_sha256": hashes}}

    def check(name, ok, detail):
        results["assertions"].append({"name": name, "ok": bool(ok),
                                      "detail": detail})

    # A1 input integrity (frozen pins).
    check("A1_input_integrity",
          hashes["base.zip"] == EXPECTED["base.zip"]
          and hashes["gbbq.zip"] == EXPECTED["gbbq.zip"]
          and hashes["hsjday.zip"] == EXPECTED["hsjday.zip"]
          and manifest_sha == EXPECTED["manifest"],
          {"zip_sha256": hashes, "manifest_sha256": manifest_sha})

    db_main = os.path.join(RUN_DIR, "db-main.sqlite")
    fresh_db(db_main)

    # ---- S1/S2 base publish on db-main ----
    svc = Service(db_main, "present")
    svc.rpc("initialize")
    heads = publish_base(svc, db_main, BASE_SECURITIES,
                         "update-acceptance-daily")
    con = sqlite3.connect(db_main)

    # A2 base publication facts.
    base_facts = {}
    for s in BASE_SECURITIES:
        cnt, first, last, vid = con.execute(
            "SELECT record_count,first_date,last_date,version_id"
            " FROM data_versions WHERE symbol=? AND period='day'"
            " AND state='published'", (s,)).fetchone()
        want = EXPECTED_DAILY[s]
        base_facts[s] = {"version_id": vid, "record_count": cnt,
                         "first_date": first, "last_date": last}
        check(f"A2_base_{s}", (cnt, first, last) == want, base_facts[s])
    results["versions"]["daily_base"] = base_facts

    # provenance snapshot_inputs vs measured retained sha.
    compat = json.loads(con.execute(
        "SELECT compat_json FROM data_versions WHERE version_id=?",
        (heads[UPDATE_SYMBOL],)).fetchone()[0])
    snapshot_inputs = compat["batch"]["provenance"]["snapshot_inputs"]
    record("snapshot_inputs", snapshot_inputs)
    prov_shas = {i.get("archive_sha256") for i in snapshot_inputs}
    prov_shas.discard(None)
    gbbq_member = next(
        (i.get("member_sha256") for i in snapshot_inputs
         if i.get("role") == "gbbq"), None)
    check("A10_provenance_traceable",
          {hashes["hsjday.zip"], hashes["gbbq.zip"]} <= prov_shas
          and gbbq_member
          == "4a3e9b862247a8a3fec406cc25b9a5a6ee3303f740780822519c99decaa09d77",
          {"snapshot_inputs": snapshot_inputs, "measured": hashes})

    # ---- S3 base snapshot ----
    base_snapshot = {}
    for s in BASE_SECURITIES:
        base_snapshot[s] = {
            "head": con.execute(
                "SELECT version_id FROM effective_heads"
                " WHERE symbol=? AND period='day'", (s,)).fetchone()[0],
            "bar_hash": bar_hash(con, heads[s]),
            "versions": day_version_rows(con, s),
        }
    record("base_snapshot", base_snapshot)
    for d in ORACLE_DATES:
        query_bar(svc, UPDATE_SYMBOL, d)

    run_date = shanghai_today()
    record("acceptance_date", run_date)

    # ---- S4 update_data u1 ----
    acc_u1 = svc.tool("update_data", update_request(
        "u1", UPDATE_SYMBOL, heads[UPDATE_SYMBOL],
        EXPECTED_DAILY[UPDATE_SYMBOL][1], run_date))
    record("accepted_update_u1", acc_u1)
    frozen = con.execute(
        "SELECT frozen_inputs_json FROM task_items WHERE task_id=?",
        (acc_u1["task_id"],)).fetchone()[0]
    frozen = json.loads(frozen)
    base_digest = json.loads(con.execute(
        "SELECT compat_json FROM data_versions WHERE version_id=?",
        (heads[UPDATE_SYMBOL],)).fetchone()[0])["integrity_sha256"]
    check("A3_update_acceptance",
          frozen.get("base_version_id") == heads[UPDATE_SYMBOL]
          and frozen.get("base_integrity_sha256") == base_digest
          and frozen.get("range", {}).get("start")
          == EXPECTED_DAILY[UPDATE_SYMBOL][1]
          and frozen.get("range", {}).get("end") == run_date,
          {"frozen_inputs": frozen, "task_id": acc_u1["task_id"]})

    # ---- S5 execution ----
    svc.tool("run_worker_once", {})
    outcome_u1 = json.loads(con.execute(
        "SELECT outcome_json FROM task_items WHERE task_id=?",
        (acc_u1["task_id"],)).fetchone()[0])
    record("update_outcome_u1", outcome_u1)
    impl_landed = outcome_u1.get("success") is True
    check("A4_update_execution_success", impl_landed,
          {"outcome": outcome_u1,
           "note": "BLOCKED_ON_IMPL" if not impl_landed else "impl landed"})

    upd_res = con.execute(
        "SELECT result_id,base_version_id,result_version_id,no_change,"
        "audit_json,result_sha256 FROM market_update_results WHERE task_id=?",
        (acc_u1["task_id"],)).fetchall()
    record("update_results_u1", upd_res)

    n_versions_after = len(day_version_rows(con, UPDATE_SYMBOL))
    if impl_landed:
        check("A5_nochange_reuses_base",
              upd_res and upd_res[0][2] == heads[UPDATE_SYMBOL]
              and n_versions_after == 1,
              {"result_version_id": upd_res[0][2] if upd_res else None,
               "base": heads[UPDATE_SYMBOL],
               "day_versions": n_versions_after})
        check("A6_execution_trail",
              upd_res and upd_res[0][1] == heads[UPDATE_SYMBOL]
              and upd_res[0][3] in (1, "1", "true")
              and upd_res[0][4] and upd_res[0][5],
              {"row": [r[:4] for r in upd_res]})
    else:
        check("A5_nochange_reuses_base", False, "BLOCKED_ON_IMPL")
        check("A6_execution_trail", False, "BLOCKED_ON_IMPL")

    # ---- S6 same request_id replay ----
    replay = svc.tool("update_data", update_request(
        "u1", UPDATE_SYMBOL, heads[UPDATE_SYMBOL],
        EXPECTED_DAILY[UPDATE_SYMBOL][1], run_date))
    record("replayed_u1", replay)
    check("A7_same_request_id_reused",
          replay.get("reused") is True
          and replay.get("task_id") == acc_u1["task_id"], replay)

    # ---- S7 fresh request_id, same frozen inputs ----
    acc_u2 = svc.tool("update_data", update_request(
        "u2", UPDATE_SYMBOL, heads[UPDATE_SYMBOL],
        EXPECTED_DAILY[UPDATE_SYMBOL][1], run_date))
    record("accepted_update_u2", acc_u2)
    svc.tool("run_worker_once", {})
    outcome_u2 = json.loads(con.execute(
        "SELECT outcome_json FROM task_items WHERE task_id=?",
        (acc_u2["task_id"],)).fetchone()[0])
    upd_res_u2 = con.execute(
        "SELECT result_version_id,no_change FROM market_update_results"
        " WHERE task_id=?", (acc_u2["task_id"],)).fetchall()
    record("update_outcome_u2", {"outcome": outcome_u2,
                                 "results": upd_res_u2})
    n_versions_final = len(day_version_rows(con, UPDATE_SYMBOL))
    if impl_landed:
        check("A8_fresh_request_nochange",
              outcome_u2.get("success") is True
              and upd_res_u2
              and upd_res_u2[0][0] == heads[UPDATE_SYMBOL]
              and upd_res_u2[0][1] in (1, "1", "true")
              and n_versions_final == 1,
              {"results": upd_res_u2, "day_versions": n_versions_final})
    else:
        check("A8_fresh_request_nochange", False, "BLOCKED_ON_IMPL")

    # ---- S8 read-back invariance ----
    pages = {d: query_bar(svc, UPDATE_SYMBOL, d) for d in ORACLE_DATES}
    versions_page = svc.tool("list_data_versions", {
        "dataset": "market.daily", "contract_version": "v1",
        "selection": {"kind": "single", "entity": entity(UPDATE_SYMBOL)}})
    record("versions_page", versions_page)
    head_now = con.execute(
        "SELECT version_id FROM effective_heads WHERE symbol=? AND period='day'",
        (UPDATE_SYMBOL,)).fetchone()[0]
    hash_now = bar_hash(con, heads[UPDATE_SYMBOL])

    def bar_from_page(page):
        recs = page.get("records") or page.get("items") or []
        if not recs:
            return None
        return recs[0]

    if impl_landed:
        b10 = bar_from_page(pages["2000-01-10"]) or {}
        oracle_ok = all(
            str(b10.get(k)) == v for k, v in ORACLE_20000110.items())
        b_last = bar_from_page(pages["2026-09-14"])
        check("A9_readback_unchanged",
              oracle_ok and b_last is not None
              and head_now == heads[UPDATE_SYMBOL]
              and hash_now == base_snapshot[UPDATE_SYMBOL]["bar_hash"],
              {"oracle_20000110": b10, "last_bar": b_last,
               "head": head_now, "bar_hash_equal":
               hash_now == base_snapshot[UPDATE_SYMBOL]["bar_hash"]})
    else:
        check("A9_readback_unchanged", False, "BLOCKED_ON_IMPL")

    svc.close()
    con.close()

    # ---- S6 refusals: R1 env-absent, R2 uninspectable manifest ----
    for tag, env_mode in (("R1_env_absent", "absent"),
                          ("R2_uninspectable_manifest", "bad-manifest")):
        db = os.path.join(RUN_DIR, f"db-{tag.split('_')[0].lower()}.sqlite")
        fresh_db(db)
        s0 = Service(db, "present")
        s0.rpc("initialize")
        rheads = publish_base(s0, db, [UPDATE_SYMBOL], f"{tag}-daily")
        s0.close()
        s1 = Service(db, env_mode)
        s1.rpc("initialize")
        acc = s1.tool("update_data", update_request(
            f"{tag}-update", UPDATE_SYMBOL, rheads[UPDATE_SYMBOL],
            EXPECTED_DAILY[UPDATE_SYMBOL][1], run_date))
        record(f"{tag}_accepted", acc)
        s1.tool("run_worker_once", {})
        rcon = sqlite3.connect(db)
        outcome = rcon.execute(
            "SELECT outcome_json FROM task_items WHERE task_id=?",
            (acc["task_id"],)).fetchone()[0]
        n_day = len(day_version_rows(rcon, UPDATE_SYMBOL))
        rcon.close()
        record(f"{tag}_refusal", {"outcome": outcome, "day_versions": n_day})
        results["blocked_paths"][tag] = outcome
        s1.close()
        text = str(outcome)
        refused = ("SOURCE_NOT_VERIFIED" in text
                   or "NOT_PRODUCTION_READY" in text) and n_day == 1
        check("A11_env_absent_refusal" if tag.startswith("R1")
              else "A12_uninspectable_refusal",
              refused and json.loads(outcome).get("success") is False,
              {"outcome": outcome[:500], "day_versions": n_day})

    # ---- R3 base refusals on db-r3 (accept-layer, verbatim) ----
    db3 = os.path.join(RUN_DIR, "db-r3.sqlite")
    fresh_db(db3)
    s3 = Service(db3, "present")
    s3.rpc("initialize")
    h3 = publish_base(s3, db3, ["sh.600006"], "r3-daily")
    bad_base = s3.tool_raw("update_data", update_request(
        "r3a-missing-base", UPDATE_SYMBOL,
        "collection-version-" + "0" * 64,
        EXPECTED_DAILY[UPDATE_SYMBOL][1], run_date))
    record("R3a_refusal", bad_base)
    results["blocked_paths"]["R3a_base_missing"] = refusal_text(bad_base)
    cross = s3.tool_raw("update_data", update_request(
        "r3b-cross-base", UPDATE_SYMBOL, h3["sh.600006"],
        EXPECTED_DAILY[UPDATE_SYMBOL][1], run_date))
    record("R3b_refusal", cross)
    results["blocked_paths"]["R3b_base_identity"] = refusal_text(cross)
    r3con = sqlite3.connect(db3)
    n_tasks = r3con.execute(
        "SELECT COUNT(*) FROM tasks WHERE json_extract(command_key_json,"
        "'$.operation')='update_data'").fetchone()[0]
    n_sz_day = len(day_version_rows(r3con, UPDATE_SYMBOL))
    r3con.close()
    check("A13_base_missing_refusal",
          refusal_text(bad_base) != "" and n_tasks == 0 and n_sz_day == 0,
          {"verbatim": refusal_text(bad_base)[:400],
           "update_tasks": n_tasks})
    check("A14_base_identity_refusal",
          refusal_text(cross) != "" and n_tasks == 0 and n_sz_day == 0,
          {"verbatim": refusal_text(cross)[:400],
           "update_tasks": n_tasks})
    s3.close()

    # ---- A15 gate preserved: blocker list verbatim in evidence ----
    blockers = None
    for e in open(os.path.join(HERE, "run-log.jsonl")):
        row = json.loads(e)
        if row["kind"] == "accepted_daily":
            acc = row["payload"]["accepted"]
            blockers = acc.get("blocking_reasons") or blockers
    npr = "NOT_PRODUCTION_READY" in compat["adjustment_evidence"]["reason"]
    results["production_blockers"] = blockers
    check("A15_gates_preserved",
          blockers is not None and len(blockers) == 5 and npr,
          {"blocking_reasons": blockers,
           "adjustment_evidence_reason":
           compat["adjustment_evidence"]["reason"]})

    with open(os.path.join(HERE, "results.json"), "w") as f:
        json.dump(results, f, ensure_ascii=False, indent=2)
    failed = [a for a in results["assertions"] if not a["ok"]]
    print(f"{len(results['assertions'])} assertions, {len(failed)} failed")
    for a in failed:
        print("FAIL", a["name"], json.dumps(a["detail"], ensure_ascii=False)[:400])
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
