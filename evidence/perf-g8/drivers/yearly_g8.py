#!/usr/bin/env python3
# Adapted from /tmp/fresh_rebuild_yearly.py for XP-008 private yearly rerun.
# Raw unix socket; poll 1s; per-call latency log + per-chunk timing capture.
# XP-014: resolve_years' ~35-37 per-year get_data_availability probes per
# chunk are replaced by ONE get_yearly_base_years call per <=100-entity
# chunk (exact present years on the pinned base, single snapshot).
import json, socket, sys, time
from datetime import datetime, timezone
from pathlib import Path

SOCK = sys.argv[3] if len(sys.argv) > 3 else "/root/x-perf-g8/svc/run/mcp.sock"
RUN_ID = "g8-y"
POLL_S = 1.0
SUMMARY = Path(sys.argv[1] if len(sys.argv) > 1 else "/root/x-perf-g8/yearly-run.json")
LOG = Path(sys.argv[1] + ".log" if len(sys.argv) > 1 else "/root/x-perf-g8/yearly-run.log")
MAX_CHUNKS = int(sys.argv[2]) if len(sys.argv) > 2 else 0  # 0 = all chunks (smoke bound)
CALLS = []  # every mcp call: {name, ms, t}

def log(msg):
    line = f"{datetime.now(timezone.utc).isoformat()} {msg}"
    print(line, flush=True)
    with LOG.open("a", encoding="utf-8") as f: f.write(line + "\n")

class Mcp:
    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(path)
        self.f = self.sock.makefile("r", encoding="utf-8"); self.next_id = 0
    def call(self, name, arguments):
        self.next_id += 1; rid = self.next_id
        req = {"jsonrpc":"2.0","id":rid,"method":"tools/call","params":{"name":name,"arguments":arguments}}
        t0 = time.time()
        self.sock.sendall((json.dumps(req,separators=(",",":"))+"\n").encode())
        while True:
            line = self.f.readline()
            if not line: raise RuntimeError(f"{name} EOF")
            resp = json.loads(line)
            if resp.get("id") != rid: continue
            ms = (time.time()-t0)*1000
            CALLS.append({"name":name,"ms":round(ms,1),"t":round(t0,3)})
            if "error" in resp: raise RuntimeError(f"{name} rpc {resp['error']}")
            r = resp["result"]
            if r.get("isError"):
                e = RuntimeError(f"{name} tool {json.dumps(r.get('structuredContent') or r.get('content'))[:400]}")
                e.payload = r.get("structuredContent")
                raise e
            return r.get("structuredContent") or {}

def wait_task(client, task_id, timeout_s=1800):
    t0=time.time(); last=None; polls=0; first=None
    while time.time()-t0 < timeout_s:
        tq=time.time()
        p = client.call("get_task_progress", {"task_id":task_id,"page_size":1000})
        polls+=1
        if first is None: first=tq
        st=(p.get("task") or {}).get("status")
        if st!=last:
            log(f"task={task_id} status={st} fin={p.get('finished')}/{p.get('total')} fail={p.get('failed')} run={p.get('running')}")
            last=st
        if st in {"succeeded","partially_failed","failed","cancelled","needs_verification","interrupted"}:
            return p, {"polls":polls,"first_seen":first}
        time.sleep(POLL_S)
    raise RuntimeError(f"task {task_id} timeout")

def years_for(item):
    return list(range(int(item["first_record"][:4]), int(item["last_record"][:4])+1))

def resolve_years(client, daily_items):
    """One get_yearly_base_years call per <=100-entity chunk (XP-014):
    present_years of each pinned base daily version, resolved inside a
    single bounded snapshot. Returns (resolved, empty, unavailable):
    resolved maps entity-key -> exact in-window year set; empty lists
    entities whose in-window set is [] (recorded, never submitted);
    unavailable lists per-item failures that block an unqualified PASS.
    Future-dated years in the response are reported as anomalies, never
    silently dropped."""
    cur_year = datetime.now(timezone.utc).year
    req = [{"entity": {"exchange": k[0], "code": k[1]},
            "base_daily_version_id": i["version_ref"]["version_id"],
            "end_year": cur_year} for k, i in daily_items.items()]
    r = client.call("get_yearly_base_years", {"contract_version": "v1", "items": req})
    resolved, empty, unavailable = {}, [], []
    for it in r.get("items") or []:
        e = it.get("entity") or {}
        k = (e.get("exchange"), e.get("code"))
        if k not in daily_items:
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
            continue
        years = sorted(set(present) & set(years_for(daily_items[k])))
        if years:
            resolved[k] = years
        else:
            empty.append({"entity": k, "reason_code": "empty_present_years"})
    return resolved, empty, unavailable

mcp=Mcp(SOCK)
result={"run_id":RUN_ID,"started_at":datetime.now(timezone.utc).isoformat(),
        "poll_s":POLL_S,"catalog":{},"chunks":[],"calls":CALLS}
try:
    versions=mcp.call("list_data_versions",{"dataset":"securities.catalog","contract_version":"v1",
        "selection":{"kind":"catalog","scope":"current_sh_sz_a"},"page_size":10})
    cat_ver=versions["items"][0]["version_ref"]["version_id"]
    result["catalog"]["version_id"]=cat_ver
    records=[]; cursor=None
    while True:
        args={"dataset":"securities.catalog","contract_version":"v1",
              "selection":{"kind":"catalog","scope":"current_sh_sz_a"},
              "filter":{"kind":"catalog"},
              "version":{"kind":"catalog_version","version_id":cat_ver},"page_size":1000}
        if cursor: args["cursor"]=cursor
        page=mcp.call("query_data",args)
        records+=page.get("records") or []
        cursor=page.get("next_cursor")
        if not cursor: break
    entities=[]; seen=set()
    for rec in records:
        e=rec.get("entity") or {}
        if e.get("exchange") and e.get("code") and (e["exchange"],e["code"]) not in seen:
            seen.add((e["exchange"],e["code"])); entities.append({"exchange":e["exchange"],"code":e["code"]})
    result["catalog"]["entities"]=len(entities)
    log(f"entities={len(entities)}")

    chunks=[entities[i:i+100] for i in range(0,len(entities),100)]
    if MAX_CHUNKS: chunks=chunks[:MAX_CHUNKS]
    for idx,chunk in enumerate(chunks,1):
        crec={"index":idx}
        t_chunk=time.time()
        t0=time.time()
        daily=mcp.call("get_data_availability",{"dataset":"market.daily","contract_version":"v1",
            "selection":{"kind":"batch","entities":chunk},
            "filter":{"kind":"daily","start_date":"1990-01-01","end_date":"2026-12-31"},
            "version":{"kind":"latest_per_entity"},"page_size":100})
        crec["avail_daily_ms"]=round((time.time()-t0)*1000,1)
        t0=time.time()
        yearly=mcp.call("get_data_availability",{"dataset":"market.yearly","contract_version":"v1",
            "selection":{"kind":"batch","entities":chunk},
            "filter":{"kind":"yearly","start_year":1900,"end_year":2026},
            "version":{"kind":"latest_per_entity"},"page_size":100})
        crec["avail_yearly_ms"]=round((time.time()-t0)*1000,1)
        yheads={}
        for it in yearly.get("items") or []:
            e=it.get("entity") or {}; yheads[(e.get("exchange"),e.get("code"))]=(it.get("version_ref") or {}).get("version_id")
        ditems={}
        for it in daily.get("items") or []:
            e=it.get("entity") or {}; k=(e.get("exchange"),e.get("code"))
            if not it.get("version_ref") or not it.get("first_record") or not it.get("last_record"):
                continue
            ditems[k]=it
        # XP-014: exact per-chunk presence resolution BEFORE accept — one
        # get_yearly_base_years call replaces the per-year probe loop.
        if not ditems:
            crec["status"]="skipped"; result["chunks"].append(crec); continue
        tp=time.time()
        present,empty,unavailable=resolve_years(mcp,ditems)
        crec["presence_ms"]=round((time.time()-tp)*1000,1)
        if empty: crec["empty"]=empty
        if unavailable:
            crec["unavailable"]=unavailable
            result["unqualified"]=True  # any unavailable blocks unqualified PASS
        items=[{"entity":{"exchange":k[0],"code":k[1]},"base_daily_version_id":i["version_ref"]["version_id"],
                "years":present[k],"publication":{"mode":"current_head","expected_yearly_head":yheads.get(k)}}
               for k,i in ditems.items() if k in present]
        rid=f"{RUN_ID}-{idx:03d}"
        crec["items"]=len(items)
        # Resume checks BOTH binding kinds: the normal request_id and any
        # resolved-year request_id a previous run bound — the bound payload
        # replays verbatim; a fresh run may use a fresh request_id+pin.
        task_id=None
        for cand in (rid, rid+"-resolved-years"):
            t0=time.time()
            existing=mcp.call("get_request",{"request_id":cand,"contract_version":"v1"})
            crec.setdefault("get_request_ms",round((time.time()-t0)*1000,1))
            if existing.get("bound"):
                task_id=existing["task"]["task_id"]; crec["replayed"]=cand; break
        if task_id is None and items:
            t0=time.time()
            acc=mcp.call("update_data",{"dataset":"market.yearly","contract_version":"v1",
                 "request_id":rid,"items":items})
            task_id=acc["task_id"]; crec["accept_total_ms"]=round((time.time()-t0)*1000,1)
        elif task_id is None:
            crec["status"]="empty"; result["chunks"].append(crec); continue
        crec["request_id"]=rid; crec["task_id"]=task_id
        prog,meta=wait_task(mcp,task_id)
        task=prog["task"]
        crec.update({"status":task["status"],"succeeded":prog.get("succeeded"),"failed":prog.get("failed"),
                     "server_created_at":task.get("created_at"),"server_last_progress_at":task.get("last_progress_at"),
                     "polls":meta["polls"],"wall_s":round(time.time()-t_chunk,2)})
        for it in prog.get("unfinished") or []:
            if "selected year absent from fixed daily" not in str(it.get("error")):
                crec.setdefault("unexpected",[]).append(it)
        result["chunks"].append(crec)
        log(f"chunk {idx}/{len(chunks)} {task['status']} succ={prog.get('succeeded')} wall={crec['wall_s']}s presence_ms={crec.get('presence_ms')}")
    result["finished_at"]=datetime.now(timezone.utc).isoformat()
    SUMMARY.write_text(json.dumps(result,ensure_ascii=False,indent=1))
    log("complete")
except Exception as e:
    result["error"]=str(e)
    SUMMARY.write_text(json.dumps(result,ensure_ascii=False,indent=1))
    log(f"ERROR {e}")
    raise
