#!/usr/bin/env python3
"""Issue #194 PLAN-v2 acceptance run (P1 ruling 5710586446).

Real mcp_data stdio + official env on retained gen-2 snapshot:
start_collection securities.catalog -> run_worker_once -> item success
with verbatim reject_candidate exclusion trail -> catalog publishes the
5224 named members -> idempotent re-collection (content-identical
second version) -> readback -> refusal legs R1-R4 on independent
scratch DBs. Mechanical expectations recomputed from retained bytes,
never from the implementation under test. v1 artifacts untouched.
"""
import datetime
import hashlib
import io
import json
import os
import sqlite3
import subprocess
import sys
import zipfile

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
HERE = os.path.dirname(os.path.abspath(__file__))
RUN_DIR = "/home/box/acceptance-run-issue194"
SNAP_DIR = "/home/box/acceptance-run-issue3/snapshot"
MANIFEST = os.path.join(REPO, "evidence/issue-3-acceptance-run/snapshot-manifest.json")
BAD_MANIFEST = os.path.join(RUN_DIR, "bad-manifest.json")
MCP_DATA = os.path.join(REPO, "target/debug/mcp_data")
TASK_SQLITE = os.path.join(REPO, "target/debug/task_sqlite")
EXPECTED = {  # PLAN §1 pins (unchanged by v2)
    "base.zip": "dcdf6b49898926675698263535021aed7ad92b34be82a1a9e8a2a8982bd68bc6",
    "gbbq.zip": "daad6290c4f1cd478d7436b1e7077260a3e3e52cefd7eae58e4f24878fa2d0e9",
    "manifest": "b8a0d7b9aec1a270549d1e53ba26c0ab04842a23c05d1d1f7e75dd44a9f444d3",
}
MEMBER_PINS = {
    "tdxhy.cfg": "df7cb7051d0f73a0d1381e9b36eafa33c8f5081d28ddef87cad406dd28ed8ea6",
    "infoharbor_ex.code": "79eed0f6857ffde3d1d3cc8966ceff6e400f45e6e2387224446e453850fb5281",
    "zhb.zip": "e1ded566dc5269642de760211ec1b67e5ebebd67ab1e6338f70cab7c05048350",
    "incon.dat": "86f758b29033632fae23b85a7f002b4a6906fb88a3d2904495865c3b2cfb6f51",
}
FROZEN = {  # PLAN §2 mechanical expectations (unchanged)
    "total": 5659, "scope": 5230, "scope_sz": 2906, "scope_sh": 2324,
    "bj": 348, "b_share": 78, "anomalous": 3, "ex_names": 5571,
    "tdxrshy": 474, "tdxnhy": 145,
    "nameless": ["0:001235", "0:300728", "1:601206", "1:603302",
                 "1:603361", "1:688688"],
}
FROZEN_V2 = {  # PLAN-v2 §v2.1 published-side expectations
    "published": 5224,
    "source": "official-file:b8a0d7b9aec1a270",
    "missing_industry": 0,
    "t_code_residuals": 0,
}

logf = open(os.path.join(HERE, "run-log-v2.jsonl"), "w")


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


def sha256_bytes(b):
    return hashlib.sha256(b).hexdigest()


def mechanical_recompute():
    """Independent parse of retained bytes (PLAN §2 anchored formats)."""
    base = zipfile.ZipFile(os.path.join(SNAP_DIR, "base.zip"))
    gbbq = zipfile.ZipFile(os.path.join(SNAP_DIR, "gbbq.zip"))
    members = {
        "tdxhy.cfg": base.read("tdxhy.cfg"),
        "infoharbor_ex.code": base.read("infoharbor_ex.code"),
        "zhb.zip": gbbq.read("zhb.zip"),
    }
    members["incon.dat"] = zipfile.ZipFile(
        io.BytesIO(members["zhb.zip"])).read("incon.dat")
    member_sha = {k: sha256_bytes(v) for k, v in members.items()}

    rows = []
    for line in members["tdxhy.cfg"].decode("ascii").split("\r\n"):
        if not line:
            continue
        f = line.split("|")
        assert len(f) == 6, line
        rows.append((f[0], f[1], f[2], f[5]))
    scope = [r for r in rows if r[0] in "01" and r[2] and r[3]]
    rest = [r for r in rows if r[0] in "01" and not (r[2] and r[3])]
    counts = {
        "total": len(rows), "scope": len(scope),
        "scope_sz": sum(1 for r in scope if r[0] == "0"),
        "scope_sh": sum(1 for r in scope if r[0] == "1"),
        "bj": sum(1 for r in rows if r[0] == "2"),
        "b_share": sum(1 for r in rest if r[1].startswith(("200", "900"))),
        "anomalous": sum(1 for r in rest
                       if not r[1].startswith(("200", "900"))),
    }
    names = {}
    for line in members["infoharbor_ex.code"].decode("gb18030").split("\r\n"):
        if not line:
            continue
        f = line.split("|")
        assert len(f) >= 3 and f[0] not in names
        names[f[0]] = f[1]
    sections, cur = {}, None
    for line in members["incon.dat"].decode("gb18030").split("\r\n"):
        if not line:
            continue
        if line.startswith("#"):
            cur = line[1:]
            sections[cur] = []
        else:
            sections[cur].append(line.split("|"))
    rshy = {f[0] for f in sections["TDXRSHY"]}
    tnhy = {f[0] for f in sections.get("TDXNHY", [])}

    def l2(x):
        if len(x) in (3, 5, 7) and x[0].isalpha() and x[1:].isdigit():
            lv = (len(x) - 1) // 2
            return x if lv == 2 else (x[:5] if lv == 3 else None)
        return None

    nameless = [f"{m}:{c}" for m, c, t, x in scope if c not in names]
    published = [r for r in scope if r[1] in names]
    recomputed = dict(counts)
    recomputed.update({
        "ex_names": len(names), "tdxrshy": len(rshy),
        "tdxnhy": len(tnhy), "nameless": nameless,
        "published": len(published),
        "published_missing_industry": sum(
            1 for m, c, t, x in published
            if l2(x) is None or l2(x) not in rshy),
        "published_t_residuals": [
            f"{m}:{c} t={t}" for m, c, t, x in published
            if t not in tnhy],
    })
    record("mechanical_recompute", {"member_sha256": member_sha,
                                    "recomputed": recomputed})
    return member_sha, recomputed


class Service:
    def __init__(self, db, env_mode="present", snap_dir=None):
        env = dict(os.environ)
        env.pop("X_DATA_MCP_OFFICIAL_MANIFEST", None)
        env.pop("X_DATA_MCP_OFFICIAL_DIR", None)
        if env_mode == "present":
            env["X_DATA_MCP_OFFICIAL_MANIFEST"] = MANIFEST
            env["X_DATA_MCP_OFFICIAL_DIR"] = snap_dir or SNAP_DIR
        elif env_mode == "bad-manifest":
            env["X_DATA_MCP_OFFICIAL_MANIFEST"] = BAD_MANIFEST
            env["X_DATA_MCP_OFFICIAL_DIR"] = snap_dir or SNAP_DIR
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


def catalog_request(request_id):
    return {"request_id": request_id, "dataset": "securities.catalog",
            "contract_version": "v1",
            "selection": {"scope": "current_sh_sz_a"}}


def catalog_query(cursor):
    return {"dataset": "securities.catalog", "contract_version": "v1",
            "selection": {"kind": "catalog", "scope": "current_sh_sz_a"},
            "filter": {"kind": "catalog", "code": None,
                       "name_query": None, "industry_code": None},
            "version": {"kind": "latest_per_entity"},
            "page_size": 1000, "cursor": cursor}


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


def item_outcome(db, task_id):
    con = sqlite3.connect(db)
    row = con.execute(
        "SELECT outcome_json FROM task_items WHERE task_id=?",
        (task_id,)).fetchone()
    con.close()
    return json.loads(row[0]) if row and row[0] else None


def catalog_state(db):
    con = sqlite3.connect(db)
    versions = con.execute(
        "SELECT version_id,source,digest FROM catalog_versions "
        "ORDER BY published_at").fetchall()
    head = con.execute(
        "SELECT version_id FROM catalog_head").fetchone()
    members = con.execute(
        "SELECT version_id,exchange,code,name,source_security_id,"
        "industry_status,industry_code,industry_name,industry_reason "
        "FROM catalog_members ORDER BY version_id,exchange,code").fetchall()
    con.close()
    return {"versions": versions, "head": head[0] if head else None,
            "member_count": len(members), "members": members}


def main():
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
    arch = {n: sha256_file(os.path.join(SNAP_DIR, n))
            for n in ("base.zip", "gbbq.zip")}
    manifest_sha = sha256_file(MANIFEST)
    member_sha, recomputed = mechanical_recompute()
    record("env", {"run_dir": RUN_DIR, "head": head, "cargo": cargo,
                   "mcp_data_sha256": sha256_file(MCP_DATA),
                   "task_sqlite_sha256": sha256_file(TASK_SQLITE),
                   "manifest_sha256": manifest_sha, "archive_sha256": arch})

    results = {"assertions": [], "blocked_paths": {},
               "run_identity": {"head": head, "cargo": cargo,
                                "manifest_sha256": manifest_sha,
                                "archive_sha256": arch,
                                "member_sha256": member_sha,
                                "mechanical_recompute": recomputed}}

    def check(name, ok, detail):
        results["assertions"].append({"name": name, "ok": bool(ok),
                                      "detail": detail})

    check("V1_input_integrity",
          arch["base.zip"] == EXPECTED["base.zip"]
          and arch["gbbq.zip"] == EXPECTED["gbbq.zip"]
          and manifest_sha == EXPECTED["manifest"],
          {"archive_sha256": arch, "manifest_sha256": manifest_sha})
    check("V2_member_sha256", member_sha == MEMBER_PINS, member_sha)
    check("V3_scope_partition",
          recomputed["total"] == FROZEN["total"]
          and recomputed["scope"] == FROZEN["scope"]
          and recomputed["scope_sz"] == FROZEN["scope_sz"]
          and recomputed["scope_sh"] == FROZEN["scope_sh"]
          and recomputed["bj"] == FROZEN["bj"]
          and recomputed["b_share"] == FROZEN["b_share"]
          and recomputed["anomalous"] == FROZEN["anomalous"]
          and recomputed["ex_names"] == FROZEN["ex_names"]
          and recomputed["tdxrshy"] == FROZEN["tdxrshy"]
          and recomputed["tdxnhy"] == FROZEN["tdxnhy"]
          and recomputed["nameless"] == FROZEN["nameless"]
          and recomputed["published"] == FROZEN_V2["published"]
          and recomputed["published_missing_industry"]
              == FROZEN_V2["missing_industry"]
          and recomputed["published_t_residuals"] == [],
          recomputed)

    # ---- collection on db2-main ----
    db = os.path.join(RUN_DIR, "db2-main.sqlite")
    fresh_db(db)
    svc = Service(db, "present")
    svc.rpc("initialize")
    acc1 = svc.tool("start_collection", catalog_request("c1"))
    record("accepted_c1", acc1)
    svc.tool("run_worker_once", {})
    outcome = item_outcome(db, acc1["task_id"])
    state1 = catalog_state(db)
    record("item_outcome_c1", outcome)
    record("catalog_state_after_c1",
           {k: v for k, v in state1.items() if k != "members"}
           | {"member_rows_sample": state1["members"][:3]})

    evidence = (outcome or {}).get("evidence") or {}
    exclusions = [e.get("member") for e in
                  evidence.get("reject_candidate_exclusions", [])]
    check("V4_success_exclusion_trail",
          outcome is not None and outcome.get("success") is True
          and exclusions == FROZEN["nameless"]
          and all("reject_candidate" in (e.get("reason") or "")
                  for e in evidence.get("reject_candidate_exclusions", []))
          and evidence.get("published_members") == FROZEN_V2["published"]
          and evidence.get("missing_industry") == []
          and evidence.get("t_code_residuals") == [],
          {"outcome": outcome})
    check("V5_publish_counts",
          state1["member_count"] == FROZEN_V2["published"]
          and len(state1["versions"]) == 1
          and state1["head"] == state1["versions"][0][0]
          and all(m[4].startswith("tdxhy:") for m in state1["members"]),
          {"versions": state1["versions"], "head": state1["head"],
           "member_count": state1["member_count"]})
    check("V6_provenance_binding",
          state1["versions"]
          and state1["versions"][0][1] == FROZEN_V2["source"],
          {"source": state1["versions"][0][1] if state1["versions"]
           else None, "expected": FROZEN_V2["source"],
           "member_sha256": member_sha})

    # ---- idempotent replay + re-collection ----
    v1_digest = state1["versions"][0][2] if state1["versions"] else None
    rep = svc.tool("start_collection", catalog_request("c1"))
    record("replay_c1", rep)
    state_after_replay = catalog_state(db)
    record("catalog_state_after_replay",
           {k: v for k, v in state_after_replay.items()
            if k != "members"})
    acc2 = svc.tool("start_collection", catalog_request("c2"))
    svc.tool("run_worker_once", {})
    outcome2 = item_outcome(db, acc2["task_id"])
    state2 = catalog_state(db)
    worker_again = svc.tool("run_worker_once", {})
    record("recollection", {"task_id_c2": acc2["task_id"],
                            "outcome_c2": outcome2,
                            "catalog_state": {
                                k: v for k, v in state2.items()
                                if k != "members"},
                            "worker_again": worker_again})
    v1_after = [m[1:] for m in state2["members"]
                if m[0] == state1["versions"][0][0]] if state1[
                    "versions"] else []
    v1_members_before = sorted(m[1:] for m in state1["members"])
    v2_members = sorted(m[1:] for m in state2["members"]
                        if m[0] == state2["head"])
    check("V7_version_immutable",
          len(state2["versions"]) >= 1
          and state2["versions"][0][2] == v1_digest
          and v1_after == [m[1:] for m in state1["members"]],
          {"v1_digest_before": v1_digest,
           "v1_digest_after": state2["versions"][0][2]
           if state2["versions"] else None})
    check("V8_idempotent",
          rep.get("reused") is True
          and len(state_after_replay["versions"]) == 1
          and acc2["task_id"] != acc1["task_id"]
          and outcome2 is not None and outcome2.get("success") is True
          and len(state2["versions"]) == 2
          and state2["head"] != state1["versions"][0][0]
          and state2["head"] == state2["versions"][1][0]
          and v2_members == v1_members_before
          and worker_again.get("idle") is True,
          {"reused": rep.get("reused"),
           "versions": state2["versions"], "head": state2["head"],
           "v2_members_equal_v1": v2_members == v1_members_before,
           "idle": worker_again.get("idle"),
           "outcome_c2_success": (outcome2 or {}).get("success")})

    # ---- readback ----
    avail = svc.tool("get_data_availability", {
        "dataset": "securities.catalog", "contract_version": "v1",
        "selection": {"kind": "catalog", "scope": "current_sh_sz_a"},
        "filter": {"kind": "catalog"},
        "version": {"kind": "latest_per_entity"},
        "page_size": 10, "cursor": None})
    record("catalog_availability", avail)
    rows, cursor, pages = [], None, 0
    while True:
        page = svc.tool("query_data", catalog_query(cursor))
        pages += 1
        rows.extend(page.get("records", []))
        cursor = page.get("next_cursor")
        if not cursor:
            break
        if pages > 10:
            raise SystemExit("pagination did not converge")
    record("catalog_query_readback",
           {"pages": pages, "record_count": len(rows),
            "first_row": rows[0] if rows else None})
    versions = svc.tool("list_data_versions", {
        "dataset": "securities.catalog", "contract_version": "v1",
        "selection": {"kind": "catalog", "scope": "current_sh_sz_a"},
        "page_size": 1000, "cursor": None})
    record("catalog_versions_read", versions)
    member_keys = sorted(
        (m[1], m[2], m[3]) for m in state2["members"]
        if m[0] == state2["head"])
    row_keys = sorted(
        (r["entity"]["exchange"], r["entity"]["code"], r["name"])
        for r in rows)
    check("V9_readback",
          len(rows) == FROZEN_V2["published"]
          and row_keys == member_keys
          and avail.get("items")
          and avail["items"][0].get("availability") != "absent"
          and isinstance(versions, dict),
          {"paged_records": len(rows), "pages": pages,
           "row_keys_equal_members": row_keys == member_keys,
           "availability": avail, "versions": versions})
    svc.close()

    # ---- refusal legs R1-R4 (independent scratch DBs) ----
    legs = []

    # R1 env-absent
    db1 = os.path.join(RUN_DIR, "db2-r1.sqlite")
    fresh_db(db1)
    s1 = Service(db1, "absent")
    s1.rpc("initialize")
    a1 = s1.tool("start_collection", catalog_request("r1-c"))
    s1.tool("run_worker_once", {})
    o1 = item_outcome(db1, a1["task_id"])
    st1 = catalog_state(db1)
    record("R1_refusal", {"outcome": o1,
                          "catalog_counts": (len(st1["versions"]),
                                             st1["member_count"],
                                             st1["head"] is not None)})
    results["blocked_paths"]["R1_env_absent"] = o1
    check("V10_env_absent",
          o1 is not None and o1.get("success") is False
          and "SOURCE_NOT_VERIFIED" in (o1.get("error") or "")
          and not st1["versions"] and st1["member_count"] == 0
          and st1["head"] is None,
          {"outcome": o1})
    s1.close()

    # R2 uninspectable manifest
    db2 = os.path.join(RUN_DIR, "db2-r2.sqlite")
    fresh_db(db2)
    s2 = Service(db2, "bad-manifest")
    s2.rpc("initialize")
    a2 = s2.tool("start_collection", catalog_request("r2-c"))
    s2.tool("run_worker_once", {})
    o2 = item_outcome(db2, a2["task_id"])
    st2 = catalog_state(db2)
    record("R2_refusal", {"outcome": o2,
                          "catalog_counts": (len(st2["versions"]),
                                             st2["member_count"],
                                             st2["head"] is not None)})
    results["blocked_paths"]["R2_uninspectable_manifest"] = o2
    legs.append(("R2", o2, st2))
    s2.close()

    # R3 member missing (scratch snapshot dir without base.zip)
    snap_r3 = os.path.join(RUN_DIR, "snap2-r3")
    os.makedirs(snap_r3, exist_ok=True)
    for n in ("gbbq.zip", "hsjday.zip"):
        dst = os.path.join(snap_r3, n)
        if not os.path.exists(dst):
            os.link(os.path.join(SNAP_DIR, n), dst)
    db3 = os.path.join(RUN_DIR, "db2-r3.sqlite")
    fresh_db(db3)
    s3 = Service(db3, "present", snap_dir=snap_r3)
    s3.rpc("initialize")
    a3 = s3.tool("start_collection", catalog_request("r3-c"))
    s3.tool("run_worker_once", {})
    o3 = item_outcome(db3, a3["task_id"])
    st3 = catalog_state(db3)
    record("R3_refusal", {"outcome": o3,
                          "catalog_counts": (len(st3["versions"]),
                                             st3["member_count"],
                                             st3["head"] is not None)})
    results["blocked_paths"]["R3_member_missing"] = o3
    legs.append(("R3", o3, st3))
    s3.close()

    # R4 member corrupt (scratch snapshot dir, base.zip one byte flipped)
    snap_r4 = os.path.join(RUN_DIR, "snap2-r4")
    os.makedirs(snap_r4, exist_ok=True)
    for n in ("gbbq.zip", "hsjday.zip"):
        dst = os.path.join(snap_r4, n)
        if not os.path.exists(dst):
            os.link(os.path.join(SNAP_DIR, n), dst)
    corrupted = bytearray(open(os.path.join(SNAP_DIR, "base.zip"), "rb")
                          .read())
    corrupted[1000] ^= 0xFF
    with open(os.path.join(snap_r4, "base.zip"), "wb") as f:
        f.write(corrupted)
    record("R4_corrupt", {"flipped_byte_offset": 1000,
                          "corrupt_base_sha256": sha256_bytes(corrupted)})
    db4 = os.path.join(RUN_DIR, "db2-r4.sqlite")
    fresh_db(db4)
    s4 = Service(db4, "present", snap_dir=snap_r4)
    s4.rpc("initialize")
    a4 = s4.tool("start_collection", catalog_request("r4-c"))
    s4.tool("run_worker_once", {})
    o4 = item_outcome(db4, a4["task_id"])
    st4 = catalog_state(db4)
    record("R4_refusal", {"outcome": o4,
                          "catalog_counts": (len(st4["versions"]),
                                             st4["member_count"],
                                             st4["head"] is not None)})
    results["blocked_paths"]["R4_member_corrupt"] = o4
    legs.append(("R4", o4, st4))
    s4.close()

    check("V11_source_fail_closed",
          all(o is not None and o.get("success") is False
              and "CATALOG_SOURCE_FAILURE" in (o.get("error") or "")
              and not st["versions"] and st["member_count"] == 0
              and st["head"] is None
              for _, o, st in legs),
          {"legs": {n: {"outcome": o,
                        "catalog_counts": (len(st["versions"]),
                                           st["member_count"],
                                           st["head"] is not None)}
                    for n, o, st in legs}})

    # ---- gates ----
    blockers = acc1.get("blocking_reasons")
    results["production_blockers"] = blockers
    check("V12_gates_preserved",
          isinstance(blockers, list) and len(blockers) == 4
          and any("CATALOG_PUBLISH_NOT_VERIFIED" in b for b in blockers),
          {"blocking_reasons": blockers})
    results["notes"] = {
        "plan": "PLAN-v2 (P1 ruling 5710586446); v1 run record preserved "
                "in run-log.jsonl/results.json/README.md",
        "not_run": "market.daily 5230 collection, deployment, gen-3 "
                   "snapshot — out of scope per issue.",
    }

    with open(os.path.join(HERE, "results-v2.json"), "w") as f:
        json.dump(results, f, ensure_ascii=False, indent=2)
    failed = [a for a in results["assertions"] if not a["ok"]]
    print(f"{len(results['assertions'])} assertions, {len(failed)} failed")
    for a in failed:
        print("FAIL", a["name"],
              json.dumps(a["detail"], ensure_ascii=False)[:400])
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
