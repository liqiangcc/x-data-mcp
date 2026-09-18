#!/usr/bin/env python3
"""Issue #4 Phase B — market.daily full-catalog first-collection acceptance.

Real mcp_data stdio + official env on the retained gen-2 snapshot:
catalog publish first (5224) -> catalog-selection verbatim rejection leg
(BLOCKED_ON_IMPL) -> 53 sharded explicit start_collection
(daily-000..daily-052, sorted catalog members, <=100 each) ->
run_worker_once loop to idle -> idempotency legs -> readback ->
env-absent refusal leg on an independent scratch DB -> gate verbatim.
Assertions D1-D12 per PLAN.md; mechanical expectations recomputed from
retained bytes, never from the implementation under test.
"""
import datetime
import hashlib
import json
from decimal import Decimal
import os
import sqlite3
import subprocess
import sys
import zipfile

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
HERE = os.path.dirname(os.path.abspath(__file__))
RUN_DIR = "/home/box/acceptance-run-issue4"
SNAP_DIR = "/home/box/acceptance-run-issue3/snapshot"
MANIFEST = os.path.join(REPO, "evidence/issue-3-acceptance-run/snapshot-manifest.json")
# PLAN §8 v2: release build after the #204 equivalence fix; BUILD_PROFILE
# override exists only so the aborted debug run's harness stays reproducible.
PROFILE = os.environ.get("BUILD_PROFILE", "release")
MCP_DATA = os.path.join(REPO, "target", PROFILE, "mcp_data")
TASK_SQLITE = os.path.join(REPO, "target", PROFILE, "task_sqlite")
EXPECTED = {  # PLAN §2 pins
    "base.zip": "dcdf6b49898926675698263535021aed7ad92b34be82a1a9e8a2a8982bd68bc6",
    "gbbq.zip": "daad6290c4f1cd478d7436b1e7077260a3e3e52cefd7eae58e4f24878fa2d0e9",
    "hsjday.zip": "c82f852a7bc2e52c7e89347a1e0d1878b01ce997946c2745f7af323752270892",
    "manifest": "b8a0d7b9aec1a270549d1e53ba26c0ab04842a23c05d1d1f7e75dd44a9f444d3",
}
# PLAN §3 spot-check pins (member sha recomputed below from retained bytes)
SPOT = {
    "sz.000002": {
        "member": "sz\\lday\\sz000002.day", "member_bytes": 271328,
        "member_sha256": "6f168cf053ca561fedde1aa97c3b38d71876854ade3dae5f1a2c9ba9f6b63826",
        "qfq_bars": 8479,
        "first": {"trade_date": "1991-01-29", "open": "-8.83", "high": "-8.83",
                  "low": "-8.83", "close": "-8.83"},
        "last": {"trade_date": "2026-09-14", "open": "3.05", "high": "3.07",
                 "low": "3.03", "close": "3.05"},
    },
    "sh.600006": {
        "member": "sh\\lday\\sh600006.day", "member_bytes": 206944,
        "member_sha256": "77ecaea34c1cc5b08572c4816962b2f8d97d082b545c6034e86e99ecf126655a",
        "qfq_bars": 6467,
        "first": {"trade_date": "1999-07-27", "open": "1.16", "high": "1.17",
                  "low": "0.92", "close": "0.94"},
        "last": {"trade_date": "2026-09-14", "open": "5.19", "high": "5.39",
                 "low": "5.19", "close": "5.36"},
    },
    "sh.600004": {
        "member": "sh\\lday\\sh600004.day", "member_bytes": 180160,
        "member_sha256": "49656073176fea18fca2e8131e7159a0c91952cdbe9994856d2b60f42471b7eb",
        "qfq_bars": 5630,
        "first": {"trade_date": "2003-04-28", "open": "0.96", "high": "1.56",
                  "low": "0.94", "close": "1.55"},
        "last": {"trade_date": "2026-09-14", "open": "7.71", "high": "7.79",
                 "low": "7.70", "close": "7.76"},
    },
}
ANCHOR = {"symbol": "sz.000002", "trade_date": "2000-01-10",
          "open": "-8.14", "high": "-8.06", "low": "-8.15", "close": "-8.06"}
FROZEN = {  # PLAN §3 mechanical expectations
    "hsjday_members": 12411, "standard_day": 11801, "nonstandard": 610,
    "scope": 5230, "published": 5224,
    "nameless": ["0:001235", "0:300728", "1:601206", "1:603302",
                 "1:603361", "1:688688"],
    "with_day": 5219,
    "no_day": ["sh601091", "sh688837", "sz001246", "sz301686", "sz301716"],
}
CATALOG_SOURCE = "official-file:b8a0d7b9aec1a270"

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
    """Independent retained-byte recompute (PLAN §3): catalog scope from
    tdxhy.cfg + infoharbor_ex.code names; .day member set from hsjday.zip
    names; intersection and the no-member residue."""
    base = zipfile.ZipFile(os.path.join(SNAP_DIR, "base.zip"))
    tdxhy = base.read("tdxhy.cfg").decode("ascii")
    names = set()
    for line in base.read("infoharbor_ex.code").decode("gb18030").split("\r\n"):
        if line:
            names.add(line.split("|")[0])
    scope = []  # (market, code) with f0 in {0,1}, f2/f5 nonempty
    for line in tdxhy.split("\r\n"):
        if not line:
            continue
        f = line.split("|")
        assert len(f) == 6, line
        if f[0] in "01" and f[2] and f[5]:
            scope.append((f[0], f[1]))
    nameless = [f"{m}:{c}" for m, c in scope if c not in names]
    published = [(m, c) for m, c in scope if c in names]

    hsj = zipfile.ZipFile(os.path.join(SNAP_DIR, "hsjday.zip"))
    member_names = hsj.namelist()
    day_keys = set()  # e.g. "sh600006"
    standard = 0
    for n in member_names:
        leaf = n.replace("\\", "/").rsplit("/", 1)[-1]
        if len(leaf) == 12 and leaf.endswith(".day") \
                and leaf[:2] in ("sh", "sz") and leaf[2:8].isdigit():
            standard += 1
            day_keys.add(leaf[:-4])
    pub_keys = {("sh" if m == "1" else "sz") + c for m, c in published}
    with_day = sorted(pub_keys & day_keys)
    no_day = sorted(pub_keys - day_keys)

    spot_sha = {}
    spot_len = {}
    for sym, pin in SPOT.items():
        raw = hsj.read(pin["member"])
        spot_sha[sym] = sha256_bytes(raw)
        spot_len[sym] = len(raw)

    out = {
        "scope": len(scope), "nameless": nameless,
        "published": len(published),
        "hsjday_members": len(member_names), "standard_day": standard,
        "nonstandard": len(member_names) - standard,
        "with_day": len(with_day), "no_day": no_day,
        "spot_member_sha256": spot_sha, "spot_member_bytes": spot_len,
    }
    record("mechanical_recompute", out)
    return out


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


def daily_request(request_id, securities):
    return {"request_id": request_id, "dataset": "market.daily",
            "contract_version": "v1",
            "selection": {"kind": "explicit", "securities": securities}}


def catalog_request(request_id):
    return {"request_id": request_id, "dataset": "securities.catalog",
            "contract_version": "v1",
            "selection": {"scope": "current_sh_sz_a"}}


def outcomes(db, task_id):
    con = sqlite3.connect(db)
    rows = con.execute(
        "SELECT item_id,target_json,stage,outcome_json FROM task_items "
        "WHERE task_id=? ORDER BY item_id", (task_id,)).fetchall()
    con.close()
    return [{"item_id": r[0], "target": json.loads(r[1]), "stage": r[2],
             "outcome": json.loads(r[3]) if r[3] else None} for r in rows]


def daily_counts(db):
    con = sqlite3.connect(db)
    out = {
        "data_versions": con.execute(
            "SELECT COUNT(*) FROM data_versions").fetchone()[0],
        "published": con.execute(
            "SELECT COUNT(*) FROM data_versions WHERE state='published'"
        ).fetchone()[0],
        "effective_heads": con.execute(
            "SELECT COUNT(*) FROM effective_heads").fetchone()[0],
        "daily_bars": con.execute(
            "SELECT COUNT(*) FROM daily_bars").fetchone()[0],
    }
    con.close()
    return out


def main():
    for name in ("base.zip", "gbbq.zip", "hsjday.zip"):
        if not os.path.isfile(os.path.join(SNAP_DIR, name)):
            raise SystemExit(f"snapshot member absent: {SNAP_DIR}/{name}")
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
    mech = mechanical_recompute()
    record("env", {"run_dir": RUN_DIR, "head": head, "cargo": cargo,
                   "mcp_data_sha256": sha256_file(MCP_DATA),
                   "task_sqlite_sha256": sha256_file(TASK_SQLITE),
                   "manifest_sha256": manifest_sha, "archive_sha256": arch})

    results = {"assertions": [], "blocked_paths": {},
               "run_identity": {"head": head, "cargo": cargo,
                                "manifest_sha256": manifest_sha,
                                "archive_sha256": arch,
                                "mechanical_recompute": mech}}

    def check(name, ok, detail):
        results["assertions"].append({"name": name, "ok": bool(ok),
                                      "detail": detail})

    check("D1_input_integrity",
          arch == {k: EXPECTED[k] for k in
                   ("base.zip", "gbbq.zip", "hsjday.zip")}
          and manifest_sha == EXPECTED["manifest"],
          {"archive_sha256": arch, "manifest_sha256": manifest_sha})

    # ---- S1 catalog publish first ----
    db = os.path.join(RUN_DIR, "db.sqlite")
    fresh_db(db)
    svc = Service(db, "present")
    svc.rpc("initialize")
    acc_cat = svc.tool("start_collection", catalog_request("catalog-1"))
    record("accepted_catalog", acc_cat)
    svc.tool("run_worker_once", {})
    con = sqlite3.connect(db)
    cat_versions = con.execute(
        "SELECT version_id,source,digest FROM catalog_versions "
        "ORDER BY published_at").fetchall()
    cat_head = con.execute(
        "SELECT version_id FROM catalog_head").fetchone()
    cat_members = con.execute(
        "SELECT exchange,code FROM catalog_members WHERE version_id=? "
        "ORDER BY exchange,code", (cat_versions[-1][0],)).fetchall() \
        if cat_versions else []
    cat_outcome = con.execute(
        "SELECT outcome_json FROM task_items WHERE task_id=?",
        (acc_cat["task_id"],)).fetchone()
    con.close()
    catalog_version = cat_versions[-1][0] if cat_versions else None
    record("catalog_state", {"versions": cat_versions, "head": cat_head,
                             "member_count": len(cat_members),
                             "outcome": json.loads(cat_outcome[0])
                             if cat_outcome and cat_outcome[0] else None})
    check("D2_catalog_prerequisite",
          len(cat_members) == FROZEN["published"]
          and cat_versions and cat_versions[-1][1] == CATALOG_SOURCE
          and cat_head and cat_head[0] == catalog_version,
          {"versions": cat_versions, "head": cat_head,
           "member_count": len(cat_members)})

    # ---- S2 catalog-selection leg (BLOCKED_ON_IMPL verbatim) ----
    cat_sel = svc.tool_raw("start_collection", {
        "request_id": "cat-select", "dataset": "market.daily",
        "contract_version": "v1",
        "selection": {"kind": "catalog", "version_id": catalog_version}})
    record("S2_catalog_selection", cat_sel)
    con = sqlite3.connect(db)
    s2_tasks = con.execute(
        "SELECT COUNT(*) FROM tasks").fetchone()[0]
    con.close()
    s2_err = json.dumps(cat_sel.get("result", {}).get("structuredContent")
                        or cat_sel.get("error") or {}, ensure_ascii=False)
    check("D3_catalog_selection_budget",
          (cat_sel.get("result", {}).get("isError") or cat_sel.get("error"))
          and "resolved selection budget 1..100" in s2_err
          and s2_tasks == 1,  # only the catalog task exists
          {"response": cat_sel, "task_count": s2_tasks})
    results["blocked_paths"]["S2_catalog_selection"] = cat_sel.get(
        "result", {}).get("structuredContent") or cat_sel.get("error")

    # ---- S3 53 sharded explicit collections ----
    securities = [{"exchange": ex, "code": code} for ex, code in cat_members]
    shards = [securities[i:i + 100] for i in range(0, len(securities), 100)]
    accepts = []
    for i, shard in enumerate(shards):
        rid = f"daily-{i:03d}"
        acc = svc.tool("start_collection", daily_request(rid, shard))
        accepts.append(acc)
        record("accepted_shard", {"request_id": rid, "acceptance": acc,
                                  "shard_size": len(shard),
                                  "first": shard[0], "last": shard[-1]})
    total_items = sum(len(a.get("item_ids") or []) for a in accepts)
    check("D4_sharded_acceptance",
          len(accepts) == 53
          and all(len(a.get("item_ids") or []) <= 100 for a in accepts)
          and total_items == FROZEN["published"],
          {"tasks": len(accepts), "total_items": total_items,
           "per_task_items": [len(a.get("item_ids") or []) for a in accepts]})

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

    # per-item outcomes across the 53 tasks
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
    fail_syms = sorted((f["symbol"] or "").replace(".", "")
                       for f in failed_items)
    check("D5_item_outcomes",
          success_items == FROZEN["with_day"]
          and len(failed_items) == 5
          and fail_syms == FROZEN["no_day"]
          and all("member absent or ambiguous" in (f["error"] or "")
                  for f in failed_items),
          {"success": success_items, "failed": failed_items})
    check("D4b_worker_runs", non_idle == 53, {"non_idle": non_idle})

    # ---- S5 idempotency ----
    replay = svc.tool("start_collection",
                      daily_request("daily-000", shards[0]))
    record("replay_daily_000", replay)
    counts_before = daily_counts(db)
    rerun = svc.tool("start_collection",
                     daily_request("daily-053", shards[0]))
    svc.tool("run_worker_once", {})
    rerun_items = outcomes(db, rerun["task_id"])
    rerun_errors = sorted({(i["outcome"] or {}).get("error")
                           for i in rerun_items})
    counts_after = daily_counts(db)
    record("S5_rerun", {"replay": replay, "rerun_accept": rerun,
                        "errors": rerun_errors,
                        "counts_before": counts_before,
                        "counts_after": counts_after})
    check("D9_idempotent_recollection",
          replay.get("reused") is True
          and replay.get("task_id") == accepts[0]["task_id"]
          and rerun.get("task_id") != accepts[0]["task_id"]
          and len(rerun_items) == len(shards[0])
          and all((i["outcome"] or {}).get("success") is False
                  for i in rerun_items)
          and rerun_errors == [
              "INVALID_MARKET_COMMAND: existing daily head requires "
              "the update_data range path"]
          and counts_after == counts_before,
          {"reused": replay.get("reused"), "errors": rerun_errors,
           "counts_before": counts_before, "counts_after": counts_after})

    # ---- S6 readback + version/head/provenance assertions ----
    counts = daily_counts(db)
    con = sqlite3.connect(db)
    heads = con.execute(
        "SELECT symbol,version_id FROM effective_heads").fetchall()
    versions = con.execute(
        "SELECT version_id,symbol,state,record_count,coverage_status,"
        "coverage_reason,first_date,last_date FROM data_versions").fetchall()
    spot_versions = {v[1]: v for v in versions if v[1] in SPOT}
    # provenance: extract snapshot_inputs via SQLite json_extract (full
    # compat_json embeds all bars; do not load it into python)
    prov_rows = con.execute(
        "SELECT version_id, json_extract(compat_json,"
        " '$.batch.provenance.snapshot_inputs') FROM data_versions").fetchall()
    con.close()

    prov_bad = []
    prov_spot = {}
    for vid, inputs_json in prov_rows:
        inputs = json.loads(inputs_json) if inputs_json else None
        ok = isinstance(inputs, list) and len(inputs) == 2
        if ok:
            by_role = {i.get("role"): i for i in inputs}
            ok = (by_role.get("day", {}).get("archive_sha256")
                  == EXPECTED["hsjday.zip"]
                  and by_role.get("gbbq", {}).get("archive_sha256")
                  == EXPECTED["gbbq.zip"]
                  and by_role.get("day", {}).get("member_sha256")
                  and by_role.get("gbbq", {}).get("member_sha256"))
            sym = next((v[1] for v in versions if v[0] == vid), None)
            if sym in SPOT:
                prov_spot[sym] = by_role
        if not ok:
            prov_bad.append(vid)
    spot_member_ok = all(
        prov_spot.get(sym, {}).get("day", {}).get("member_sha256")
        == SPOT[sym]["member_sha256"]
        and prov_spot.get(sym, {}).get("day", {}).get("member")
        == SPOT[sym]["member"]
        for sym in SPOT)
    check("D6_versions_and_heads",
          counts["data_versions"] == FROZEN["with_day"]
          and counts["published"] == FROZEN["with_day"]
          and counts["effective_heads"] == FROZEN["with_day"]
          and len(heads) == FROZEN["with_day"]
          and len({h[0] for h in heads}) == FROZEN["with_day"]
          and len(versions) == FROZEN["with_day"],
          {"counts": counts, "distinct_head_symbols": len({h[0] for h in heads})})
    check("D7_provenance_binding",
          not prov_bad and len(prov_rows) == FROZEN["with_day"]
          and spot_member_ok,
          {"versions_checked": len(prov_rows), "bad": prov_bad[:10],
           "spot_day_member_sha256": {
               s: prov_spot.get(s, {}).get("day", {}).get("member_sha256")
               for s in SPOT}})

    # spot-check bar content
    con = sqlite3.connect(db)
    spot_detail = {}
    spot_ok = True
    for sym, pin in SPOT.items():
        vid = spot_versions[sym][0] if sym in spot_versions else None
        bars = con.execute(
            "SELECT trade_date,open,high,low,close,volume,amount "
            "FROM daily_bars WHERE version_id=? ORDER BY trade_date",
            (vid,)).fetchall() if vid else []
        first = dict(zip(("trade_date", "open", "high", "low", "close",
                          "volume", "amount"), bars[0])) if bars else None
        last = dict(zip(("trade_date", "open", "high", "low", "close",
                         "volume", "amount"), bars[-1])) if bars else None

        # Projection TEXT columns store decimal-normalized text ("7.7" not
        # "7.70"); the pin freezes record content, so compare numerically.
        # compat_json retains the pinned textual form verbatim.
        def pin_eq(row, pinrow):
            return row["trade_date"] == pinrow["trade_date"] and all(
                Decimal(row[k]) == Decimal(pinrow[k])
                for k in ("open", "high", "low", "close"))

        ok = len(bars) == pin["qfq_bars"] \
            and pin_eq(first, pin["first"]) \
            and pin_eq(last, pin["last"]) \
            and first["volume"] is None and first["amount"] is None
        if sym == ANCHOR["symbol"]:
            anchor = [b for b in bars if b[0] == ANCHOR["trade_date"]]
            ok = ok and len(anchor) == 1 and all(
                Decimal(anchor[0][i]) == Decimal(ANCHOR[k])
                for i, k in ((1, "open"), (2, "high"), (3, "low"),
                             (4, "close")))
            spot_detail["anchor_bar"] = anchor
        spot_ok = spot_ok and ok
        spot_detail[sym] = {"version_id": vid, "bar_count": len(bars),
                            "first": first, "last": last, "ok": ok}
    con.close()
    check("D8_spot_check_content", spot_ok, spot_detail)

    # query/availability/versions readback for spot symbols
    readback = {}
    query_ok = True
    for sym, pin in SPOT.items():
        ex, code = sym.split(".")
        rows, cursor, pages = [], None, 0
        while True:
            page = svc.tool("query_data", {
                "dataset": "market.daily", "contract_version": "v1",
                "selection": {"kind": "single",
                              "entity": {"exchange": ex, "code": code}},
                "filter": {"kind": "daily", "start_date": "1900-01-01",
                           "end_date": "2100-12-31"},
                "version": {"kind": "latest_per_entity"},
                "page_size": 1000, "cursor": cursor})
            pages += 1
            rows.extend(page.get("records", []))
            cursor = page.get("next_cursor")
            if not cursor or pages > 20:
                break
        avail = svc.tool("get_data_availability", {
            "dataset": "market.daily", "contract_version": "v1",
            "selection": {"kind": "single",
                          "entity": {"exchange": ex, "code": code}},
            "filter": {"kind": "daily", "start_date": "1900-01-01",
                       "end_date": "2100-12-31"},
            "version": {"kind": "latest_per_entity"},
            "page_size": 10, "cursor": None})
        vers = svc.tool("list_data_versions", {
            "dataset": "market.daily", "contract_version": "v1",
            "selection": {"kind": "single",
                          "entity": {"exchange": ex, "code": code}},
            "page_size": 1000, "cursor": None})
        readback[sym] = {"query_rows": len(rows), "pages": pages,
                         "availability": avail, "versions": vers}
        record("readback_spot", {"symbol": sym, "rows": len(rows),
                                 "pages": pages, "availability": avail,
                                 "versions": vers})
        query_ok = query_ok and len(rows) == pin["qfq_bars"]
    check("D12_readback", query_ok,
          {s: {"rows": r["query_rows"],
               "expected": SPOT[s]["qfq_bars"],
               "availability": (r["availability"].get("items") or [{}])[0]
               .get("availability")}
           for s, r in readback.items()})

    # ---- S7 env-absent refusal leg (independent db-r1) ----
    db1 = os.path.join(RUN_DIR, "db-r1.sqlite")
    fresh_db(db1)
    s1 = Service(db1, "absent")
    s1.rpc("initialize")
    a1 = s1.tool("start_collection", daily_request(
        "r1-daily", [{"exchange": "sz", "code": "000002"}]))
    record("R1_accept", a1)
    s1.tool("run_worker_once", {})
    r1_items = outcomes(db1, a1["task_id"])
    r1_counts = daily_counts(db1)
    record("R1_refusal", {"items": r1_items, "counts": r1_counts})
    results["blocked_paths"]["R1_env_absent"] = [
        i["outcome"] for i in r1_items]
    check("D10_env_absent",
          len(r1_items) == 1
          and (r1_items[0]["outcome"] or {}).get("success") is False
          and "SOURCE_NOT_VERIFIED" in
          ((r1_items[0]["outcome"] or {}).get("error") or "")
          and r1_counts["data_versions"] == 0
          and r1_counts["effective_heads"] == 0,
          {"items": r1_items, "counts": r1_counts})
    s1.close()

    # ---- S8 gates verbatim ----
    blockers = accepts[0].get("blocking_reasons") if accepts else None
    all_same = all(a.get("blocking_reasons") == blockers for a in accepts)
    caps = svc.tool("get_data_capabilities", {"dataset": "market.daily"})
    record("S8_gates", {"blocking_reasons": blockers,
                        "all_accepts_identical": all_same,
                        "capabilities_production_ready":
                            caps.get("production_ready")})
    check("D11_gates_verbatim",
          blockers is not None and len(blockers) == 3
          and all_same
          and any("GBBQ_QFQ_NOT_VERIFIED" in b for b in blockers)
          and any("FILE_ACCESS_USE_NOT_VERIFIED" in b for b in blockers)
          and any("FILE_SET_CONSISTENCY_NOT_VERIFIED" in b for b in blockers)
          and caps.get("production_ready") is False,
          {"blocking_reasons": blockers,
           "production_ready": caps.get("production_ready")})
    svc.close()

    results["summary"] = {
        "passed": sum(1 for a in results["assertions"] if a["ok"]),
        "total": len(results["assertions"]),
        "failed": [a["name"] for a in results["assertions"] if not a["ok"]],
        "catalog_version": catalog_version,
        "daily_tasks": [a["task_id"] for a in accepts],
        "counts": counts,
        "non_idle_worker_runs": non_idle,
    }
    with open(os.path.join(HERE, "results.json"), "w") as f:
        json.dump(results, f, ensure_ascii=False, indent=1)
    print(json.dumps(results["summary"], ensure_ascii=False, indent=1))
    sys.exit(0 if not results["summary"]["failed"] else 1)


if __name__ == "__main__":
    main()
