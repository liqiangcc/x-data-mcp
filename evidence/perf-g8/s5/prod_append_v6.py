#!/usr/bin/env python3
"""prod_append_v6.py — production "append to latest trade day" driver for the
v6/schema-21 deployment (generic-data-v6). Operations tooling, not product
code. Same guard semantics as prod_rebuild_v6.py.

Purpose: the deployed DB already holds every catalog entity's daily head
(data through the previous trade day). After the acquire side activates a
newer generation, this driver appends market.daily to the generation's
latest trade day via `update_data` explicit-range items and re-derives the
yearly rows left behind — instead of an AI session paging 5,228 entities.

Safety rules (TASK-DEVIN-APPEND-DRIVER-V6):
  * Production only when PROD_APPEND_V6_PRODUCTION=1 AND the socket resolves
    exactly to /opt/x-data-mcp-preview/run/mcp.sock; any other socket is a
    rehearsal target. PROD_APPEND_V6_DB must equal the canonical db path in
    production mode. PROD_APPEND_V6_OUT may not resolve under /tmp.
  * Read-only preflight before ANY write: capability generic-data-v6, 21
    distinct data tools, production_ready false, an active (verified)
    generation ref, every catalog entity has a daily head except the
    allowed-absent set, and no in-flight task blocks the write path.
  * Tool whitelist: write = update_data ONLY; reads as listed below; every
    call passes through GuardedMcp. acquire_*/activate_*/
    check_official_updates/start_collection/republish_data/backfill_data/
    cancel_task/run_worker_once are refused outright.
  * Idempotent: entities already at END_DATE are never resubmitted; a rerun
    processes only entities still lagging. Write failure stops the run —
    no blind retry; on submit timeout/request-binding loss the SAME
    request_id is recovered via get_request (SKILL §5) and only an
    unbound request is reported for an operator decision.
  * stale-base items (concurrent head advance) get ONE bounded retry pass
    with freshly re-read heads; a second stale failure stops the run.
  * Readback expectations come from env/args, never hardcoded:
    PROD_APPEND_V6_EXPECT_* (see EXPECT_KEYS below).

usage:
  PROD_APPEND_V6_SOCKET=<sock> PROD_APPEND_V6_DB=<db> PROD_APPEND_V6_OUT=<dir> \
  PROD_APPEND_V6_END_DATE=YYYY-MM-DD [PROD_APPEND_V6_PRODUCTION=1] \
  [PROD_APPEND_V6_PREFIX=v6append] [PROD_APPEND_V6_EXPECT_* =...] \
  prod_append_v6.py
"""
from __future__ import annotations

import json
import os
import signal
import sqlite3
import sys
import time
from datetime import date, timedelta
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from s5_common import (  # noqa: E402
    Mcp, chunked, utc_now, wait_task, write_json, merge_outcome_totals,
    sum_progress, Phase,
)
import s5_run  # noqa: E402  (daily_heads / catalog_entities / availability reuse; also puts ../drivers on sys.path)
from s5_run import resolve_years  # noqa: E402
from xp015_common import classify_range_results  # noqa: E402

PROD_SOCKET = "/opt/x-data-mcp-preview/run/mcp.sock"
PROD_DB = "/opt/x-data-mcp-preview/state/preview.sqlite"

ALLOWED_WRITE_TOOLS = frozenset({"update_data"})
ALLOWED_READ_TOOLS = frozenset({
    "get_data_capabilities",
    "get_task_progress",
    "get_data_availability",
    "get_yearly_base_years",
    "list_data_versions",
    "list_tasks",
    "get_request",
    "query_data",
    "verify_publication_result",
})
FORBIDDEN_TOOLS = frozenset({
    "check_official_updates",
    "acquire_generation",
    "verify_generation",
    "activate_generation",
    "retire_generation",
    "get_acquisition_task",
    "get_acquisition_request",
    "sync_status",
    "start_collection",
    "republish_data",
    "backfill_data",
    "cancel_task",
    "run_worker_once",
    "inspect_verification_task",
    "verify_stopped_writes",
    "verify_data_gap",
})

# Statuses that mean the write path is occupied or halted pending inspection.
INFLIGHT_TASK_STATUSES = frozenset(
    {"pending", "running", "cancel_requested", "needs_verification"})

# Yearly availability items that need re-derivation: trailing the daily base
# or absent for the catalog entity.
YEARLY_REDERIVE_STALE = "behind"
YEARLY_INVALID_REASONS = frozenset(
    {"daily_replaced_since_derivation", "value_semantics_changed"})

# Expected fail-closed class (SKILL §fail-closed): entities whose source
# snapshot carries no bars in the accepted window — suspended / delisted /
# no-data tails. Expected on every full-market append; never retried, never
# blocking, recorded as the fail-closed set.
FAIL_CLOSED_NO_BARS = "no bars inside the accepted range"

# Env-provided expectations (unset = record-only, no diff for that key).
EXPECT_KEYS = (
    "PROD_APPEND_V6_EXPECT_DAILY_PUBLISHED",
    "PROD_APPEND_V6_EXPECT_DAILY_ABSENT",      # comma-separated e.g. "sz.a,sz.b"
    "PROD_APPEND_V6_EXPECT_GENERATION",
    "PROD_APPEND_V6_EXPECT_MAX_APPEND_EVENTS",
    "PROD_APPEND_V6_EXPECT_STILL_LAGGING",     # comma-separated; entities allowed to trail
    "PROD_APPEND_V6_EXPECT_STAGED_RECORDS",    # int; unset -> require 0
    "PROD_APPEND_V6_EXPECT_CAPABILITY",        # default generic-data-v6; v7 after switch
    "PROD_APPEND_V6_EXPECT_DATA_TOOLS",        # default 21; 23 after switch
)


def resolve_env() -> dict:
    sock = os.environ.get("PROD_APPEND_V6_SOCKET")
    if not sock:
        sys.exit("PROD_APPEND_V6_SOCKET is required")
    if ".." in sock.split("/"):
        sys.exit(f"refusing socket path with '..': {sock!r}")
    real_sock = os.path.realpath(sock)
    mode = "rehearsal"
    if real_sock == PROD_SOCKET:
        if os.environ.get("PROD_APPEND_V6_PRODUCTION") != "1":
            sys.exit("refusing production socket without PROD_APPEND_V6_PRODUCTION=1")
        mode = "production"
    db = os.environ.get("PROD_APPEND_V6_DB")
    if not db:
        sys.exit("PROD_APPEND_V6_DB is required")
    if ".." in db.split("/"):
        sys.exit(f"refusing db path with '..': {db!r}")
    real_db = os.path.realpath(db)
    if mode == "production" and real_db != PROD_DB:
        sys.exit(f"production mode requires PROD_APPEND_V6_DB == {PROD_DB}, got {real_db!r}")
    out = os.environ.get("PROD_APPEND_V6_OUT")
    if not out:
        sys.exit("PROD_APPEND_V6_OUT is required")
    real_out = os.path.realpath(out)
    if real_out == "/tmp" or real_out.startswith("/tmp/") or real_out.startswith("/dev/shm"):
        sys.exit(f"PROD_APPEND_V6_OUT may not resolve under /tmp or /dev/shm: {real_out!r}")
    os.makedirs(real_out, exist_ok=True)
    end = os.environ.get("PROD_APPEND_V6_END_DATE")
    if not end:
        sys.exit("PROD_APPEND_V6_END_DATE (active generation's latest trade day) is required")
    try:
        end_date = date.fromisoformat(end)
    except ValueError:
        sys.exit(f"PROD_APPEND_V6_END_DATE not a date: {end!r}")
    expect = {k: os.environ.get(k) for k in EXPECT_KEYS}
    return {"sock": real_sock, "db": real_db, "out": real_out, "mode": mode,
            "prefix": os.environ.get("PROD_APPEND_V6_PREFIX", "v6append"),
            "end_date": end_date, "expect": expect}


class GuardedMcp:
    """Tool-call whitelist wrapper around s5_common.Mcp."""

    def __init__(self, inner: Mcp):
        self._inner = inner

    def call(self, name: str, arguments: dict):
        if name in FORBIDDEN_TOOLS or name not in (ALLOWED_READ_TOOLS | ALLOWED_WRITE_TOOLS):
            raise RuntimeError(f"tool {name!r} is not whitelisted for the append driver")
        return self._inner.call(name, arguments)


def expected_absent(expect: dict) -> set:
    v = expect.get("PROD_APPEND_V6_EXPECT_DAILY_ABSENT")
    return {s.strip() for s in v.split(",") if s.strip()} if v else set()


def submit_and_wait(mcp: GuardedMcp, request_id: str, items: list) -> dict:
    """One update_data call with SKILL §5 recovery: on submit timeout /
    binding loss, recover the SAME request_id via get_request and adopt the
    bound task; an unbound request is reported, never resent."""
    try:
        acc = mcp.call("update_data", {
            "dataset": "market.daily", "contract_version": "v1",
            "request_id": request_id, "items": items, "selection": None})
        task_id = acc["task_id"]
    except Exception as exc:
        # Binding ambiguous: recover, never re-send under a new id here.
        view = mcp.call("get_request", {"contract_version": "v1", "request_id": request_id})
        if view.get("bound") and view.get("task"):
            task_id = view["task"]["task_id"]
        elif view.get("terminal_result") is not None:
            return {"request_id": request_id, "recovered_terminal": view["terminal_result"],
                    "submit_error": str(exc)[:300]}
        else:
            raise RuntimeError(
                f"{request_id} submit failed ({exc}) and get_request shows no binding; "
                f"stopping for operator decision (never resent)") from exc
    prog, meta = wait_task(mcp, task_id)
    return {"request_id": request_id, "task_id": task_id,
            "progress": prog, "meta": meta}


def build_append_items(heads: dict, end_date: date) -> list:
    """explicit-range-task-v1 items for entities still behind end_date.
    Idempotent: entities already at/past end_date produce nothing."""
    items = []
    for key, h in heads.items():
        last = date.fromisoformat(h["last_record"])
        if last >= end_date:
            continue
        items.append({"entity": h["entity"],
                      "base_version_id": h["version_ref"]["version_id"],
                      "start_date": (last + timedelta(days=1)).isoformat(),
                      "end_date": end_date.isoformat()})
    return items


def phase_append(mcp: GuardedMcp, prefix: str, entities, end_date: date) -> dict:
    """Serial <=100-item update_data chunks over the lagging set, then ONE
    bounded retry pass for entities still behind (fresh heads)."""
    ph = Phase("daily_append")
    out = {"end_date": end_date.isoformat(), "chunks": [], "retry_chunks": []}
    heads, absent = s5_run.daily_heads(mcp, entities, end_date)
    out["preflight_heads"] = len(heads)
    out["absent"] = absent
    items = build_append_items(heads, end_date)
    out["lagging_entities"] = len(items)
    out["submitted_keys"] = sorted(
        f"{i['entity']['exchange']}.{i['entity']['code']}" for i in items)
    ph.start()
    fail_closed = {}   # entity key -> error text (expected class only)
    hard_fail = {}     # entity key -> error text (any other item failure)
    for idx, chunk in enumerate(chunked(items, 100), 1):
        rid = f"{prefix}-append-{idx:03d}"
        rec = submit_and_wait(mcp, rid, chunk)
        rec["items"] = len(chunk)
        rec["outcomes"] = classify_range_results(rec.get("progress") or {})
        for u in (rec.get("progress") or {}).get("unfinished") or []:
            e = u.get("entity") or {}
            key = f"{e.get('exchange')}.{e.get('code')}"
            err = u.get("error") or ""
            if FAIL_CLOSED_NO_BARS in err:
                fail_closed[key] = err
            else:
                hard_fail[key] = err
        ph.record_task(rec)
        out["chunks"].append(rec)
        st = (rec.get("progress", {}).get("task") or {}).get("status")
        # cancelled/interrupted/needs_verification are infrastructure-level
        # stops; a `failed` task whose item failures are ALL the expected
        # no-bars class is recorded, not blocking.
        if st in {"cancelled", "interrupted", "needs_verification"} or \
                (st == "failed" and hard_fail):
            out["stopped"] = {"reason": "task_terminal", "request_id": rid,
                              "status": st, "hard_fail": hard_fail}
            out.update(ph.finish())
            out["outcome_totals"] = merge_outcome_totals(out["chunks"])
            out["progress_totals"] = sum_progress(out["chunks"])
            return out
    # Bounded retry ONLY for entities still behind without an expected
    # fail-closed verdict (stale-base / unknown causes); no-bars items are
    # never retried (SKILL: expected fail-closed, do not retry).
    still = _still_lagging(mcp, entities, end_date)
    retry_heads = {k: h for k, h in still.items() if k not in fail_closed}
    if retry_heads:
        retry_items = build_append_items(retry_heads, end_date)
        for idx, chunk in enumerate(chunked(retry_items, 100), 1):
            rid = f"{prefix}-append-retry-{idx:03d}"
            rec = submit_and_wait(mcp, rid, chunk)
            rec["items"] = len(chunk)
            rec["outcomes"] = classify_range_results(rec.get("progress") or {})
            for u in (rec.get("progress") or {}).get("unfinished") or []:
                e = u.get("entity") or {}
                key = f"{e.get('exchange')}.{e.get('code')}"
                err = u.get("error") or ""
                if FAIL_CLOSED_NO_BARS in err:
                    fail_closed.setdefault(key, err)
                else:
                    hard_fail.setdefault(key, err)
            ph.record_task(rec)
            out["retry_chunks"].append(rec)
    out["still_lagging_final"] = sorted(_still_lagging(mcp, entities, end_date).keys())
    out["fail_closed_entities"] = sorted(fail_closed)
    out["hard_fail_entities"] = sorted(hard_fail)
    blocking = sorted(set(out["still_lagging_final"]) - set(fail_closed))
    if blocking or hard_fail:
        out["stopped"] = {"reason": "entities_still_lagging_or_failed",
                          "still_lagging_not_fail_closed": blocking,
                          "hard_fail": hard_fail}
    out.update(ph.finish())
    out["outcome_totals"] = merge_outcome_totals(out["chunks"] + out["retry_chunks"])
    out["progress_totals"] = sum_progress(out["chunks"] + out["retry_chunks"])
    return out


def _still_lagging(mcp: GuardedMcp, entities, end_date: date) -> dict:
    heads, _ = s5_run.daily_heads(mcp, entities, end_date)
    return {k: h for k, h in heads.items()
            if date.fromisoformat(h["last_record"]) < end_date}


def yearly_needs_rederive(yearly_items: list) -> set:
    """Entity keys whose yearly availability marks the row stale/invalid."""
    keys = set()
    for it in yearly_items:
        e = it.get("entity") or {}
        key = (e.get("exchange"), e.get("code"))
        reason = it.get("reason")
        if (not it.get("version_ref")) or it.get("stale") == YEARLY_REDERIVE_STALE \
                or reason in YEARLY_INVALID_REASONS:
            keys.add(key)
    return keys


def phase_yearly_rederive(mcp: GuardedMcp, prefix: str, entities,
                          updated_keys: set) -> dict:
    """S5 yearly_rederive shape restricted to entities needing it: yearly
    stale/absent OR daily updated this run. Same per-chunk availability +
    get_yearly_base_years + update_data(market.yearly) flow."""
    ph = Phase("yearly_rederive")
    out = {"rederive_set_size": 0, "skipped_up_to_date": 0}
    ph.start()
    for idx, chunk in enumerate(chunked(entities, 100), 1):
        yearly = mcp.call("get_data_availability", {
            "dataset": "market.yearly", "contract_version": "v1",
            "selection": {"kind": "batch", "entities": chunk},
            "filter": {"kind": "yearly", "start_year": 1900, "end_year": 2026},
            "version": {"kind": "latest_per_entity"}, "page_size": 100})
        items = yearly.get("items") or []
        need = yearly_needs_rederive(items) | {
            (it["entity"]["exchange"], it["entity"]["code"]) for it in items
            if f"{it['entity']['exchange']}.{it['entity']['code']}" in updated_keys}
        out["rederive_set_size"] += len(need)
        if not need:
            out["skipped_up_to_date"] += len(chunk)
            continue
        # Head pins come from list_data_versions, NOT availability: an
        # invalidated yearly reports version_ref=null (absent +
        # daily_replaced/value_semantics_changed) while market_series still
        # holds the head row — expected_yearly_head must pin that stored
        # version or the CAS gate rejects with "effective head already
        # exists". Only entities with no stored yearly at all get null.
        vers = mcp.call("list_data_versions", {
            "dataset": "market.yearly", "contract_version": "v1",
            "selection": {"kind": "batch", "entities": chunk}, "page_size": 100})
        yheads = {}
        for it in vers.get("items") or []:
            e = ((it.get("version_ref") or {}).get("entity")) or {}
            yheads[(e.get("exchange"), e.get("code"))] = \
                (it.get("version_ref") or {}).get("version_id")
        daily = mcp.call("get_data_availability", {
            "dataset": "market.daily", "contract_version": "v1",
            "selection": {"kind": "batch", "entities": chunk},
            "filter": {"kind": "daily", "start_date": "1990-01-01", "end_date": "2026-12-31"},
            "version": {"kind": "latest_per_entity"}, "page_size": 100})
        ditems = {}
        for it in daily.get("items") or []:
            e = it.get("entity") or {}
            k = (e.get("exchange"), e.get("code"))
            if k in need and it.get("version_ref") and it.get("first_record") and it.get("last_record"):
                ditems[k] = it
        if not ditems:
            ph.record_task({"index": idx, "status": "skipped_no_daily_head", "need": sorted(need)})
            continue
        req = [{"entity": {"exchange": k[0], "code": k[1]},
                "base_daily_version_id": i["version_ref"]["version_id"]}
               for k, i in ditems.items()]
        presence = mcp.call("get_yearly_base_years", {"contract_version": "v1", "items": req})
        resolved, empty, unavailable = resolve_years(presence, ditems)
        yitems = [{"entity": {"exchange": k[0], "code": k[1]},
                   "base_daily_version_id": i["version_ref"]["version_id"],
                   "years": resolved[k],
                   "publication": {"mode": "current_head",
                                   "expected_yearly_head": yheads.get(k)}}
                  for k, i in ditems.items() if k in resolved]
        rec = {"index": idx, "need": len(need), "empty": empty,
               "unavailable": unavailable, "items": len(yitems)}
        if yitems:
            rid = f"{prefix}-y-{idx:03d}"
            t = time.time()
            acc = mcp.call("update_data", {"dataset": "market.yearly", "contract_version": "v1",
                                           "request_id": rid, "items": yitems})
            prog, meta = wait_task(mcp, acc["task_id"])
            rec["task_id"] = acc["task_id"]
            rec["status"] = (prog.get("task") or {}).get("status")
            rec["wall_s"] = meta["terminal_seen"] - t
            rec["outcomes"] = classify_range_results(prog)
            if rec["status"] == "failed":
                rec["stopped"] = {"reason": "task_failed", "task_id": acc["task_id"]}
                ph.record_task(rec)
                out.update(ph.finish())
                return out
        ph.record_task(rec)
    out.update(ph.finish())
    out["outcome_totals"] = merge_outcome_totals(out["tasks"])
    out["progress_totals"] = sum_progress(out["tasks"])
    return out


def preflight(mcp: GuardedMcp, entities, end_date: date, expect: dict) -> dict:
    caps = mcp.call("get_data_capabilities", {})
    tool_names = {i.get("tool_name") for i in caps.get("items") or []}
    gen_ref = next((i.get("active_generation_ref") for i in caps.get("items") or []
                    if i.get("active_generation_ref")), None)
    inflight = []
    cursor = None
    while True:
        args = {"page_size": 1000}
        if cursor:
            args["cursor"] = cursor
        tl = mcp.call("list_tasks", args)
        inflight += [{"task_id": t.get("task_id"), "status": t.get("status")}
                     for t in tl.get("items") or []
                     if t.get("status") in INFLIGHT_TASK_STATUSES]
        cursor = tl.get("next_cursor")
        if not cursor:
            break
    heads, absent = s5_run.daily_heads(mcp, entities, end_date)
    absent_keys = sorted(f"{(a['entity'] or {}).get('exchange')}.{(a['entity'] or {}).get('code')}"
                         for a in absent)
    exp_abs = expected_absent(expect)
    # Capability/tool-count expectations are env-parametrized (default
    # v6/21): the same preflight must gate the post-switch v7/23 service.
    exp_capability = (expect.get("PROD_APPEND_V6_EXPECT_CAPABILITY")
                      or "generic-data-v6")
    exp_tools = int(expect.get("PROD_APPEND_V6_EXPECT_DATA_TOOLS") or "21")
    checks = {
        "capability_revision_expected": caps.get("capability_revision") == exp_capability,
        "data_tools_expected": len(tool_names) == exp_tools,
        "production_ready_false": caps.get("production_ready") is False,
        "active_generation_present": gen_ref is not None,
        "no_inflight_tasks": not inflight,
        "heads_present_except_allowed_absent": (
            not exp_abs or absent_keys == sorted(exp_abs)),
        "end_date_reasonable": end_date >= date(2000, 1, 1),
    }
    exp_gen = expect.get("PROD_APPEND_V6_EXPECT_GENERATION")
    if exp_gen:
        checks["generation_matches_expect"] = (gen_ref == exp_gen)
    return {"checks": checks, "ok": all(checks.values()),
            "expected": {"capability_revision": exp_capability,
                         "data_tools": exp_tools},
            "active_generation_ref": gen_ref, "inflight": inflight,
            "heads": len(heads), "absent": absent_keys,
            "capability": {k: caps.get(k) for k in
                           ("capability_revision", "production_ready")}}


def readback(mcp: GuardedMcp, db: str, entities, end_date: date,
             sample_entities: list, expect: dict) -> dict:
    out = {"samples": [], "end_date": end_date.isoformat()}
    heads, absent = s5_run.daily_heads(mcp, entities, end_date)
    for e in sample_entities:
        key = f"{e['exchange']}.{e['code']}"
        h = heads.get(key)
        out["samples"].append({"entity": key,
                               "last_record": h.get("last_record") if h else None,
                               "version_id": (h or {}).get("version_ref", {}).get("version_id") if h else None})
    yearly_items = []
    for chunk in chunked(entities, 100):
        y = mcp.call("get_data_availability", {
            "dataset": "market.yearly", "contract_version": "v1",
            "selection": {"kind": "batch", "entities": chunk},
            "filter": {"kind": "yearly", "start_year": 1900, "end_year": 2026},
            "version": {"kind": "latest_per_entity"}, "page_size": 100})
        yearly_items += y.get("items") or []
    # "Behind" only counts entities that CAN be derived — entities with no
    # daily head (the source-missing absent set) hold no yearly by design.
    daily_keys = {f"{h['entity']['exchange']}.{h['entity']['code']}"
                  for h in heads.values()}
    out["yearly_still_behind"] = len(
        {f"{k[0]}.{k[1]}" for k in yearly_needs_rederive(yearly_items)
         if f"{k[0]}.{k[1]}" in daily_keys})
    out["daily_published"] = len(heads)
    out["still_lagging"] = sorted(
        k for k, h in heads.items()
        if date.fromisoformat(h["last_record"]) < end_date)
    con = sqlite3.connect(f"file:{db}?mode=ro", uri=True, timeout=10)
    try:
        out["user_version"] = con.execute("PRAGMA user_version").fetchone()[0]
        out["staged_records"] = con.execute("SELECT count(*) FROM staged_records").fetchone()[0]
        out["append_events"] = con.execute(
            "SELECT count(*) FROM market_events WHERE kind='append'").fetchone()[0]
        out["no_change_count"] = con.execute(
            "SELECT count(*) FROM market_events WHERE kind='append' AND outcome='no_change'"
        ).fetchone()[0] if _col_exists(con, "market_events", "outcome") else None
    finally:
        con.close()
    diffs = []
    if out["yearly_still_behind"] != 0:
        diffs.append(f"yearly_still_behind {out['yearly_still_behind']} != 0")
    if out["user_version"] != 21:
        diffs.append(f"user_version {out['user_version']} != 21")
    exp_staged = expect.get("PROD_APPEND_V6_EXPECT_STAGED_RECORDS")
    want_staged = int(exp_staged) if exp_staged else 0
    if out["staged_records"] != want_staged:
        diffs.append(f"staged_records {out['staged_records']} != expected {want_staged}")
    exp_lag = expected_absent(expect) | {
        s.strip() for s in (expect.get("PROD_APPEND_V6_EXPECT_STILL_LAGGING") or "").split(",")
        if s.strip()}
    for s in out["samples"]:
        if (s["last_record"] is not None and s["last_record"] < end_date.isoformat()
                and s["entity"] not in exp_lag):
            diffs.append(f"sample {s['entity']} last_record {s['last_record']} < {end_date}")
    extra_lag = sorted(set(out["still_lagging"]) - exp_lag)
    if extra_lag:
        diffs.append(f"still_lagging beyond expected set: {extra_lag}")
    exp_daily = expect.get("PROD_APPEND_V6_EXPECT_DAILY_PUBLISHED")
    if exp_daily and out["daily_published"] != int(exp_daily):
        diffs.append(f"daily_published {out['daily_published']} != expected {exp_daily}")
    exp_max_app = expect.get("PROD_APPEND_V6_EXPECT_MAX_APPEND_EVENTS")
    if exp_max_app and out["append_events"] > int(exp_max_app):
        diffs.append(f"append_events {out['append_events']} > expected max {exp_max_app}")
    out["diffs"] = diffs
    return out


def _col_exists(con, table: str, col: str) -> bool:
    return any(r[1] == col for r in con.execute(f"PRAGMA table_info({table})"))


def main() -> int:
    signal.signal(signal.SIGINT, signal.default_int_handler)
    env = resolve_env()
    out_dir = Path(env["out"])
    run = {"started_at": utc_now(), "mode": env["mode"], "socket": env["sock"],
           "db": env["db"], "out": env["out"], "end_date": env["end_date"].isoformat(),
           "phases": {}}
    mcp = GuardedMcp(Mcp(env["sock"]))
    # Request ids must be unique per RUN: the server binds request_id -> task
    # (first-write-wins replay), so reusing a prior run's id would silently
    # adopt its old task instead of executing this run's items.
    run["request_tag"] = f"{env['prefix']}-{int(time.time())}"
    try:
        _, entities = s5_run.catalog_entities(mcp)
        pre = preflight(mcp, entities, env["end_date"], env["expect"])
        run["phases"]["preflight"] = pre
        write_json(str(out_dir / "append-preflight.json"), pre)
        if not pre["ok"]:
            run["error"] = f"preflight failed: {[k for k, v in pre['checks'].items() if not v]}"
            write_json(str(out_dir / "append-run.json"), run)
            print(run["error"], file=sys.stderr)
            return 2
        res = phase_append(mcp, run["request_tag"], entities, env["end_date"])
        run["phases"]["append"] = res
        write_json(str(out_dir / "append-daily.json"), res)
        if res.get("stopped"):
            raise RuntimeError(f"append stopped: {res['stopped']}")
        updated_keys = set(res.get("submitted_keys") or [])
        resy = phase_yearly_rederive(mcp, run["request_tag"], entities, updated_keys)
        run["phases"]["yearly_rederive"] = resy
        write_json(str(out_dir / "append-yearly.json"), resy)
        if resy.get("stopped") or any((t.get("status") == "failed") for t in resy.get("tasks", [])):
            raise RuntimeError("yearly_rederive failed")
        samples = [{"exchange": "sh", "code": "600000"},
                   {"exchange": "sz", "code": "000001"},
                   {"exchange": "sz", "code": "001246"},
                   {"exchange": "sh", "code": "688496"}]
        rb = readback(mcp, env["db"], entities, env["end_date"], samples, env["expect"])
        run["phases"]["readback"] = rb
        write_json(str(out_dir / "append-readback.json"), rb)
        if rb["diffs"]:
            run["error"] = "readback diffs: " + "; ".join(rb["diffs"])
            write_json(str(out_dir / "append-run.json"), run)
            print(run["error"], file=sys.stderr)
            return 3
    except KeyboardInterrupt:
        run["error"] = "interrupted"
        write_json(str(out_dir / "append-run.json"), run)
        raise
    except Exception as exc:
        run["error"] = str(exc)
        write_json(str(out_dir / "append-run.json"), run)
        raise
    run["finished_at"] = utc_now()
    write_json(str(out_dir / "append-run.json"), run)
    print(f"APPEND OK mode={env['mode']} end={env['end_date']} "
          f"lagging={run['phases']['append'].get('lagging_entities')}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
