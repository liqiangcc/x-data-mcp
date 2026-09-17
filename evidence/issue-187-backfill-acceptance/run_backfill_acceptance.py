#!/usr/bin/env python3
"""Issue #187 Phase D acceptance run per the frozen plan
(evidence/issue-187-backfill-acceptance/PLAN.md).

Real mcp_data stdio + official env on retained gen-2 snapshot:
seed controlled local-gap subject (sz.000002 minus 2000-01-10) ->
verify_data_gap -> decision + proven_gap -> availability ->
backfill_data default -> run_worker_once -> new version + gap_covering
-> idempotency -> refusals R1-R8 on independent scratch DBs.
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
RUN_DIR = "/home/box/acceptance-run-issue187"
SNAP_DIR = "/home/box/acceptance-run-issue3/snapshot"
MANIFEST = os.path.join(REPO, "evidence/issue-3-acceptance-run/snapshot-manifest.json")
BAD_MANIFEST = os.path.join(RUN_DIR, "bad-manifest.json")
MCP_DATA = os.path.join(REPO, "target/debug/mcp_data")
TASK_SQLITE = os.path.join(REPO, "target/debug/task_sqlite")
SEED = os.path.join(REPO, "target/debug/seed_gap_subject")
RETAINED = "/home/box/retained-inputs"
EXPECTED = {  # frozen PLAN §1
    "base.zip": "dcdf6b49898926675698263535021aed7ad92b34be82a1a9e8a2a8982bd68bc6",
    "gbbq.zip": "daad6290c4f1cd478d7436b1e7077260a3e3e52cefd7eae58e4f24878fa2d0e9",
    "hsjday.zip": "c82f852a7bc2e52c7e89347a1e0d1878b01ce997946c2745f7af323752270892",
    "manifest": "b8a0d7b9aec1a270549d1e53ba26c0ab04842a23c05d1d1f7e75dd44a9f444d3",
}
SYMBOL = "sz.000002"
TARGET = "2000-01-10"          # frozen: missing from subject, in .day
SOURCE_MISS = "2000-01-08"     # frozen: Saturday, absent from .day
LOCAL_PRESENT = "2000-01-11"   # frozen: present in .day and full subject
STALE_DROP = "2000-01-11"      # frozen: v2 drops this to move the head
FROZEN_BAR = {"open": "-8.14", "high": "-8.06", "low": "-8.15",
              "close": "-8.06", "volume": None, "amount": None}
GBBQ_MEMBER_SHA = "4a3e9b862247a8a3fec406cc25b9a5a6ee3303f740780822519c99decaa09d77"

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


def official_env():
    env = dict(os.environ)
    env["X_DATA_MCP_OFFICIAL_MANIFEST"] = MANIFEST
    env["X_DATA_MCP_OFFICIAL_DIR"] = SNAP_DIR
    return env


def seed(db, drop_date=None, source=None, request_id=None):
    args = [SEED, db, "--symbol", SYMBOL]
    if drop_date:
        args += ["--drop-date", drop_date]
    if source:
        args += ["--source", source]
    if request_id:
        args += ["--request-id", request_id]
    p = subprocess.run(args, capture_output=True, text=True, env=official_env())
    record("seed", {"db": db, "args": args[2:], "returncode": p.returncode,
                    "stdout": p.stdout, "stderr": p.stderr})
    if p.returncode != 0:
        raise SystemExit(f"seed failed: {p.stderr[:800]}")
    return json.loads(p.stdout)


class Service:
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


def verify_request(request_id, date):
    return {"request_id": request_id, "contract_version": "v1",
            "target": {"symbol": SYMBOL, "period": "day",
                       "start_date": date, "end_date": date}}


def backfill_default(request_id):
    return {"request_id": request_id, "dataset": "market.daily",
            "contract_version": "v1", "selection": {"kind": "default"}}


def backfill_explicit(request_id, base_id, start, end):
    return {"request_id": request_id, "dataset": "market.daily",
            "contract_version": "v1",
            "items": [{"entity": entity(SYMBOL), "base_version_id": base_id,
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


def bar_hash(con, version_id, exclude=None):
    q = ("SELECT trade_date,open,high,low,close,volume,amount FROM daily_bars"
         " WHERE version_id=?")
    rows = [r for r in con.execute(q, (version_id,)).fetchall()
            if r[0] != exclude]
    return hashlib.sha256(json.dumps(rows).encode()).hexdigest()


def tables_counts(con):
    return {
        "data_versions": con.execute(
            "SELECT COUNT(*) FROM data_versions WHERE period='day'").fetchone()[0],
        "decisions": con.execute(
            "SELECT COUNT(*) FROM gap_verification_decisions").fetchone()[0],
        "proven_gaps": con.execute(
            "SELECT COUNT(*) FROM proven_gaps").fetchone()[0],
        "gap_coverings": con.execute(
            "SELECT COUNT(*) FROM gap_coverings").fetchone()[0],
    }


def availability(svc):
    return svc.tool("get_data_availability", {
        "dataset": "market.daily", "contract_version": "v1",
        "selection": {"kind": "single", "entity": entity(SYMBOL)},
        "filter": {"kind": "daily", "start_date": "1991-01-01",
                   "end_date": "2026-12-31"},
        "version": {"kind": "latest_per_entity"},
        "page_size": 10, "cursor": None})


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

    # ---- S0 ----
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
                   "seed_sha256": sha256_file(SEED),
                   "manifest_sha256": manifest_sha, "zip_sha256": hashes})

    results = {"assertions": [], "blocked_paths": {},
               "run_identity": {"head": head, "cargo": cargo,
                                "manifest_sha256": manifest_sha,
                                "zip_sha256": hashes}}

    def check(name, ok, detail):
        results["assertions"].append({"name": name, "ok": bool(ok),
                                      "detail": detail})

    check("B1_input_integrity",
          hashes["base.zip"] == EXPECTED["base.zip"]
          and hashes["gbbq.zip"] == EXPECTED["gbbq.zip"]
          and hashes["hsjday.zip"] == EXPECTED["hsjday.zip"]
          and manifest_sha == EXPECTED["manifest"],
          {"zip_sha256": hashes, "manifest_sha256": manifest_sha})

    # ---- S1/S2: seed subject on db-main ----
    db = os.path.join(RUN_DIR, "db-main.sqlite")
    fresh_db(db)
    subj = seed(db, drop_date=TARGET)
    v1, v1_digest = subj["version_id"], subj["integrity_sha256"]
    con = sqlite3.connect(db)
    row = con.execute(
        "SELECT record_count,first_date,last_date FROM data_versions"
        " WHERE version_id=?", (v1,)).fetchone()
    target_row = con.execute(
        "SELECT COUNT(*) FROM daily_bars WHERE version_id=? AND trade_date=?",
        (v1, TARGET)).fetchone()[0]
    compat = json.loads(con.execute(
        "SELECT compat_json FROM data_versions WHERE version_id=?",
        (v1,)).fetchone()[0])
    cov_reason = compat["batch"]["coverage"]["reason"]
    cat_src = con.execute(
        "SELECT source FROM catalog_versions LIMIT 1").fetchone()
    check("B2_subject_honest",
          subj["bars"] == 8478 and row[0] == 8478 and target_row == 0
          and "ACCEPTANCE-LOCAL-GAP-SUBJECT" in cov_reason
          and "not complete source history" in cov_reason
          and compat["batch"]["provenance"]["source"]
          == "tdx-official-file-snapshot"
          and compat["batch"]["provenance"].get("snapshot_inputs")
          and cat_src and cat_src[0] == "fixture:acceptance-187",
          {"bars": subj["bars"], "record_count": row[0],
           "target_row": target_row, "coverage_reason": cov_reason,
           "catalog_source": cat_src,
           "version_id": v1, "integrity_sha256": v1_digest})
    results["subject"] = {"version_id": v1, "integrity_sha256": v1_digest,
                          "coverage_reason": cov_reason}

    svc = Service(db, "present")
    svc.rpc("initialize")

    # ---- S3 verify_data_gap ----
    acc_g1 = svc.tool("verify_data_gap", verify_request("g1", TARGET))
    record("accepted_verify_g1", acc_g1)
    frozen_g = json.loads(con.execute(
        "SELECT frozen_inputs_json FROM task_items WHERE task_id=?",
        (acc_g1["task_id"],)).fetchone()[0])
    check("B3_verify_freeze",
          frozen_g.get("subject_version_id") == v1
          and frozen_g.get("subject_version_digest") == v1_digest
          and frozen_g.get("target", {}).get("start_date") == TARGET
          and frozen_g.get("target", {}).get("end_date") == TARGET,
          frozen_g)
    svc.tool("run_worker_once", {})
    outcome_g = json.loads(con.execute(
        "SELECT outcome_json FROM task_items WHERE task_id=?",
        (acc_g1["task_id"],)).fetchone()[0])
    record("verify_outcome_g1", outcome_g)
    dec = con.execute(
        "SELECT decision_id,symbol,start_date,end_date,subject_version_id,"
        "subject_version_digest,conclusion,basis_kind,basis_ref,basis_sha256,"
        "verifier_id FROM gap_verification_decisions").fetchall()
    gaps = con.execute(
        "SELECT gap_id,symbol,start_date,end_date,evidence_kind,evidence_id,"
        "evidence_digest FROM proven_gaps").fetchall()
    record("decisions", dec)
    record("proven_gaps", gaps)
    check("B4_decision_gap_atomic",
          outcome_g.get("success") is True and len(dec) == 1
          and dec[0][6] == "confirmed_missing_range"
          and dec[0][4] == v1 and dec[0][5] == v1_digest
          and dec[0][7] == "source_gap_verifier"
          and len(gaps) == 1 and gaps[0][2] == TARGET and gaps[0][3] == TARGET,
          {"outcome": outcome_g, "decisions": dec, "proven_gaps": gaps})

    # ---- S4 availability shows the open gap ----
    avail = availability(svc)
    record("availability_open", avail)
    items = avail.get("items") or []
    open_gaps = (items[0].get("confirmed_gaps") if items else []) or []
    check("B5_availability_open_gap",
          len(open_gaps) == 1
          and (open_gaps[0].get("range") or {}).get("start_date") == TARGET
          and (open_gaps[0].get("range") or {}).get("end_date") == TARGET,
          open_gaps)

    # ---- S5 backfill default ----
    acc_b1 = svc.tool("backfill_data", backfill_default("b1"))
    record("accepted_backfill_b1", acc_b1)
    frozen_b = json.loads(con.execute(
        "SELECT frozen_inputs_json FROM task_items WHERE task_id=?",
        (acc_b1["task_id"],)).fetchone()[0])
    pg = frozen_b.get("proven_gap_ranges") or []
    check("B6_backfill_freeze",
          frozen_b.get("base_version_id") == v1
          and len(pg) == 1
          and pg[0].get("range", {}).get("start_date") == TARGET
          and pg[0].get("range", {}).get("end_date") == TARGET
          and "refresh_range" not in frozen_b,
          frozen_b)
    svc.tool("run_worker_once", {})
    outcome_b = json.loads(con.execute(
        "SELECT outcome_json FROM task_items WHERE task_id=?",
        (acc_b1["task_id"],)).fetchone()[0])
    record("backfill_outcome_b1", outcome_b)
    mur = con.execute(
        "SELECT result_id,base_version_id,result_version_id,no_change"
        " FROM market_update_results WHERE task_id=?",
        (acc_b1["task_id"],)).fetchall()
    record("market_update_results_b1", mur)
    new_head = con.execute(
        "SELECT version_id FROM effective_heads WHERE symbol=? AND period='day'",
        (SYMBOL,)).fetchone()[0]
    v1_state = con.execute(
        "SELECT state FROM data_versions WHERE version_id=?", (v1,)).fetchone()[0]
    new_row = con.execute(
        "SELECT record_count,first_date,last_date FROM data_versions"
        " WHERE version_id=?", (new_head,)).fetchone()
    check("B7_backfill_executed",
          outcome_b.get("success") is True and new_head != v1,
          {"outcome": outcome_b, "new_head": new_head})

    # ---- S6 values: restored bar + outside-range invariance ----
    got_bar = dict(zip(
        ("open", "high", "low", "close", "volume", "amount"),
        con.execute(
            "SELECT open,high,low,close,volume,amount FROM daily_bars"
            " WHERE version_id=? AND trade_date=?",
            (new_head, TARGET)).fetchone()))
    query = svc.tool("query_data", {
        "dataset": "market.daily", "contract_version": "v1",
        "selection": {"kind": "single", "entity": entity(SYMBOL)},
        "filter": {"kind": "daily", "start_date": TARGET, "end_date": TARGET},
        "version": {"kind": "latest_per_entity"},
        "page_size": 10, "cursor": None})
    record("query_restored_bar", query)
    qbar = (query.get("records") or [{}])[0]
    same_outside = bar_hash(con, v1, exclude=None) == bar_hash(
        con, new_head, exclude=TARGET)
    check("B8_restored_bar",
          new_row[0] == 8479 and got_bar == FROZEN_BAR
          and all(str(qbar.get(k)) == str(FROZEN_BAR[k]) for k in FROZEN_BAR)
          and same_outside,
          {"record_count": new_row[0], "stored": got_bar,
           "query_record": qbar, "outside_hash_equal": same_outside})
    check("B9_head_governance",
          new_head == mur[0][2] and mur[0][1] == v1
          and mur[0][3] in (0, "0", "false") and v1_state == "published",
          {"new_head": new_head, "mur": mur, "v1_state": v1_state})

    # ---- S7 covering + audit history ----
    coverings = con.execute(
        "SELECT gap_id,version_id,covered_at FROM gap_coverings").fetchall()
    avail2 = availability(svc)
    open2 = ((avail2.get("items") or [{}])[0].get("confirmed_gaps")) or []
    dec2 = con.execute(
        "SELECT COUNT(*) FROM gap_verification_decisions").fetchone()[0]
    gaps2 = con.execute("SELECT COUNT(*) FROM proven_gaps").fetchone()[0]
    record("coverings", coverings)
    record("availability_after", avail2)
    check("B10_covering",
          len(coverings) == 1 and coverings[0][1] == new_head
          and not open2 and dec2 == 1 and gaps2 == 1,
          {"coverings": coverings, "open_after": open2})

    # ---- S8 idempotency ----
    rep_g = svc.tool("verify_data_gap", verify_request("g1", TARGET))
    rep_b = svc.tool("backfill_data", backfill_default("b1"))
    record("replay_g1", rep_g)
    record("replay_b1", rep_b)
    acc_g2 = svc.tool("verify_data_gap", verify_request("g2", TARGET))
    svc.tool("run_worker_once", {})
    # Re-verify binds the advanced head (v2, which contains the target
    # day) so an honest second `no_local_gap` decision is written; the
    # invariant is no new gap/covering/version (amended PLAN §8/B11).
    dec2_rows = con.execute(
        "SELECT conclusion,subject_version_id FROM"
        " gap_verification_decisions ORDER BY decided_at").fetchall()
    counts_after_replay = tables_counts(con)
    bad_b2 = svc.tool_raw("backfill_data", backfill_default("b2"))
    record("second_backfill", bad_b2)
    svc.tool("run_worker_once", {})  # worker re-run idempotent
    counts_final = tables_counts(con)
    check("B11_idempotent",
          rep_g.get("reused") is True and rep_b.get("reused") is True
          and counts_after_replay == counts_final
          and counts_final["data_versions"] == 2
          and counts_final["decisions"] == 2
          and dec2_rows[-1] == ("no_local_gap", new_head)
          and counts_final["proven_gaps"] == 1
          and counts_final["gap_coverings"] == 1,
          {"replay_g": rep_g.get("reused"), "replay_b": rep_b.get("reused"),
           "counts": counts_final, "decisions": dec2_rows,
           "second_backfill": refusal_text(bad_b2)[:300]})
    results["blocked_paths"]["second_backfill_default"] = refusal_text(bad_b2)

    # ---- S9 provenance ----
    new_compat = json.loads(con.execute(
        "SELECT compat_json FROM data_versions WHERE version_id=?",
        (new_head,)).fetchone()[0])
    inputs = new_compat["batch"]["provenance"].get("snapshot_inputs") or []
    arch = {i.get("archive_sha256") for i in inputs}
    gbbq_member = next((i.get("member_sha256") for i in inputs
                        if i.get("role") == "gbbq"), None)
    basis_ref = dec[0][8] if dec else ""
    day_member_sha = next((i.get("member_sha256") for i in inputs
                           if i.get("role") == "day"), None)
    check("B12_provenance_binds",
          {hashes["hsjday.zip"], hashes["gbbq.zip"]} <= arch
          and gbbq_member == GBBQ_MEMBER_SHA
          and manifest_sha in basis_ref
          and day_member_sha and day_member_sha in basis_ref,
          {"snapshot_inputs": inputs, "basis_ref": basis_ref,
           "basis_sha256": dec[0][9] if dec else None})
    svc.close()

    # ================= refusals =================
    # ---- R1 env-absent / R2 uninspectable manifest ----
    for tag, env_mode in (("R1_env_absent", "absent"),
                          ("R2_uninspectable_manifest", "bad-manifest")):
        dbx = os.path.join(RUN_DIR, f"db-{tag.split('_')[0].lower()}.sqlite")
        fresh_db(dbx)
        seed(dbx, drop_date=TARGET, request_id=f"{tag}-seed")
        s1 = Service(dbx, env_mode)
        s1.rpc("initialize")
        acc = s1.tool("verify_data_gap", verify_request(f"{tag}-v", TARGET))
        s1.tool("run_worker_once", {})
        xcon = sqlite3.connect(dbx)
        outcome = xcon.execute(
            "SELECT outcome_json FROM task_items WHERE task_id=?",
            (acc["task_id"],)).fetchone()[0]
        cnt = xcon.execute(
            "SELECT (SELECT COUNT(*) FROM gap_verification_decisions),"
            "(SELECT COUNT(*) FROM proven_gaps)").fetchone()
        xcon.close()
        record(f"{tag}_refusal", {"outcome": outcome, "counts": cnt})
        results["blocked_paths"][tag] = outcome
        s1.close()
        check("B13_env_absent_verify" if tag.startswith("R1")
              else "B14_uninspectable_verify",
              json.loads(outcome).get("success") is False
              and cnt == (0, 0),
              {"outcome": outcome[:400], "counts": cnt})

    # ---- R3 source-limitation / R4 no-local-gap (db-r3 full subject) ----
    db3 = os.path.join(RUN_DIR, "db-r3.sqlite")
    fresh_db(db3)
    seed(db3)  # no --drop-date: full official subject
    s3 = Service(db3, "present")
    s3.rpc("initialize")
    for tag, date, expect_concl in (
            ("R3_source_limitation", SOURCE_MISS, "source_limitation"),
            ("R4_no_local_gap", LOCAL_PRESENT, "no_local_gap")):
        acc = s3.tool("verify_data_gap", verify_request(f"{tag}-v", date))
        s3.tool("run_worker_once", {})
        r3con = sqlite3.connect(db3)
        outcome = json.loads(r3con.execute(
            "SELECT outcome_json FROM task_items WHERE task_id=?",
            (acc["task_id"],)).fetchone()[0])
        d = r3con.execute(
            "SELECT decision_id,conclusion FROM gap_verification_decisions"
            " ORDER BY decided_at").fetchall()
        ngap = r3con.execute("SELECT COUNT(*) FROM proven_gaps").fetchone()[0]
        r3con.close()
        record(f"{tag}_refusal", {"outcome": outcome, "decisions": d})
        results["blocked_paths"][tag] = {"outcome": outcome, "decisions": d}
        last_concl = d[-1][1] if d else None
        check("B15_source_limitation" if tag.startswith("R3")
              else "B16_no_local_gap",
              outcome.get("success") is True and last_concl == expect_concl
              and ngap == 0,
              {"conclusion": last_concl, "proven_gaps": ngap})
    s3.close()

    # ---- R5 subject identity mismatch (db-r5, fixture-source subject) ----
    db5 = os.path.join(RUN_DIR, "db-r5.sqlite")
    fresh_db(db5)
    seed(db5, drop_date=TARGET, source="fixture", request_id="r5-seed")
    s5 = Service(db5, "present")
    s5.rpc("initialize")
    acc5 = s5.tool("verify_data_gap", verify_request("r5-v", TARGET))
    s5.tool("run_worker_once", {})
    r5con = sqlite3.connect(db5)
    out5 = r5con.execute(
        "SELECT outcome_json FROM task_items WHERE task_id=?",
        (acc5["task_id"],)).fetchone()[0]
    cnt5 = r5con.execute(
        "SELECT (SELECT COUNT(*) FROM gap_verification_decisions),"
        "(SELECT COUNT(*) FROM proven_gaps)").fetchone()
    r5con.close()
    record("R5_refusal", {"outcome": out5, "counts": cnt5})
    results["blocked_paths"]["R5_subject_identity"] = out5
    check("B17_subject_identity",
          json.loads(out5).get("success") is False and cnt5 == (0, 0),
          {"outcome": out5[:400], "counts": cnt5})
    s5.close()

    # ---- R6 stale-head (db-r6) ----
    db6 = os.path.join(RUN_DIR, "db-r6.sqlite")
    fresh_db(db6)
    seed(db6, drop_date=TARGET, request_id="r6-seed-v1")
    s6 = Service(db6, "present")
    s6.rpc("initialize")
    acc6 = s6.tool("verify_data_gap", verify_request("r6-v", TARGET))
    # run_worker_once claims only tasks accepted by its own run id, so the
    # accepting service must stay alive while the seed moves the head.
    seed(db6, drop_date=STALE_DROP, request_id="r6-seed-v2")
    s6.tool("run_worker_once", {})
    r6con = sqlite3.connect(db6)
    out6 = r6con.execute(
        "SELECT outcome_json FROM task_items WHERE task_id=?",
        (acc6["task_id"],)).fetchone()[0]
    cnt6 = r6con.execute(
        "SELECT (SELECT COUNT(*) FROM gap_verification_decisions),"
        "(SELECT COUNT(*) FROM proven_gaps)").fetchone()
    nv6 = r6con.execute(
        "SELECT COUNT(*) FROM data_versions WHERE period='day'").fetchone()[0]
    r6con.close()
    record("R6_refusal", {"outcome": out6, "counts": cnt6, "versions": nv6})
    results["blocked_paths"]["R6_stale_head"] = out6
    check("B18_stale_head",
          json.loads(out6).get("success") is False and cnt6 == (0, 0)
          and nv6 == 2,
          {"outcome": out6[:400], "counts": cnt6, "versions": nv6})
    s6.close()

    # ---- R7 backfill no gap / R8 explicit backfill blocked (db-r7) ----
    db7 = os.path.join(RUN_DIR, "db-r7.sqlite")
    fresh_db(db7)
    full = seed(db7, request_id="r7-seed")
    s7 = Service(db7, "present")
    s7.rpc("initialize")
    r7 = s7.tool_raw("backfill_data", backfill_default("r7-b"))
    record("R7_refusal", r7)
    results["blocked_paths"]["R7_no_frozen_gap"] = refusal_text(r7)
    acc8 = s7.tool("backfill_data", backfill_explicit(
        "r8-b", full["version_id"], LOCAL_PRESENT, LOCAL_PRESENT))
    s7.tool("run_worker_once", {})
    r7con = sqlite3.connect(db7)
    out8 = r7con.execute(
        "SELECT outcome_json FROM task_items WHERE task_id=?",
        (acc8["task_id"],)).fetchone()[0]
    nv7 = r7con.execute(
        "SELECT COUNT(*) FROM data_versions WHERE period='day'").fetchone()[0]
    r7con.close()
    record("R8_refusal", {"outcome": out8, "versions": nv7})
    results["blocked_paths"]["R8_explicit_backfill"] = out8
    check("B19_backfill_no_gap",
          bool(r7.get("error"))
          or (r7.get("result") or {}).get("isError") is True,
          refusal_text(r7)[:300])
    check("B20_explicit_backfill_blocked",
          json.loads(out8).get("success") is False
          and "NOT_PRODUCTION_READY" in out8 and nv7 == 1,
          {"outcome": out8[:400], "versions": nv7})
    s7.close()

    # ---- B22 gates preserved ----
    # The publish-path accept (backfill) reports all five production
    # blockers; the verify accept reports only its operation-relevant
    # blocker. Both verbatim into results.
    blockers = verify_blockers = None
    for e in open(os.path.join(HERE, "run-log.jsonl")):
        r = json.loads(e)
        if r["kind"] == "accepted_backfill_b1":
            blockers = r["payload"].get("blocking_reasons") or blockers
        if r["kind"] == "accepted_verify_g1":
            verify_blockers = r["payload"].get("blocking_reasons") \
                or verify_blockers
    npr = "NOT_PRODUCTION_READY" in compat["adjustment_evidence"]["reason"]
    results["production_blockers"] = blockers
    check("B22_gates_preserved",
          blockers is not None and len(blockers) == 5
          and verify_blockers == ["GAP_BASIS_NOT_VERIFIED (#54/#46/#71: "
                                  "no trusted source-gap verifier)"]
          and npr,
          {"blocking_reasons": blockers,
           "verify_blocking_reasons": verify_blockers,
           "adjustment_evidence_reason":
           compat["adjustment_evidence"]["reason"]})
    results["notes"] = {
        "partial_coverage_no_covering": "NOT_OBSERVABLE_VIA_REAL_SOURCE: "
            "committed range equals the frozen union by construction; the "
            "invariant is unit-test covered (ensure_default_coverings).",
        "changed_generation_update": "NOT_RUN: only one pinned official "
            "snapshot generation exists (gen-1 permanently unavailable).",
    }

    with open(os.path.join(HERE, "results.json"), "w") as f:
        json.dump(results, f, ensure_ascii=False, indent=2)
    failed = [a for a in results["assertions"] if not a["ok"]]
    print(f"{len(results['assertions'])} assertions, {len(failed)} failed")
    for a in failed:
        print("FAIL", a["name"],
              json.dumps(a["detail"], ensure_ascii=False)[:400])
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
