#!/usr/bin/env python3
# Fresh-rebuild driver (derived from rebuild_g8.py): catalog -> 53 x <=100 daily start_collection.
# Empty-database prechecks: refuses unless the target has no catalog (before any write) and zero daily heads (before the first daily write).
# Raw unix-socket JSON-RPC (same frames mcp_connect bridges to stdio).
# Adds: catalog start_collection first; per-chunk client+server timestamps.
import json, socket, sys, time, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from xp015_common import POLL_S
from prod_guard import socket_path, out_dir
from datetime import datetime, timezone
from pathlib import Path

SOCK, MODE = socket_path()  # production socket only when explicitly and exactly allowed (prod_guard)
RUN_ID = os.environ.get("G8_RUN_ID", "deploy31-rebuild")
_OUT = out_dir(MODE)
SUMMARY = Path(_OUT) / f"{RUN_ID}.json"
LOG = Path(_OUT) / f"{RUN_ID}.log"

def log(msg):
    line = f"{datetime.now(timezone.utc).isoformat()} {msg}"
    print(line, flush=True)
    with LOG.open("a", encoding="utf-8") as f:
        f.write(line + "\n")

def now():
    return time.time()

class Mcp:
    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(path)
        self.f = self.sock.makefile("r", encoding="utf-8")
        self.next_id = 0
    def call(self, name, arguments):
        self.next_id += 1
        rid = self.next_id
        req = {"jsonrpc":"2.0","id":rid,"method":"tools/call","params":{"name":name,"arguments":arguments}}
        self.sock.sendall((json.dumps(req,separators=(",",":"))+"\n").encode())
        while True:
            line = self.f.readline()
            if not line: raise RuntimeError(f"{name} EOF")
            resp = json.loads(line)
            if resp.get("id") != rid: continue
            if "error" in resp: raise RuntimeError(f"{name} rpc {resp['error']}")
            r = resp["result"]
            if r.get("isError"): raise RuntimeError(f"{name} tool {r.get('structuredContent') or r.get('content')}")
            return r.get("structuredContent") or {}

def wait_task(client, task_id, timeout_s=3600):
    t0 = now(); first_seen = None; last = None
    polls = 0
    while now() - t0 < timeout_s:
        tq = now()
        progress = client.call("get_task_progress", {"task_id": task_id, "page_size": 1000})
        polls += 1
        if first_seen is None:
            first_seen = tq
        task = progress.get("task") or {}
        status = task.get("status")
        if status != last:
            log(f"task={task_id} status={status} fin={progress.get('finished')}/{progress.get('total')} failed={progress.get('failed')} running={progress.get('running')}")
            last = status
        if status in {"succeeded","partially_failed","failed","cancelled","needs_verification","interrupted"}:
            return progress, {"polls": polls, "first_progress_seen": first_seen, "terminal_seen": now()}
        time.sleep(POLL_S)
    raise RuntimeError(f"task {task_id} timeout")

mcp = Mcp(SOCK)
result = {"run_id": RUN_ID, "started_at": datetime.now(timezone.utc).isoformat(),
          "poll_s": POLL_S, "catalog": {}, "daily": {"chunks": []}}
try:
    # --- precheck 1 (before ANY write): the target must have no catalog version ---
    pre = mcp.call("list_data_versions", {"dataset": "securities.catalog", "contract_version": "v1",
        "selection": {"kind": "catalog", "scope": "current_sh_sz_a"}, "page_size": 10})
    if (pre.get("items") or []):
        raise RuntimeError(f"REFUSED: target already has catalog version(s); fresh rebuild requires an empty database")
    result["precheck_catalog_versions"] = 0
    # --- catalog collection ---
    t = now()
    acc = mcp.call("start_collection", {
        "dataset": "securities.catalog", "contract_version": "v1",
        "request_id": f"{RUN_ID}-catalog",
        "selection": {"scope": "current_sh_sz_a"}})
    t_acc = now()
    log(f"catalog accepted task={acc['task_id']} accept_wall_ms={(t_acc-t)*1000:.1f}")
    prog, meta = wait_task(mcp, acc["task_id"])
    task = prog["task"]
    result["catalog"] = {"task_id": acc["task_id"], "status": task["status"],
        "client_submit": t, "client_accept": t_acc, "accept_ms": (t_acc-t)*1000,
        "server_created_at": task.get("created_at"), "server_last_progress_at": task.get("last_progress_at"),
        "client_terminal": meta["terminal_seen"], "polls": meta["polls"],
        "wall_s": meta["terminal_seen"] - t}
    log(f"catalog terminal status={task['status']} wall={result['catalog']['wall_s']:.1f}s")

    versions = mcp.call("list_data_versions", {
        "dataset": "securities.catalog", "contract_version": "v1",
        "selection": {"kind":"catalog","scope":"current_sh_sz_a"}, "page_size": 10})
    cat_ver = versions["items"][0]["version_ref"]["version_id"]
    result["catalog"]["version_id"] = cat_ver
    log(f"catalog_version={cat_ver}")

    records = []; cursor = None
    while True:
        args = {"dataset":"securities.catalog","contract_version":"v1",
                "selection":{"kind":"catalog","scope":"current_sh_sz_a"},
                "filter":{"kind":"catalog"},
                "version":{"kind":"catalog_version","version_id":cat_ver},
                "page_size":1000}
        if cursor: args["cursor"] = cursor
        page = mcp.call("query_data", args)
        records += page.get("records") or []
        cursor = page.get("next_cursor")
        if not cursor: break
    entities = []; seen = set()
    for rec in records:
        e = rec.get("entity") or {}
        if e.get("exchange") and e.get("code") and (e["exchange"],e["code"]) not in seen:
            seen.add((e["exchange"],e["code"])); entities.append({"exchange":e["exchange"],"code":e["code"]})
    result["catalog"]["records"] = len(records); result["catalog"]["entities"] = len(entities)
    log(f"catalog entities={len(entities)}")

    # --- precheck 2 (before the first daily write): full-catalog availability must show 0 daily heads ---
    heads = 0
    for i in range(0, len(entities), 100):
        page = mcp.call("get_data_availability", {"dataset": "market.daily", "contract_version": "v1",
            "selection": {"kind": "batch", "entities": entities[i:i+100]},
            "filter": {"kind": "daily", "start_date": "1990-12-19", "end_date": "2099-12-31"},
            "version": {"kind": "latest_per_entity"}, "page_size": 100})
        heads += sum(1 for it in page.get("items", []) if it.get("version_ref") and it.get("last_record"))
    result["precheck_daily_heads"] = heads
    if heads:
        raise RuntimeError(f"REFUSED: {heads} published daily head(s) already exist; refusing to write")
    # --- daily collection, 100-item chunks serial ---
    chunks = [entities[i:i+100] for i in range(0, len(entities), 100)]
    _mc = int(os.environ.get("XP015_MAX_CHUNKS", "0"))  # rehearsal bound; 0 = all
    if _mc: chunks = chunks[:_mc]
    _mc = int(os.environ.get("XP015_MAX_CHUNKS", "0"))  # rehearsal bound; 0 = all
    if _mc: chunks = chunks[:_mc]
    result["daily"]["chunk_total"] = len(chunks)
    published = 0
    for idx, chunk in enumerate(chunks, 1):
        req = f"{RUN_ID}-daily-{idx:03d}"
        t = now()
        acc = mcp.call("start_collection", {
            "dataset":"market.daily","contract_version":"v1","request_id":req,
            "selection":{"kind":"explicit","securities":chunk}})
        t_acc = now()
        prog, meta = wait_task(mcp, acc["task_id"])
        task = prog["task"]
        rec = {"index": idx, "request_id": req, "task_id": acc["task_id"],
               "items": len(chunk), "status": task["status"],
               "succeeded": prog.get("succeeded"), "failed": prog.get("failed"),
               "client_submit": t, "client_accept": t_acc,
               "accept_ms": (t_acc-t)*1000,
               "server_created_at": task.get("created_at"),
               "server_last_progress_at": task.get("last_progress_at"),
               "client_terminal": meta["terminal_seen"], "polls": meta["polls"],
               "wall_s": meta["terminal_seen"] - t}
        result["daily"]["chunks"].append(rec)
        published += int(prog.get("succeeded") or 0)
        log(f"chunk {idx}/{len(chunks)} status={task['status']} succ={prog.get('succeeded')} fail={prog.get('failed')} wall={rec['wall_s']:.1f}s accept_ms={rec['accept_ms']:.0f}")
        bad = [i for i in prog.get("unfinished") or []
               if "member absent or ambiguous" not in str(i.get("error"))]
        if task["status"] in {"failed","cancelled","needs_verification","interrupted"} or bad:
            result["daily"]["aborted_at"] = idx
            result["daily"]["unexpected"] = bad
            SUMMARY.write_text(json.dumps(result, ensure_ascii=False, indent=1))
            raise RuntimeError(f"chunk {idx} terminal {task['status']} unexpected={bad[:3]}")
    result["daily"]["published"] = published
    result["finished_at"] = datetime.now(timezone.utc).isoformat()
    SUMMARY.write_text(json.dumps(result, ensure_ascii=False, indent=1))
    log(f"complete published={published}")
except Exception as e:
    result["error"] = str(e)
    SUMMARY.write_text(json.dumps(result, ensure_ascii=False, indent=1))
    log(f"ERROR {e}")
    raise
