#!/usr/bin/env python3
"""S5 acceptance driver (schema-21 latest-only storage). WRITE-ONLY kit —
NOT run by its author; c-worker1 reviews and executes at S5.

Chain (mirrors the XP-015 load order, PLAN-XP-015 §1):
  init (fresh empty db) -> gen-A acquire -> collection (catalog + daily)
  -> yearly first pass -> gen-B acquire -> daily cross-gen append
  -> daily same-gen no-change -> yearly re-aggregate -> readback hook.

Timing (XP-015 口径): each write phase's write-loop is monotonic from
just before the first write submission to the last task terminal
observed, polling included; preflight/probes are recorded separately.
Gate: write-loop < 600 s per phase (docs/SLO.md).

All paths come from CLI args — no defaults point at the production
deploy dir or any /root/x-perf-g8* tree, and every resolved path is
asserted outside those roots at startup (s5_common.guard_paths).

usage:
  s5_run.py --stack-root DIR --pkg-bin DIR --gen-root DIR \
            --input-old DIR --input-new DIR --out DIR \
            [--init-subcommand init21] [--end-date YYYY-MM-DD] \
            [--phases collect,yearly1,append,nochange,yearly2] \
            [--max-chunks N] [--no-service-mgmt] [--request-prefix s5]
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import signal
import subprocess
import sys
import time
from datetime import date, datetime, timedelta
from zoneinfo import ZoneInfo
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "drivers"))
from s5_common import (
    Mcp,
    Phase,
    chunked,
    guard_paths,
    iso,
    merge_outcome_totals,
    sum_progress,
    db_size_snapshot,
    server_window,
    utc_now,
    wait_task,
    write_json,
    POLL_S,
)
from xp015_common import classify_range_results

ACQ_CONTRACT = {"contract_version": "v1"}


# ---------------------------------------------------------------------------
# service lifecycle (private stack only; every path guarded by caller)
# ---------------------------------------------------------------------------

class Stack:
    """data_service + acquire_service child processes under --stack-root.

    Layout: <root>/db/preview.sqlite, <root>/run/{mcp,acquire}.sock,
    <root>/gen (X_DATA_MCP_GENERATION_ROOT), <root>/mirror.
    """

    def __init__(self, root: str, pkg_bin: str, gen_root: str, mirror: str):
        self.root = Path(root)
        self.pkg_bin = Path(pkg_bin)
        self.gen_root = gen_root
        self.mirror = mirror
        self.db = str(self.root / "db" / "preview.sqlite")
        self.mcp_sock = str(self.root / "run" / "mcp.sock")
        self.acq_sock = str(self.root / "run" / "acquire.sock")
        self.procs = []

    def dirs(self):
        for d in ("db", "run", "mirror"):
            p = self.root / d
            p.mkdir(parents=True, exist_ok=True)
            os.chmod(p, 0o700)

    def init_db(self, subcommand: str) -> str:
        out = subprocess.run(
            [str(self.pkg_bin / "task_sqlite"), subcommand, self.db],
            capture_output=True, text=True, check=True,
        )
        os.chmod(self.db, 0o600)
        return out.stdout.strip()

    def start(self) -> None:
        env = dict(os.environ)
        env.pop("X_DATA_MCP_STAGE_TRACE", None)  # XP-015 口径: trace OFF
        env["X_DATA_MCP_GENERATION_ROOT"] = self.gen_root
        data = subprocess.Popen(
            [str(self.pkg_bin / "data_service"), self.db, self.mcp_sock],
            env=env,
        )
        acq = subprocess.Popen(
            [
                str(self.pkg_bin / "acquire_service"),
                self.gen_root,
                self.acq_sock,
                "--mirror",
                self.mirror,
            ],
            env=env,
        )
        self.procs = [data, acq]

    def wait_sockets(self, timeout_s: float = 60.0) -> dict:
        t0 = time.monotonic()
        for path in (self.mcp_sock, self.acq_sock):
            while not os.path.exists(path):
                if time.monotonic() - t0 > timeout_s:
                    raise RuntimeError(f"socket {path} did not appear in {timeout_s}s")
                for p in self.procs:
                    if p.poll() is not None:
                        raise RuntimeError(f"service pid {p.pid} exited {p.returncode}")
                time.sleep(0.1)
        return {"socket_wait_s": round(time.monotonic() - t0, 3)}

    def stop(self) -> None:
        for p in self.procs:
            if p.poll() is None:
                p.send_signal(signal.SIGTERM)
        for p in self.procs:
            try:
                p.wait(timeout=30)
            except subprocess.TimeoutExpired:
                p.kill()
                p.wait()


# ---------------------------------------------------------------------------
# acquire-side helpers
# ---------------------------------------------------------------------------

MIRROR_FILES = ("base.zip", "gbbq.zip", "hsjday.zip")


def mirror_inputs(mirror: str, input_dir: str) -> list:
    """Replace mirror contents with the generation's three declared role
    files (acquire's Mirror fetcher maps only these names; evidence JSON
    next to them in the input dir is not copied). Returns installed names."""
    for f in os.listdir(mirror):
        os.remove(os.path.join(mirror, f))
    installed = []
    for name in MIRROR_FILES:
        src = os.path.join(input_dir, name)
        if not os.path.isfile(src):
            raise RuntimeError(f"input dir {input_dir} lacks {name}")
        shutil.copy2(src, os.path.join(mirror, name))
        installed.append(name)
    return installed


def acquire_and_activate(acq: Mcp, request_prefix: str, tag: str) -> dict:
    """acquire_generation -> verify_generation -> activate_generation.
    Wire shapes per src/acquire/wire.rs: the acceptance carries
    request_id/task_id/state/reused only; the task view carries `state`
    (accepted/running/succeeded/failed) and, once installed, generation_id;
    verify replies `state` (consistency_verified/not_verified/conflict/
    refused); activate replies `state` (active/refused + reason_code);
    sync_status carries the active pointer under `active`."""
    rec = {"tag": tag}
    t = time.monotonic()
    accept = acq.call("acquire_generation", {**ACQ_CONTRACT, "request_id": f"{request_prefix}-{tag}-acq"})
    rec["acquire_accept"] = accept
    task_id = accept.get("task_id")
    while True:
        task = acq.call("get_acquisition_task", {**ACQ_CONTRACT, "task_id": task_id})
        status = task.get("state")
        if status in {"succeeded", "failed"}:
            break
        if time.monotonic() - t > 2700:
            raise RuntimeError(f"acquisition {task_id} timeout state={status}")
        time.sleep(5.0)
    if status != "succeeded":
        raise RuntimeError(f"acquisition {task_id} terminal {status}: {task}")
    gen_id = task.get("generation_id")
    if not gen_id:
        raise RuntimeError(f"acquisition {task_id} succeeded without generation_id: {task}")
    rec["acquire_task"] = {"task_id": task_id, "generation_id": gen_id, "status": status,
                           "stage": task.get("stage"), "bytes_done": task.get("bytes_done")}
    ver = acq.call("verify_generation", {**ACQ_CONTRACT, "request_id": f"{request_prefix}-{tag}-ver",
                                          "generation_id": gen_id})
    rec["verify"] = ver
    if ver.get("state") != "consistency_verified":
        raise RuntimeError(f"verify_generation not verified: {ver}")
    sync = acq.call("sync_status", ACQ_CONTRACT)
    prev = (sync.get("active") or {}).get("generation_id")
    act = acq.call("activate_generation", {**ACQ_CONTRACT, "request_id": f"{request_prefix}-{tag}-act",
                                            "generation_id": gen_id, "expected_previous": prev})
    rec["activate"] = act
    if act.get("state") != "active":
        raise RuntimeError(f"activate_generation refused: {act}")
    rec["wall_s"] = round(time.monotonic() - t, 1)
    return rec


# ---------------------------------------------------------------------------
# data-side phases
# ---------------------------------------------------------------------------

def catalog_entities(mcp: Mcp) -> tuple:
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
    return cat_ver, entities


def availability(mcp: Mcp, dataset: str, entities, filter_):
    items = []
    for chunk in chunked(entities, 100):
        page = mcp.call("get_data_availability", {
            "dataset": dataset, "contract_version": "v1",
            "selection": {"kind": "batch", "entities": chunk},
            "filter": filter_, "version": {"kind": "latest_per_entity"}, "page_size": 100})
        items.extend(page.get("items") or [])
    return items


def task_summary(request_id, task_id, progress, meta, t_submit, t_accept):
    task = progress["task"]
    return {
        "request_id": request_id, "task_id": task_id,
        "status": task.get("status"),
        "client_submit": iso(t_submit), "client_accept": iso(t_accept),
        "accept_ms": (t_accept - t_submit) * 1000,
        "client_terminal": iso(meta["terminal_seen"]), "polls": meta["polls"],
        "server_window_s": server_window(task),
        "wall_s": meta["terminal_seen"] - t_submit,
        "total": progress.get("total"), "succeeded": progress.get("succeeded"),
        "failed": progress.get("failed"),
        "outcomes": classify_range_results(progress),
    }


def phase_collect(mcp: Mcp, prefix: str, max_chunks: int) -> dict:
    ph = Phase("collect")
    out = {"catalog": {}, "chunks": []}
    t = time.time(); ph.start()
    acc = mcp.call("start_collection", {
        "dataset": "securities.catalog", "contract_version": "v1",
        "request_id": f"{prefix}-catalog",
        "selection": {"scope": "current_sh_sz_a"}})
    prog, meta = wait_task(mcp, acc["task_id"])
    out["catalog"] = {"task_id": acc["task_id"], "status": prog["task"]["status"],
                      "wall_s": meta["terminal_seen"] - t}
    cat_ver, entities = catalog_entities(mcp)
    out["catalog"]["version_id"] = cat_ver
    out["catalog"]["entities"] = len(entities)
    chunks = chunked(entities, 100)
    if max_chunks:
        chunks = chunks[:max_chunks]
    for idx, chunk in enumerate(chunks, 1):
        rid = f"{prefix}-daily-{idx:03d}"
        t = time.time()
        acc = mcp.call("start_collection", {
            "dataset": "market.daily", "contract_version": "v1", "request_id": rid,
            "selection": {"kind": "explicit", "securities": chunk}})
        t_acc = time.time()
        prog, meta = wait_task(mcp, acc["task_id"])
        rec = task_summary(rid, acc["task_id"], prog, meta, t, t_acc)
        rec["items"] = len(chunk)
        ph.record_task(rec)
        out["chunks"].append(rec)
        status = prog["task"]["status"]
        if status in {"failed", "cancelled", "needs_verification", "interrupted"}:
            out["aborted_at"] = idx
            break
    out.update(ph.finish())
    out["outcome_totals"] = merge_outcome_totals(out["chunks"])
    out["progress_totals"] = sum_progress(out["chunks"])
    return out


def daily_heads(mcp: Mcp, entities, end_date: date) -> tuple:
    items = availability(mcp, "market.daily", entities,
                         {"kind": "daily", "start_date": "1990-12-19",
                          "end_date": end_date.isoformat()})
    heads, absent = {}, []
    for item in items:
        e = item.get("entity") or {}
        key = f"{e.get('exchange')}.{e.get('code')}"
        if item.get("version_ref") and item.get("last_record"):
            heads[key] = item
        else:
            absent.append({"entity": e, "reason": item.get("reason") or item.get("status")})
    return heads, absent


def phase_daily_update(mcp: Mcp, prefix: str, name: str, entities,
                       end_date: date, no_change: bool, max_chunks: int) -> dict:
    """update_g8.py / update_g8_nc.py shape, chunked <=100 serial."""
    t_pf = time.monotonic()
    heads, absent = daily_heads(mcp, entities, end_date)
    preflight_s = time.monotonic() - t_pf
    items = []
    for h in heads.values():
        last = date.fromisoformat(h["last_record"])
        start = last if no_change else last + timedelta(days=1)
        if start <= end_date:
            items.append({"entity": h["entity"],
                          "base_version_id": h["version_ref"]["version_id"],
                          "start_date": start.isoformat(), "end_date": end_date.isoformat()})
    if max_chunks:
        items = items[: max_chunks * 100]
    ph = Phase(name)
    out = {"update_items": len(items), "absent": absent, "preflight_s": preflight_s}
    ph.start()
    for idx, chunk in enumerate(chunked(items, 100), 1):
        rid = f"{prefix}-{idx:03d}"
        t = time.time()
        acc = mcp.call("update_data", {"dataset": "market.daily", "contract_version": "v1",
                                       "request_id": rid, "items": chunk, "selection": None})
        t_acc = time.time()
        prog, meta = wait_task(mcp, acc["task_id"])
        rec = task_summary(rid, acc["task_id"], prog, meta, t, t_acc)
        ph.record_task(rec)
        if prog["task"]["status"] == "failed":
            out["stopped"] = {"reason": "task_failed", "task_id": acc["task_id"]}
            break
    out.update(ph.finish())
    out["outcome_totals"] = merge_outcome_totals(out["tasks"])
    out["progress_totals"] = sum_progress(out["tasks"])
    return out


def resolve_years(presence: dict, ditems: dict) -> tuple:
    """XP-014/yearly_g8.resolve_years shape on one get_yearly_base_years
    reply. Returns (resolved, empty, unavailable): resolved maps entity-key
    -> in-window present years; future-dated years (> current Asia/Shanghai
    year) are recorded as anomalies in `unavailable`, never silently dropped
    by the window intersection; unrequested/unresolved/missing-years items
    are `unavailable` too (they block an unqualified PASS)."""
    cur_year = datetime.now(ZoneInfo("Asia/Shanghai")).year
    resolved, empty, unavailable = {}, [], []
    for it in presence.get("items") or []:
        e = it.get("entity") or {}
        k = (e.get("exchange"), e.get("code"))
        if k not in ditems:
            unavailable.append({"entity": k, "reason_code": "unrequested_item"})
            continue
        if it.get("status") != "resolved":
            unavailable.append({"entity": k, "reason_code": it.get("reason_code")})
            continue
        present = it.get("present_years")
        if not isinstance(present, list):
            unavailable.append({"entity": k, "reason_code": "missing_present_years"})
            continue
        future = sorted(y for y in present if y > cur_year)
        if future:
            unavailable.append({"entity": k, "reason_code": "future_years_in_response", "years": future})
        lo, hi = int(ditems[k]["first_record"][:4]), int(ditems[k]["last_record"][:4])
        years = sorted(y for y in present if lo <= y <= hi)
        if years:
            resolved[k] = years
        else:
            empty.append({"entity": k, "reason_code": "empty_present_years"})
    return resolved, empty, unavailable


def phase_yearly(mcp: Mcp, prefix: str, name: str, entities, max_chunks: int) -> dict:
    """yearly_g8.py shape: per <=100 chunk — daily+yearly availability,
    one get_yearly_base_years, update_data(market.yearly,current_head)."""
    ph = Phase(name)
    out = {}
    ph.start()
    chunks = chunked(entities, 100)
    if max_chunks:
        chunks = chunks[:max_chunks]
    for idx, chunk in enumerate(chunks,1):
        crec = {"index": idx}
        daily = mcp.call("get_data_availability", {
            "dataset": "market.daily", "contract_version": "v1",
            "selection": {"kind": "batch", "entities": chunk},
            "filter": {"kind": "daily", "start_date": "1990-01-01", "end_date": "2026-12-31"},
            "version": {"kind": "latest_per_entity"}, "page_size": 100})
        yearly = mcp.call("get_data_availability", {
            "dataset": "market.yearly", "contract_version": "v1",
            "selection": {"kind": "batch", "entities": chunk},
            "filter": {"kind": "yearly", "start_year": 1900, "end_year": 2026},
            "version": {"kind": "latest_per_entity"}, "page_size": 100})
        yheads = {}
        for it in yearly.get("items") or []:
            e = it.get("entity") or {}
            yheads[(e.get("exchange"), e.get("code"))] = (it.get("version_ref") or {}).get("version_id")
        ditems = {}
        for it in daily.get("items") or []:
            e = it.get("entity") or {}
            k = (e.get("exchange"), e.get("code"))
            if it.get("version_ref") and it.get("first_record") and it.get("last_record"):
                ditems[k] = it
        if not ditems:
            crec["status"] = "skipped"
            ph.record_task(crec)
            continue
        req = [{"entity": {"exchange": k[0], "code": k[1]},
                "base_daily_version_id": i["version_ref"]["version_id"]}
               for k, i in ditems.items()]
        presence = mcp.call("get_yearly_base_years", {"contract_version": "v1", "items": req})
        resolved, empty, unavailable = resolve_years(presence, ditems)
        items = [{"entity": {"exchange": k[0], "code": k[1]},
                  "base_daily_version_id": i["version_ref"]["version_id"],
                  "years": resolved[k],
                  "publication": {"mode": "current_head",
                                  "expected_yearly_head": yheads.get(k)}}
                 for k, i in ditems.items() if k in resolved]
        if empty:
            crec["empty"] = empty
        if unavailable:
            crec["unavailable"] = unavailable
        if not items:
            crec["status"] = "empty"
            ph.record_task(crec)
            continue
        rid = f"{prefix}-{idx:03d}"
        t = time.time()
        acc = mcp.call("update_data", {"dataset": "market.yearly", "contract_version": "v1",
                                       "request_id": rid, "items": items})
        t_acc = time.time()
        prog, meta = wait_task(mcp, acc["task_id"])
        rec = task_summary(rid, acc["task_id"], prog, meta, t, t_acc)
        rec.update(crec)
        rec["items"] = len(items)
        ph.record_task(rec)
        if prog["task"]["status"] == "failed":
            out["stopped"] = {"reason": "task_failed", "task_id": acc["task_id"]}
            break
    out.update(ph.finish())
    out["outcome_totals"] = merge_outcome_totals(out["tasks"])
    out["progress_totals"] = sum_progress(out["tasks"])
    return out


# ---------------------------------------------------------------------------

def main() -> int:
    # POSIX: a background (`&`) child of a non-interactive shell inherits
    # SIGINT=SIG_IGN, and Python then never installs its KeyboardInterrupt
    # handler — which is why watch_s5.py's SIGINT was a no-op on the
    # detached 2026-10-13 r5 run. Install it explicitly; the finally block
    # below still stops the stack services on interrupt.
    signal.signal(signal.SIGINT, signal.default_int_handler)
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--stack-root", required=True, help="private stack dir (db/run/mirror)")
    ap.add_argument("--pkg-bin", required=True, help="dir holding data_service/acquire_service/task_sqlite")
    ap.add_argument("--gen-root", required=True, help="private generation root (X_DATA_MCP_GENERATION_ROOT)")
    ap.add_argument("--input-old", required=True, help="gen-A input file dir (goes to mirror first)")
    ap.add_argument("--input-new", required=True, help="gen-B input file dir")
    ap.add_argument("--out", required=True, help="output dir for phase JSONs")
    ap.add_argument("--init-subcommand", default="init21",
                    help="task_sqlite init subcommand (init20 prints 'task schema 21 ready' on v21 builds)")
    ap.add_argument("--end-date", default=None,
                    help="update window end date; default = input-new snapshot's latest trade day (operator supplies)")
    ap.add_argument("--phases", default="collect,yearly1,genb,append,nochange,yearly2",
                    help="comma-separated phase subset")
    ap.add_argument("--max-chunks", type=int, default=0, help="rehearsal bound; 0 = all")
    ap.add_argument("--no-service-mgmt", action="store_true",
                    help="do not spawn/stop services; only drive MCP calls on existing sockets")
    ap.add_argument("--request-prefix", default="s5")
    args = ap.parse_args()

    safe = guard_paths({
        "stack_root": args.stack_root, "pkg_bin": args.pkg_bin,
        "gen_root": args.gen_root, "input_old": args.input_old,
        "input_new": args.input_new, "out": args.out,
    })
    out_dir = Path(safe["out"]); out_dir.mkdir(parents=True, exist_ok=True)
    Path(safe["gen_root"]).mkdir(parents=True, exist_ok=True)
    stack = Stack(safe["stack_root"], safe["pkg_bin"], safe["gen_root"],
                  str(Path(safe["stack_root"]) / "mirror"))
    stack.dirs()
    run = {"started_at": utc_now(), "phases": {}, "args": {
        "init_subcommand": args.init_subcommand, "end_date": args.end_date,
        "phases": args.phases, "max_chunks": args.max_chunks,
        "no_service_mgmt": args.no_service_mgmt, "poll_s": POLL_S}}
    phases = args.phases.split(",")
    run["db_sizes"] = {}

    def mark(label: str) -> None:
        if not args.no_service_mgmt or os.path.exists(stack.db):
            run["db_sizes"][label] = db_size_snapshot(stack.db)
    unknown = set(phases) - {"collect", "yearly1", "genb", "append", "nochange", "yearly2"}
    if unknown:
        sys.exit(f"unknown phases {sorted(unknown)}")
    if {"append", "nochange"} & set(phases) and not args.end_date:
        sys.exit("--end-date is required for the append/nochange phases (checked before any work starts)")
    try:
        if not args.no_service_mgmt:
            run["init_out"] = stack.init_db(args.init_subcommand)
            stack.start()
            run["startup"] = stack.wait_sockets()
            mark("after_init")
        mcp = Mcp(stack.mcp_sock)
        acq = Mcp(stack.acq_sock)
        caps = mcp.call("get_data_capabilities", {})
        run["capabilities"] = {k: caps.get(k) for k in ("capability_revision", "implementation") if k in caps}
        if "collect" in phases:
            if not args.no_service_mgmt:
                installed = mirror_inputs(stack.mirror, safe["input_old"])
                run["phases"]["gen_a"] = acquire_and_activate(acq, args.request_prefix, "gena")
                run["phases"]["gen_a"]["mirror_files"] = installed
            else:
                run["phases"]["gen_a"] = {"skipped": "no_service_mgmt"}
            write_json(str(out_dir / "s5-gena.json"), run["phases"]["gen_a"])
            mark("after_gen_a")
            res = phase_collect(mcp, args.request_prefix, args.max_chunks)
            run["phases"]["collect"] = res
            write_json(str(out_dir / "s5-collect.json"), res)
            mark("after_collect")
        # Entity list is re-read after collection so later phases pin the
        # catalog snapshot that exists when they run (same as the drivers).
        entities = None
        if any(p in phases for p in ("yearly1", "append", "nochange", "yearly2")):
            _, entities = catalog_entities(mcp)
        if "yearly1" in phases:
            res = phase_yearly(mcp, f"{args.request_prefix}-y1", "yearly_first", entities, args.max_chunks)
            run["phases"]["yearly_first"] = res
            write_json(str(out_dir / "s5-yearly-first.json"), res)
            mark("after_yearly_first")
        if "genb" in phases:
            if not args.no_service_mgmt:
                installed = mirror_inputs(stack.mirror, safe["input_new"])
                run["phases"]["gen_b"] = acquire_and_activate(acq, args.request_prefix, "genb")
                run["phases"]["gen_b"]["mirror_files"] = installed
                write_json(str(out_dir / "s5-genb.json"), run["phases"]["gen_b"])
                mark("after_gen_b")
        end = date.fromisoformat(args.end_date) if args.end_date else None
        for name, no_change in (("append", False), ("nochange", True)):
            if name in phases:
                if end is None:
                    raise RuntimeError(f"phase {name} requires --end-date")
                res = phase_daily_update(mcp, f"{args.request_prefix}-{name}", name,
                                         entities, end, no_change, args.max_chunks)
                run["phases"][name] = res
                write_json(str(out_dir / f"s5-{name}.json"), res)
                mark(f"after_{name}")
        if "yearly2" in phases:
            res = phase_yearly(mcp, f"{args.request_prefix}-y2", "yearly_rederive", entities, args.max_chunks)
            run["phases"]["yearly_rederive"] = res
            write_json(str(out_dir / "s5-yearly-rederive.json"), res)
            mark("after_yearly_rederive")
    except KeyboardInterrupt:
        run["error"] = "interrupted (SIGINT, e.g. watchdog stop-line)"
        write_json(str(out_dir / "s5-run.json"), run)
        raise
    except Exception as exc:
        run["error"] = str(exc)
        write_json(str(out_dir / "s5-run.json"), run)
        raise
    finally:
        if not args.no_service_mgmt:
            stack.stop()
    run["finished_at"] = utc_now()
    write_json(str(out_dir / "s5-run.json"), run)
    return 0


if __name__ == "__main__":
    sys.exit(main())
