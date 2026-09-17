#!/usr/bin/env python3
"""Issue #194 Phase B/C acceptance run per the frozen plan
(evidence/issue-194-catalog-acceptance/PLAN.md).

Real mcp_data stdio + official env on retained gen-2 snapshot:
start_collection securities.catalog -> run_worker_once -> expected
contract-correct CATALOG_REJECTED (six nameless members verbatim) ->
idempotency -> catalog readback -> refusal legs R1-R4 on independent
scratch DBs. Mechanical expectations recomputed from retained bytes,
never from the implementation under test.
"""
import datetime
import hashlib
import io
import json
import os
import shutil
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
RETAINED = "/home/box/retained-inputs"
EXPECTED = {  # PLAN §1 pins
    "base.zip": "dcdf6b49898926675698263535021aed7ad92b34be82a1a9e8a2a8982bd68bc6",
    "gbbq.zip": "daad6290c4f1cd478d7436b1e7077260a3e3e52cefd7eae58e4f24878fa2d0e9",
    "manifest": "b8a0d7b9aec1a270549d1e53ba26c0ab04842a23c05d1d1f7e75dd44a9f444d3",
}
MEMBER_PINS = {  # PLAN §1 member table
    "tdxhy.cfg": "df7cb7051d0f73a0d1381e9b36eafa33c8f5081d28ddef87cad406dd28ed8ea6",
    "infoharbor_ex.code": "79eed0f6857ffde3d1d3cc8966ceff6e400f45e6e2387224446e453850fb5281",
    "zhb.zip": "e1ded566dc5269642de760211ec1b67e5ebebd67ab1e6338f70cab7c05048350",
    "incon.dat": "86f758b29033632fae23b85a7f002b4a6906fb88a3d2904495865c3b2cfb6f51",
}
FROZEN = {  # PLAN §2 mechanical expectations
    "total": 5659, "scope": 5230, "scope_sz": 2906, "scope_sh": 2324,
    "bj": 348, "b_share": 78, "anomalous": 3, "ex_names": 5571,
    "tdxrshy": 474, "tdxnhy": 145,
    "nameless": ["0:001235", "0:300728", "1:601206", "1:603302",
                 "1:603361", "1:688688"],
    "missing_industry": 0, "t_residuals": [("1", "688688", "T00")],
}
EXPECTED_ERROR = ("CATALOG_REJECTED: nameless scope members: "
                  + ",".join(FROZEN["nameless"]))

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
    missing_ind = sum(1 for m, c, t, x in scope
                      if l2(x) is None or l2(x) not in rshy)
    tresid = [(m, c, t) for m, c, t, x in scope if t not in tnhy]
    recomputed = dict(counts)
    recomputed.update({
        "ex_names": len(names), "tdxrshy": len(rshy),
        "tdxnhy": len(tnhy), "nameless": nameless,
        "missing_industry": missing_ind,
        "t_residuals": tresid,
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


def item_error(db, task_id):
    con = sqlite3.connect(db)
    outcome = con.execute(
        "SELECT outcome_json FROM task_items WHERE task_id=?",
        (task_id,)).fetchone()
    cat = con.execute(
        "SELECT (SELECT COUNT(*) FROM catalog_versions),"
        "(SELECT COUNT(*) FROM catalog_members),"
        "(SELECT COUNT(*) FROM catalog_head)").fetchone()
    con.close()
    return (json.loads(outcome[0]) if outcome and outcome[0] else None, cat)


def catalog_counts(db):
    con = sqlite3.connect(db)
    c = con.execute(
        "SELECT (SELECT COUNT(*) FROM catalog_versions),"
        "(SELECT COUNT(*) FROM catalog_members),"
        "(SELECT COUNT(*) FROM catalog_head),"
        "(SELECT COUNT(*) FROM tasks)").fetchone()
    con.close()
    return c


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

    check("C1_input_integrity",
          arch["base.zip"] == EXPECTED["base.zip"]
          and arch["gbbq.zip"] == EXPECTED["gbbq.zip"]
          and manifest_sha == EXPECTED["manifest"],
          {"archive_sha256": arch, "manifest_sha256": manifest_sha})
    check("C2_member_sha256", member_sha == MEMBER_PINS, member_sha)
    check("C3_scope_partition",
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
          and recomputed["missing_industry"] == FROZEN["missing_industry"]
          and recomputed["t_residuals"] == FROZEN["t_residuals"],
          recomputed)

    # ---- S1/S2: catalog collection on db-main ----
    db = os.path.join(RUN_DIR, "db-main.sqlite")
    fresh_db(db)
    svc = Service(db, "present")
    svc.rpc("initialize")
    acc1 = svc.tool("start_collection", catalog_request("c1"))
    record("accepted_c1", acc1)
    svc.tool("run_worker_once", {})
    outcome, cat = item_error(db, acc1["task_id"])
    record("item_outcome_c1", {"outcome": outcome, "catalog_counts": cat})
    record("expected_error", EXPECTED_ERROR)

    check("C4_nameless_verbatim",
          outcome is not None and outcome.get("success") is False
          and outcome.get("error") == EXPECTED_ERROR,
          {"outcome": outcome, "expected": EXPECTED_ERROR})
    check("C5_no_leak",
          cat == (0, 0, 0), {"catalog_counts": cat})

    # ---- S4 idempotency ----
    rep = svc.tool("start_collection", catalog_request("c1"))
    record("replay_c1", rep)
    acc2 = svc.tool("start_collection", catalog_request("c2"))
    svc.tool("run_worker_once", {})
    outcome2, cat2 = item_error(db, acc2["task_id"])
    worker_again = svc.tool("run_worker_once", {})
    record("replay_results", {"replayed_c1": rep, "task_id_c2":
                              acc2["task_id"], "outcome_c2": outcome2,
                              "worker_again": worker_again})
    check("C6_idempotent",
          rep.get("reused") is True
          and acc2["task_id"] != acc1["task_id"]
          and outcome2 is not None
          and outcome2.get("error") == EXPECTED_ERROR
          and worker_again.get("idle") is True
          and cat2 == (0, 0, 0),
          {"reused": rep.get("reused"), "outcome_c2": outcome2,
           "idle": worker_again.get("idle"), "catalog_counts": cat2})

    # ---- S5 catalog readback ----
    avail = svc.tool("get_data_availability", {
        "dataset": "securities.catalog", "contract_version": "v1",
        "selection": {"kind": "catalog", "scope": "current_sh_sz_a"},
        "filter": {"kind": "catalog"},
        "version": {"kind": "latest_per_entity"},
        "page_size": 10, "cursor": None})
    versions = svc.tool("list_data_versions", {
        "dataset": "securities.catalog", "contract_version": "v1",
        "selection": {"kind": "catalog", "scope": "current_sh_sz_a"}})
    record("catalog_availability", avail)
    record("catalog_versions_read", versions)
    check("C7_readback_honest",
          isinstance(avail, dict) and isinstance(versions, dict),
          {"availability": avail, "versions": versions})
    svc.close()

    # ---- S6 refusal legs ----
    # R1 env-absent
    db1 = os.path.join(RUN_DIR, "db-r1.sqlite")
    fresh_db(db1)
    s1 = Service(db1, "absent")
    s1.rpc("initialize")
    a1 = s1.tool("start_collection", catalog_request("r1-c"))
    s1.tool("run_worker_once", {})
    o1, c1 = item_error(db1, a1["task_id"])
    record("R1_refusal", {"outcome": o1, "catalog_counts": c1})
    results["blocked_paths"]["R1_env_absent"] = o1
    check("C8_env_absent",
          o1 is not None and o1.get("success") is False
          and "SOURCE_NOT_VERIFIED" in (o1.get("error") or "")
          and c1 == (0, 0, 0),
          {"outcome": o1, "catalog_counts": c1})
    s1.close()

    # R2 uninspectable manifest
    db2 = os.path.join(RUN_DIR, "db-r2.sqlite")
    fresh_db(db2)
    s2 = Service(db2, "bad-manifest")
    s2.rpc("initialize")
    a2 = s2.tool("start_collection", catalog_request("r2-c"))
    s2.tool("run_worker_once", {})
    o2, c2 = item_error(db2, a2["task_id"])
    record("R2_refusal", {"outcome": o2, "catalog_counts": c2})
    results["blocked_paths"]["R2_uninspectable_manifest"] = o2
    check("C9_uninspectable",
          o2 is not None and o2.get("success") is False
          and "CATALOG_SOURCE_FAILURE" in (o2.get("error") or "")
          and c2 == (0, 0, 0),
          {"outcome": o2, "catalog_counts": c2})
    s2.close()

    # R3 member missing (scratch snapshot dir without base.zip)
    snap_r3 = os.path.join(RUN_DIR, "snap-r3")
    os.makedirs(snap_r3, exist_ok=True)
    for n in ("gbbq.zip", "hsjday.zip"):
        os.link(os.path.join(SNAP_DIR, n), os.path.join(snap_r3, n))
    db3 = os.path.join(RUN_DIR, "db-r3.sqlite")
    fresh_db(db3)
    s3 = Service(db3, "present", snap_dir=snap_r3)
    s3.rpc("initialize")
    a3 = s3.tool("start_collection", catalog_request("r3-c"))
    s3.tool("run_worker_once", {})
    o3, c3 = item_error(db3, a3["task_id"])
    record("R3_refusal", {"outcome": o3, "catalog_counts": c3})
    results["blocked_paths"]["R3_member_missing"] = o3
    check("C10_member_missing",
          o3 is not None and o3.get("success") is False
          and "CATALOG_SOURCE_FAILURE" in (o3.get("error") or "")
          and c3 == (0, 0, 0),
          {"outcome": o3, "catalog_counts": c3})
    s3.close()

    # R4 member corrupt (scratch snapshot dir, base.zip one byte flipped)
    snap_r4 = os.path.join(RUN_DIR, "snap-r4")
    os.makedirs(snap_r4, exist_ok=True)
    for n in ("gbbq.zip", "hsjday.zip"):
        os.link(os.path.join(SNAP_DIR, n), os.path.join(snap_r4, n))
    corrupted = bytearray(open(os.path.join(SNAP_DIR, "base.zip"), "rb")
                          .read())
    corrupted[1000] ^= 0xFF
    with open(os.path.join(snap_r4, "base.zip"), "wb") as f:
        f.write(corrupted)
    record("R4_corrupt", {"flipped_byte_offset": 1000,
                          "corrupt_base_sha256": sha256_bytes(corrupted)})
    db4 = os.path.join(RUN_DIR, "db-r4.sqlite")
    fresh_db(db4)
    s4 = Service(db4, "present", snap_dir=snap_r4)
    s4.rpc("initialize")
    a4 = s4.tool("start_collection", catalog_request("r4-c"))
    s4.tool("run_worker_once", {})
    o4, c4 = item_error(db4, a4["task_id"])
    record("R4_refusal", {"outcome": o4, "catalog_counts": c4})
    results["blocked_paths"]["R4_member_corrupt"] = o4
    check("C11_member_corrupt",
          o4 is not None and o4.get("success") is False
          and "CATALOG_SOURCE_FAILURE" in (o4.get("error") or "")
          and c4 == (0, 0, 0),
          {"outcome": o4, "catalog_counts": c4})
    s4.close()

    # ---- S7 gates ----
    blockers = acc1.get("blocking_reasons")
    results["production_blockers"] = blockers
    check("C12_gates_preserved",
          isinstance(blockers, list) and len(blockers) == 4
          and any("NOT_PRODUCTION_READY" in b or
                  "CATALOG_PUBLISH_NOT_VERIFIED" in b for b in blockers),
          {"blocking_reasons": blockers})
    results["notes"] = {
        "immutable_receipt_provenance": "NOT_APPLICABLE_ON_REJECT_PATH: "
            "P1 reject_candidate produces no catalog version on gen-2; "
            "mechanical diagnostics frozen in PLAN §2.",
        "not_run": "market.daily 5230 collection, deployment, gen-3 "
            "snapshot — out of scope per issue.",
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
