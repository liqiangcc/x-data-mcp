#!/usr/bin/env python3
# Adapted from /tmp/xdata_daily_update_driver.py for XP-010 (no-change items: covered [last,last] range).
# Diffs: private SOCK; poll 1s (not 10s); per-chunk client+server timestamps.
import json, socket, sys, time, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from xp015_common import private_sock, classify_range_results, POLL_S
from datetime import date, timedelta, datetime, timezone
from pathlib import Path

SOCK = private_sock()  # XP-015: env XP015_SOCKET only, under /root/x-perf-g8-xp015/
OUT = Path(sys.argv[1])
END_DATE = date.fromisoformat(sys.argv[2])
REQ_PREFIX = sys.argv[3]

class Mcp:
    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(path)
        self.f = self.sock.makefile("r", encoding="utf-8")
        self.next_id = 0
    def call(self, name, arguments):
        self.next_id += 1
        req = {"jsonrpc":"2.0","id":self.next_id,"method":"tools/call","params":{"name":name,"arguments":arguments}}
        self.sock.sendall((json.dumps(req,separators=(",",":"))+"\n").encode())
        while True:
            line = self.f.readline()
            if not line: raise RuntimeError("socket closed")
            resp = json.loads(line)
            if resp.get("id") != self.next_id: continue
            if "error" in resp: raise RuntimeError(resp["error"])
            result = resp["result"]
            if result.get("isError"): raise RuntimeError(result)
            return result.get("structuredContent")

def iso(ts): return datetime.fromtimestamp(ts, timezone.utc).isoformat()

mcp = Mcp(SOCK)
result = {"request_id_prefix": REQ_PREFIX, "end_date": str(END_DATE), "poll_s": POLL_S,
          "catalog": {}, "availability_before": {}, "tasks": [], "absent_entities": [],
          "collection_attempt": None, "availability_after": {}, "sample_readbacks": [], "stopped": None}
try:
    caps = mcp.call("get_data_capabilities", {})
    result["capabilities"] = {"capability_revision": caps.get("implementation",{}).get("capability_revision"),
        "runtime_readiness": caps.get("runtime_readiness"), "blocking_reasons": caps.get("blocking_reasons")}
    versions = mcp.call("list_data_versions", {"dataset":"securities.catalog","contract_version":"v1",
        "selection":{"kind":"catalog","scope":"current_sh_sz_a"},"page_size":10})
    catalog_ref = versions["items"][0]["version_ref"]
    result["catalog"] = {"version_ref": catalog_ref}
    entities = []; cursor = None
    while True:
        args = {"dataset":"securities.catalog","contract_version":"v1",
                "selection":{"kind":"catalog","scope":"current_sh_sz_a"},
                "filter":{"kind":"catalog"},
                "version":{"kind":"catalog_version","version_id":catalog_ref["version_id"]},
                "page_size":1000}
        if cursor: args["cursor"] = cursor
        page = mcp.call("query_data", args)
        entities.extend(r["entity"] for r in page["records"])
        cursor = page.get("next_cursor")
        if not cursor: break
    result["catalog"]["entity_count"] = len(entities)

    def availability(selection, filter_):
        return mcp.call("get_data_availability", {"dataset":"market.daily","contract_version":"v1",
            "selection":selection,"filter":filter_,"version":{"kind":"latest_per_entity"},"page_size":1000})

    heads = {}; absent = []; avail_items = 0
    t_avail0 = time.time()
    for i in range(0, len(entities), 100):
        page = availability({"kind":"batch","entities":entities[i:i+100]},
                            {"kind":"daily","start_date":"1990-12-19","end_date":str(END_DATE)})
        for item in page.get("items", []):
            avail_items += 1
            ent = item["entity"]; key = f"{ent['exchange']}.{ent['code']}"
            if item.get("version_ref") and item.get("last_record"):
                heads[key] = {"entity": ent, "version_ref": item["version_ref"], "last_record": item["last_record"]}
            else:
                absent.append({"entity": ent, "reason": item.get("reason") or item.get("status")})
    result["availability_before"] = {"items": avail_items, "heads": len(heads), "absent": len(absent),
        "scan_wall_s": time.time()-t_avail0, "latest_counts": {}}
    lc = {}
    for h in heads.values(): lc[h["last_record"]] = lc.get(h["last_record"],0)+1
    result["availability_before"]["latest_counts"] = dict(sorted(lc.items()))
    result["absent_entities"] = absent

    update_items = []
    for h in heads.values():
        # XP-010 no-change run: submit each entity's already-covered
        # [last_record, last_record] so every item resolves as no_change —
        # identical task/tail machinery, no acquisition or append.
        last = h["last_record"]
        update_items.append({"entity": h["entity"], "base_version_id": h["version_ref"]["version_id"],
                             "start_date": last, "end_date": last})
    _mc = int(os.environ.get("XP015_MAX_CHUNKS", "0"))  # rehearsal bound; 0 = all
    if _mc: update_items = update_items[:_mc*100]
    result["daily_update_item_count"] = len(update_items)
    print(f"update_items={len(update_items)} absent={len(absent)}", flush=True)

    result["outcome_totals"] = {"considered": 0, "published": 0, "no_change": 0, "failed": 0, "needs_verification": 0, "unknown": 0}
    loop_m0 = time.monotonic(); loop_w0 = time.time()
    for index in range(0, len(update_items), 100):
        chunk_no = index // 100 + 1
        items = update_items[index:index+100]
        request_id = f"{REQ_PREFIX}-{chunk_no:03d}"
        t_submit = time.time(); m_submit = time.monotonic()
        acceptance = mcp.call("update_data", {"dataset":"market.daily","contract_version":"v1",
            "request_id":request_id,"items":items,"selection":None})
        t_accept = time.time(); m_accept = time.monotonic()
        task_id = acceptance["task_id"]
        polls = 0; first_seen = None
        while True:
            tq = time.time()
            progress = mcp.call("get_task_progress", {"task_id":task_id,"page_size":100})
            polls += 1
            if first_seen is None: first_seen = tq
            status = progress["task"]["status"]
            if status in {"succeeded","partially_failed","failed","cancelled","interrupted","needs_verification"}:
                break
            if tq - t_submit > 900:
                result["stopped"] = {"reason":"task_timeout","task_id":task_id,"status":status}
                raise RuntimeError(f"timeout {task_id} {status}")
            time.sleep(POLL_S)
        t_term = time.time(); m_term = time.monotonic()
        task = progress["task"]
        summary = {"request_id":request_id,"task_id":task_id,"status":status,
            "total":progress.get("total"),"succeeded":progress.get("succeeded"),
            "failed":progress.get("failed"),"pending":progress.get("pending"),
            "running":progress.get("running"),"finished":progress.get("finished"),
            "range_result_statuses":{},
            "client_submit":iso(t_submit),"client_accept":iso(t_accept),
            "accept_ms":(t_accept-t_submit)*1000,
            "client_terminal":iso(t_term),"polls":polls,
            "first_progress_seen":iso(first_seen),
            "server_created_at":task.get("created_at"),
            "server_last_progress_at":task.get("last_progress_at"),
            "wall_s":t_term-t_submit,
            "mono_wall_s":m_term-m_submit,"mono_accept_ms":(m_accept-m_submit)*1000,
            "mono_terminal_since_loop_start_s":m_term-loop_m0,
            "server_window_s":None}
        try:
            ca = datetime.fromisoformat(task["created_at"]).timestamp()
            lp = datetime.fromisoformat(task["last_progress_at"]).timestamp()
            summary["server_window_s"] = lp - ca
        except Exception: pass
        for item in progress.get("range_results", []):
            st = item.get("status") or item.get("result") or item.get("state") or "unknown"
            summary["range_result_statuses"][st] = summary["range_result_statuses"].get(st,0)+1
        outcome = classify_range_results(progress)
        summary["outcomes"] = outcome
        for k, v in outcome.items():
            if k in result["outcome_totals"]: result["outcome_totals"][k] += v
        result["tasks"].append(summary)
        print(f"chunk {chunk_no} status={status} wall={summary['wall_s']:.1f}s accept={summary['accept_ms']:.0f}ms srv={summary['server_window_s']}", flush=True)
        if status == "failed":
            result["stopped"] = {"reason":"task_failed","task_id":task_id}
            raise RuntimeError(f"task failed {task_id}")

    result["write_loop_mono_s"] = time.monotonic() - loop_m0
    result["write_loop_wall_clock"] = {"start": iso(loop_w0), "end": iso(time.time())}
    if absent:
        request_id = f"{REQ_PREFIX}-absent-collection"
        try:
            acceptance = mcp.call("start_collection", {"dataset":"market.daily","contract_version":"v1",
                "request_id":request_id,"selection":{"kind":"explicit","securities":[a["entity"] for a in absent]}})
            task_id = acceptance["task_id"]; started = time.time()
            while True:
                progress = mcp.call("get_task_progress", {"task_id":task_id,"page_size":100})
                status = progress["task"]["status"]
                if status in {"succeeded","partially_failed","failed","cancelled","interrupted","needs_verification"}: break
                if time.time()-started > 600: raise RuntimeError(f"timeout {task_id} {status}")
                time.sleep(POLL_S)
            result["collection_attempt"] = {"request_id":request_id,"task_id":task_id,"status":status,
                "total":progress.get("total"),"succeeded":progress.get("succeeded"),"failed":progress.get("failed")}
        except Exception as exc:
            result["collection_attempt"] = {"request_id":request_id,"error":str(exc)}

    after_heads = {}; after_absent = []
    for i in range(0, len(entities), 100):
        page = availability({"kind":"batch","entities":entities[i:i+100]},
                            {"kind":"daily","start_date":"1990-12-19","end_date":str(END_DATE)})
        for item in page.get("items", []):
            ent = item["entity"]; key = f"{ent['exchange']}.{ent['code']}"
            if item.get("version_ref") and item.get("last_record"): after_heads[key] = item
            else: after_absent.append({"entity":ent,"reason":item.get("reason") or item.get("status")})
    lc = {}
    for it in after_heads.values(): lc[it["last_record"]] = lc.get(it["last_record"],0)+1
    result["availability_after"] = {"heads":len(after_heads),"absent":len(after_absent),
        "latest_counts":dict(sorted(lc.items()))}
    result["absent_after"] = after_absent

    for symbol in ["sh.600000","sz.000001"]:
        ent = heads.get(symbol)
        if not ent: continue
        sample = mcp.call("query_data", {"dataset":"market.daily","contract_version":"v1",
            "selection":{"kind":"single","entity":ent["entity"]},
            "filter":{"kind":"daily","start_date":"2026-09-20","end_date":str(END_DATE)},
            "version":{"kind":"latest_per_entity"},"page_size":20})
        result["sample_readbacks"].append({"entity":symbol,"record_count":len(sample.get("records",[])),
            "records":sample.get("records",[])[-3:],"version_manifest":sample.get("version_manifest")})
except Exception as exc:
    result["error"] = str(exc)
    OUT.write_text(json.dumps(result, ensure_ascii=False, indent=1, sort_keys=True)+"\n")
    raise
OUT.write_text(json.dumps(result, ensure_ascii=False, indent=1, sort_keys=True)+"\n")
print(json.dumps({"catalog":result["catalog"],"before":result["availability_before"],
    "tasks":result["tasks"],"after":result["availability_after"],"error":result.get("error")}, indent=1))
