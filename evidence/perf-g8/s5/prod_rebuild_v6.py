#!/usr/bin/env python3
"""prod_rebuild_v6.py — production rebuild driver for the post-switch empty
schema-21 database (generic-data-v6). Operations tooling, not product code.

Purpose: deploy_phase_g.sh leaves production on an EMPTY schema-21 DB; the
daily-sync Skill forbids rebuilding an empty DB implicitly. This driver
performs the explicit, user-approved rebuild: read-only preflight ->
collection (catalog + market.daily) -> market.yearly first pass -> read-only
readback against fixed expectations.

Safety rules (mirroring prod_guard.py / TASK-DEVIN-DEPLOY-1 §R):
  * The production socket is usable ONLY when PROD_REBUILD_V6_PRODUCTION=1
    AND PROD_REBUILD_V6_SOCKET resolves exactly (realpath, no `..`) to
    /opt/x-data-mcp-preview/run/mcp.sock. Any other socket is rehearsal mode.
  * Read-only preflight runs BEFORE any write: capability must be
    generic-data-v6 with 21 data tools, and the target must be provably
    empty (no catalog version, no daily head on probe entities). A non-empty
    target exits before the first write call.
  * Tool whitelist: only start_collection/update_data writes plus the read
    tools below; every call passes through GuardedMcp. acquire_* /
    activate_* / check_official_updates / republish_data / backfill_data /
    cancel_task / run_worker_once / verification tools are refused.
  * Each phase writes its raw JSON into PROD_REBUILD_V6_OUT (required env;
    may not resolve under /tmp). Any write failure aborts the run; no write
    is retried automatically.
  * Readback compares against the preview.31 expectations for generation
    87322997 (data through 2026-10-08); any diff is reported and the driver
    exits non-zero without changing expectations.

usage:
  PROD_REBUILD_V6_SOCKET=<sock> PROD_REBUILD_V6_DB=<db> \
  PROD_REBUILD_V6_OUT=<dir> [PROD_REBUILD_V6_PRODUCTION=1] \
  [PROD_REBUILD_V6_PREFIX=v6rebuild] prod_rebuild_v6.py
"""
from __future__ import annotations

import json
import os
import signal
import sqlite3
import sys
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from s5_common import Mcp, utc_now, write_json  # noqa: E402
import s5_run  # noqa: E402  (phase_collect / phase_yearly / catalog_entities reuse)

PROD_SOCKET = "/opt/x-data-mcp-preview/run/mcp.sock"
PROD_DB = "/opt/x-data-mcp-preview/state/preview.sqlite"

# Whitelists — the readback asserts these sets in tests.
ALLOWED_WRITE_TOOLS = frozenset({"start_collection", "update_data"})
ALLOWED_READ_TOOLS = frozenset({
    "get_data_capabilities",
    "get_task_progress",
    "query_data",
    "list_data_versions",
    "get_data_availability",
    "get_yearly_base_years",
    "list_tasks",
})
# Never called by this driver under any mode: acquisition-side tools and any
# mutating tool outside the collection/update path.
FORBIDDEN_TOOLS = frozenset({
    "check_official_updates",
    "acquire_generation",
    "verify_generation",
    "activate_generation",
    "retire_generation",
    "get_acquisition_task",
    "sync_status",
    "republish_data",
    "backfill_data",
    "cancel_task",
    "run_worker_once",
    "inspect_verification_task",
    "verify_publication_result",
    "verify_stopped_writes",
    "verify_data_gap",
})

# Emptiness probes: well-known entities that must all be absent on a fresh DB.
PROBE_ENTITIES = [
    {"exchange": "sh", "code": "600000"},
    {"exchange": "sz", "code": "000001"},
    {"exchange": "sh", "code": "601318"},
    {"exchange": "sz", "code": "300750"},
    {"exchange": "sh", "code": "688981"},
]

# Readback expectations — preview.31 readback, generation 87322997
# (official snapshot through 2026-10-08). Compared verbatim; a mismatch is
# reported and stops the run — expectations are never edited in place.
EXPECTED = {
    "catalog_entities": 5228,
    "daily_published": 5224,
    "daily_absent": ["sz.001381", "sz.301569", "sz.301660", "sz.301718"],
    "yearly_published": 5224,
    "user_version": 21,
    "staged_records": 0,
}
SAMPLE_ENTITIES = [
    {"exchange": "sh", "code": "600000"},
    {"exchange": "sz", "code": "000001"},
    {"exchange": "sz", "code": "001246"},
    {"exchange": "sh", "code": "688496"},
    {"exchange": "sz", "code": "301718"},  # expected absent
]


def resolve_env() -> dict:
    """Env-gated target resolution. Returns {sock, db, out, mode, prefix}."""
    sock = os.environ.get("PROD_REBUILD_V6_SOCKET")
    if not sock:
        sys.exit("PROD_REBUILD_V6_SOCKET is required")
    if ".." in sock.split("/"):
        sys.exit(f"refusing socket path with '..': {sock!r}")
    real_sock = os.path.realpath(sock)
    mode = "rehearsal"
    if real_sock == PROD_SOCKET:
        if os.environ.get("PROD_REBUILD_V6_PRODUCTION") != "1":
            sys.exit(
                "refusing production socket without PROD_REBUILD_V6_PRODUCTION=1"
            )
        mode = "production"
    db = os.environ.get("PROD_REBUILD_V6_DB")
    if not db:
        sys.exit("PROD_REBUILD_V6_DB is required")
    if ".." in db.split("/"):
        sys.exit(f"refusing db path with '..': {db!r}")
    real_db = os.path.realpath(db)
    if mode == "production" and real_db != PROD_DB:
        sys.exit(
            f"production mode requires PROD_REBUILD_V6_DB == {PROD_DB}, got {real_db!r}"
        )
    out = os.environ.get("PROD_REBUILD_V6_OUT")
    if not out:
        sys.exit("PROD_REBUILD_V6_OUT is required")
    real_out = os.path.realpath(out)
    if real_out == "/tmp" or real_out.startswith("/tmp/") or real_out.startswith("/dev/shm"):
        sys.exit(f"PROD_REBUILD_V6_OUT may not resolve under /tmp or /dev/shm: {real_out!r}")
    os.makedirs(real_out, exist_ok=True)
    return {
        "sock": real_sock,
        "db": real_db,
        "out": real_out,
        "mode": mode,
        "prefix": os.environ.get("PROD_REBUILD_V6_PREFIX", "v6rebuild"),
    }


class GuardedMcp:
    """Tool-call whitelist wrapper around s5_common.Mcp."""

    def __init__(self, inner: Mcp):
        self._inner = inner

    def call(self, name: str, arguments: dict):
        if name in FORBIDDEN_TOOLS or name not in (ALLOWED_READ_TOOLS | ALLOWED_WRITE_TOOLS):
            raise RuntimeError(f"tool {name!r} is not whitelisted for the rebuild driver")
        return self._inner.call(name, arguments)


def preflight(mcp: GuardedMcp) -> dict:
    """Read-only checks that must ALL pass before the first write."""
    caps = mcp.call("get_data_capabilities", {})
    vers = mcp.call("list_data_versions", {
        "dataset": "securities.catalog", "contract_version": "v1",
        "selection": {"kind": "catalog", "scope": "current_sh_sz_a"},
        "page_size": 1000})
    avail = mcp.call("get_data_availability", {
        "dataset": "market.daily", "contract_version": "v1",
        "selection": {"kind": "batch", "entities": PROBE_ENTITIES},
        "filter": {"kind": "daily", "start_date": "1990-01-01", "end_date": "2026-12-31"},
        "version": {"kind": "latest_per_entity"}, "page_size": 100})
    tool_names = {i.get("tool_name") for i in caps.get("items") or []}
    checks = {
        "capability_revision_v6": caps.get("capability_revision") == "generic-data-v6",
        # items are one Capability per tool x dataset — distinct tool names are
        # the data-tools count (21 on the data socket).
        "data_tools_21": len(tool_names) == 21,
        "production_ready_false": caps.get("production_ready") is False,
        "catalog_versions_empty": not (vers.get("items") or []),
        "daily_heads_empty": all(
            not it.get("version_ref") for it in avail.get("items") or []),
    }
    return {
        "checks": checks,
        "ok": all(checks.values()),
        "capability": {k: caps.get(k) for k in
                       ("capability_revision", "implementation", "production_ready")},
        "probe_items": len(avail.get("items") or []),
    }


def readback(mcp: GuardedMcp, db: str) -> dict:
    """Read-only verification vs EXPECTED. Returns {checks, samples, diffs}."""
    out = {"db": db, "samples": []}
    _, entities = s5_run.catalog_entities(mcp)
    # Daily + yearly coverage across the whole catalog, chunked like the
    # yearly phase does.
    daily_pub, yearly_pub = set(), set()
    daily_absent, yearly_absent = [], []
    for chunk in s5_run.chunked(entities, 100):
        d = mcp.call("get_data_availability", {
            "dataset": "market.daily", "contract_version": "v1",
            "selection": {"kind": "batch", "entities": chunk},
            "filter": {"kind": "daily", "start_date": "1990-01-01", "end_date": "2026-12-31"},
            "version": {"kind": "latest_per_entity"}, "page_size": 100})
        for it in d.get("items") or []:
            e = it.get("entity") or {}
            key = f"{e.get('exchange')}.{e.get('code')}"
            if it.get("version_ref") and it.get("last_record"):
                daily_pub.add(key)
            else:
                daily_absent.append(key)
        y = mcp.call("get_data_availability", {
            "dataset": "market.yearly", "contract_version": "v1",
            "selection": {"kind": "batch", "entities": chunk},
            "filter": {"kind": "yearly", "start_year": 1900, "end_year": 2026},
            "version": {"kind": "latest_per_entity"}, "page_size": 100})
        for it in y.get("items") or []:
            e = it.get("entity") or {}
            key = f"{e.get('exchange')}.{e.get('code')}"
            if it.get("version_ref"):
                yearly_pub.add(key)
            else:
                yearly_absent.append(key)
    for e in SAMPLE_ENTITIES:
        a = mcp.call("get_data_availability", {
            "dataset": "market.daily", "contract_version": "v1",
            "selection": {"kind": "single", "entity": e},
            "filter": {"kind": "daily", "start_date": "1990-01-01", "end_date": "2026-12-31"},
            "version": {"kind": "latest_per_entity"}, "page_size": 10})
        items = a.get("items") or []
        it = items[0] if items else {}
        out["samples"].append({
            "entity": f"{e['exchange']}.{e['code']}",
            "status": it.get("status"), "reason": it.get("reason"),
            "version_id": (it.get("version_ref") or {}).get("version_id"),
            "last_record": it.get("last_record")})
    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True, timeout=10)
    try:
        out["user_version"] = con.execute("PRAGMA user_version").fetchone()[0]
        out["staged_records"] = con.execute("SELECT count(*) FROM staged_records").fetchone()[0]
        out["first_publish_events"] = con.execute(
            "SELECT count(*) FROM market_events WHERE kind='first_publish'").fetchone()[0]
    finally:
        con.close()
    checks = {
        "catalog_entities": len(entities),
        "daily_published": len(daily_pub),
        "daily_absent": sorted(daily_absent),
        "yearly_published": len(yearly_pub),
        "yearly_absent_count": len(yearly_absent),
        "user_version": out["user_version"],
        "staged_records": out["staged_records"],
        "first_publish_events": out["first_publish_events"],
    }
    diffs = []
    if checks["catalog_entities"] != EXPECTED["catalog_entities"]:
        diffs.append(f"catalog_entities {checks['catalog_entities']} != {EXPECTED['catalog_entities']}")
    if checks["daily_published"] != EXPECTED["daily_published"]:
        diffs.append(f"daily_published {checks['daily_published']} != {EXPECTED['daily_published']}")
    if checks["daily_absent"] != EXPECTED["daily_absent"]:
        diffs.append(f"daily_absent {checks['daily_absent']} != {EXPECTED['daily_absent']}")
    if checks["yearly_published"] != EXPECTED["yearly_published"]:
        diffs.append(f"yearly_published {checks['yearly_published']} != {EXPECTED['yearly_published']}")
    if checks["user_version"] != EXPECTED["user_version"]:
        diffs.append(f"user_version {checks['user_version']} != {EXPECTED['user_version']}")
    if checks["staged_records"] != EXPECTED["staged_records"]:
        diffs.append(f"staged_records {checks['staged_records']} != {EXPECTED['staged_records']}")
    want_fp = EXPECTED["daily_published"] + EXPECTED["yearly_published"]
    if checks["first_publish_events"] != want_fp:
        diffs.append(f"first_publish_events {checks['first_publish_events']} != {want_fp} (daily+yearly)")
    out["checks"] = checks
    out["diffs"] = diffs
    return out


def main() -> int:
    signal.signal(signal.SIGINT, signal.default_int_handler)
    env = resolve_env()
    run = {"started_at": utc_now(), "mode": env["mode"], "socket": env["sock"],
           "db": env["db"], "out": env["out"], "phases": {}}
    out_dir = Path(env["out"])
    mcp = GuardedMcp(Mcp(env["sock"]))
    try:
        pre = preflight(mcp)
        run["phases"]["preflight"] = pre
        write_json(str(out_dir / "rebuild-preflight.json"), pre)
        if not pre["ok"]:
            run["error"] = f"preflight failed: {[k for k, v in pre['checks'].items() if not v]}"
            write_json(str(out_dir / "rebuild-run.json"), run)
            print(json.dumps(run["error"], ensure_ascii=False), file=sys.stderr)
            return 2
        res = s5_run.phase_collect(mcp, env["prefix"], 0)
        run["phases"]["collect"] = res
        write_json(str(out_dir / "rebuild-collect.json"), res)
        if res.get("aborted_at") or res.get("stopped"):
            raise RuntimeError(f"collect aborted: {res.get('aborted_at') or res['stopped']}")
        _, entities = s5_run.catalog_entities(mcp)
        res = s5_run.phase_yearly(mcp, f"{env['prefix']}-y1", "yearly_first", entities, 0)
        run["phases"]["yearly_first"] = res
        write_json(str(out_dir / "rebuild-yearly-first.json"), res)
        if res.get("stopped"):
            raise RuntimeError(f"yearly_first aborted: {res['stopped']}")
        rb = readback(mcp, env["db"])
        run["phases"]["readback"] = rb
        write_json(str(out_dir / "rebuild-readback.json"), rb)
        if rb["diffs"]:
            run["error"] = "readback diffs: " + "; ".join(rb["diffs"])
            write_json(str(out_dir / "rebuild-run.json"), run)
            print(run["error"], file=sys.stderr)
            return 3
    except KeyboardInterrupt:
        run["error"] = "interrupted"
        write_json(str(out_dir / "rebuild-run.json"), run)
        raise
    except Exception as exc:
        run["error"] = str(exc)
        write_json(str(out_dir / "rebuild-run.json"), run)
        raise
    run["finished_at"] = utc_now()
    write_json(str(out_dir / "rebuild-run.json"), run)
    print(f"REBUILD OK mode={env['mode']} catalog={EXPECTED['catalog_entities']} "
          f"daily={EXPECTED['daily_published']} yearly={EXPECTED['yearly_published']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
