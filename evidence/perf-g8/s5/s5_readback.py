#!/usr/bin/env python3
"""S5 readback (schema-21). WRITE-ONLY kit — not run by its author.

Performs the XP-015 §4-style correctness readback on a private stack:
  * fixed-sample query_data (daily last-3 bars + yearly all years);
  * per-entity get_data_availability row status (both datasets);
  * `task_sqlite check` (schema-21 runs the series deep check:
    closure probes + bar fold recount) on a stopped-service DB copy;
  * SQLite `PRAGMA integrity_check` via a read-only URI.
Writes one JSON to --out.

usage:
  s5_readback.py --sock PATH --db PATH --task-sqlite BIN --out JSON \
      [--sample-list FILE] [--end-date YYYY-MM-DD]
"""
from __future__ import annotations

import argparse
import json
import os
import sqlite3
import subprocess
import sys
import time
from datetime import date, timedelta

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from s5_common import Mcp, guard_paths, utc_now, write_json


def catalog_entities(mcp: Mcp) -> list:
    versions = mcp.call("list_data_versions", {
        "dataset": "securities.catalog", "contract_version": "v1",
        "selection": {"kind": "catalog", "scope": "current_sh_sz_a"}, "page_size": 10})
    cat_ver = versions["items"][0]["version_ref"]["version_id"]
    entities, seen, cursor = [], set(), None
    while True:
        args = {"dataset": "securities.catalog", "contract_version": "v1",
                "selection": {"kind": "catalog", "scope": "current_sh_sz_a"},
                "filter": {"kind": "catalog"},
                "version": {"kind": "catalog_version", "version_id": cat_ver},
                "page_size": 1000}
        if cursor:
            args["cursor"] = cursor
        page = mcp.call("query_data", args)
        for rec in page.get("records") or []:
            e = rec.get("entity") or {}
            k = (e.get("exchange"), e.get("code"))
            if e.get("exchange") and e.get("code") and k not in seen:
                seen.add(k)
                entities.append({"exchange": e["exchange"], "code": e["code"]})
        cursor = page.get("next_cursor")
        if not cursor:
            break
    return entities


def derive_sample(entities: list, absent: list) -> list:
    """Deterministic fixed sample (XP-015 §4.3 shape): catalog first 5,
    last 5, every absent entity, 10 evenly spaced. Recorded in output;
    prefer --sample-list so the list is frozen before the run."""
    sample = entities[:5] + entities[-5:]
    seen = {(e["exchange"], e["code"]) for e in sample}
    for a in absent:
        e = a["entity"]
        k = (e["exchange"], e["code"])
        if k not in seen:
            seen.add(k)
            sample.append(e)
    step = max(1, len(entities) // 10)
    for i in range(0, len(entities), step):
        e = entities[i]
        k = (e["exchange"], e["code"])
        if k not in seen:
            seen.add(k)
            sample.append(e)
        if len(sample) >= 20 + len(absent):
            break
    return sample


def availability_rows(mcp: Mcp, dataset: str, entity: dict, filter_):
    page = mcp.call("get_data_availability", {
        "dataset": dataset, "contract_version": "v1",
        "selection": {"kind": "single", "entity": entity},
        "filter": filter_, "version": {"kind": "latest_per_entity"}})
    items = page.get("items") or []
    return items[0] if items else None


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--sock", required=True)
    ap.add_argument("--db", required=True,
                    help="market DB for task_sqlite check + integrity_check "
                         "(use a stopped-service snapshot, not a live DB)")
    ap.add_argument("--task-sqlite", required=True, help="path to task_sqlite binary")
    ap.add_argument("--out", required=True)
    ap.add_argument("--sample-list", default=None,
                    help="JSON file: frozen [{exchange,code}, ...] (preferred over derived)")
    ap.add_argument("--end-date", default="2026-12-31",
                    help="daily readback window end; pass the gen-B last trade day")
    args = ap.parse_args()

    safe = guard_paths({"sock": args.sock, "db": args.db,
                        "task_sqlite": args.task_sqlite, "out": args.out,
                        **({"sample_list": args.sample_list} if args.sample_list else {})})
    out = {"started_at": utc_now(), "samples": [], "availability": {},
           "task_sqlite_check": None, "integrity_check": None}
    mcp = Mcp(safe["sock"])
    entities = catalog_entities(mcp)
    out["catalog_entities"] = len(entities)

    absent = []
    for e in entities:
        row = availability_rows(mcp, "market.daily", e, {
            "kind": "daily", "start_date": "1990-12-19", "end_date": args.end_date})
        if row and not row.get("version_ref"):
            absent.append({"entity": e, "reason": row.get("reason") or row.get("status")})
    out["absent"] = absent

    if args.sample_list:
        with open(safe["sample_list"], encoding="utf-8") as f:
            sample = json.load(f)
        out["sample_source"] = "frozen_list"
    else:
        sample = derive_sample(entities, absent)
        out["sample_source"] = "derived_first5_last5_absent_even10"
    out["sample_entities"] = sample

    # last bars: a 40-day window ending at --end-date (an unbounded query returns
    # the oldest page first, not the newest bars)
    tail_start = (date.fromisoformat(args.end_date) - timedelta(days=40)).isoformat()
    for e in sample:
        rec = {"entity": e}
        d = mcp.call("query_data", {
            "dataset": "market.daily", "contract_version": "v1",
            "selection": {"kind": "single", "entity": e},
            "filter": {"kind": "daily", "start_date": tail_start,
                       "end_date": args.end_date},
            "version": {"kind": "latest_per_entity"}, "page_size": 50})
        rows = d.get("records") or []
        rec["daily"] = {"record_count_seen": len(rows),
                        "last_bars": rows[-3:],
                        "version_manifest": d.get("version_manifest")}
        y = mcp.call("query_data", {
            "dataset": "market.yearly", "contract_version": "v1",
            "selection": {"kind": "single", "entity": e},
            "filter": {"kind": "yearly", "start_year": 1900, "end_year": 2100},
            "version": {"kind": "latest_per_entity"}, "page_size": 100})
        rec["yearly"] = {"record_count_seen": len(y.get("records") or []),
                         "version_manifest": y.get("version_manifest")}
        rec["availability_daily"] = availability_rows(
            mcp, "market.daily", e,
            {"kind": "daily", "start_date": "1990-12-19", "end_date": args.end_date})
        rec["availability_yearly"] = availability_rows(
            mcp, "market.yearly", e,
            {"kind": "yearly", "start_year": 1900, "end_year": 2100})
        out["samples"].append(rec)

    t0 = time.monotonic()
    chk = subprocess.run([safe["task_sqlite"], "check", safe["db"]],
                         capture_output=True, text=True)
    out["task_sqlite_check"] = {"returncode": chk.returncode,
                                "stdout_tail": chk.stdout[-4000:],
                                "stderr_tail": chk.stderr[-4000:],
                                "wall_s": round(time.monotonic() - t0, 2)}

    t0 = time.monotonic()
    uri = f"file:{safe['db']}?mode=ro&immutable=1"
    con = sqlite3.connect(uri, uri=True)
    try:
        res = con.execute("PRAGMA integrity_check").fetchall()
    finally:
        con.close()
    out["integrity_check"] = {"rows": [r[0] for r in res],
                              "wall_s": round(time.monotonic() - t0, 2)}

    out["finished_at"] = utc_now()
    write_json(safe["out"], out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
