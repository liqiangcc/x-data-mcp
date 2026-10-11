#!/usr/bin/env python3
# XP-010 C: get_data_availability latency — daily batch scan, yearly batch,
# and a resolve_years-style per-year probe on one 100-entity chunk.
import json, socket, sys, time
from datetime import datetime, timezone
import os as _os, sys as _sys
_sys.path.insert(0, _os.path.dirname(_os.path.abspath(__file__)))
from xp015_common import private_sock
SOCK = private_sock()  # XP-015: env XP015_SOCKET only, under /root/x-perf-g8-xp015/
OUT = sys.argv[1]
N_DAILY_CHUNKS = int(sys.argv[2]) if len(sys.argv) > 2 else 5
PROBE_CHUNK = int(sys.argv[3]) if len(sys.argv) > 3 else 24   # 1-based; baseline probe chunk

class Mcp:
    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(path)
        self.f = self.sock.makefile("r", encoding="utf-8")
        self.next_id = 0
    def call(self, name, arguments):
        self.next_id += 1
        req = {"jsonrpc":"2.0","id":self.next_id,"method":"tools/call",
               "params":{"name":name,"arguments":arguments}}
        self.sock.sendall((json.dumps(req,separators=(",",":"))+"\n").encode())
        while True:
            line = self.f.readline()
            if not line: raise RuntimeError("socket closed")
            resp = json.loads(line)
            if resp.get("id") != self.next_id: continue
            if "error" in resp: raise RuntimeError(resp["error"])
            r = resp["result"]
            if r.get("isError"): raise RuntimeError(r)
            return r.get("structuredContent")

def iso(ts): return datetime.fromtimestamp(ts, timezone.utc).isoformat()

mcp = Mcp(SOCK)
out = {"socket": SOCK, "daily_chunks": [], "yearly_chunks": [], "probe": None,
       "calls": []}

# catalog entities (same as drivers)
versions = mcp.call("list_data_versions", {"dataset":"securities.catalog","contract_version":"v1",
    "selection":{"kind":"catalog","scope":"current_sh_sz_a"},"page_size":10})
catalog_ref = versions["items"][0]["version_ref"]
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
out["entity_count"] = len(entities)
chunks = [entities[i:i+100] for i in range(0, len(entities), 100)]

def avail(dataset, chunk, filter_):
    return mcp.call("get_data_availability", {"dataset":dataset,"contract_version":"v1",
        "selection":{"kind":"batch","entities":chunk},
        "filter":filter_,"version":{"kind":"latest_per_entity"},"page_size":100})

# daily batch availability: first N chunks
for i in range(min(N_DAILY_CHUNKS, len(chunks))):
    t0 = time.time()
    r = avail("market.daily", chunks[i], {"kind":"daily","start_date":"1990-12-19","end_date":"2026-10-08"})
    ms = (time.time()-t0)*1000
    out["daily_chunks"].append({"chunk": i+1, "ms": round(ms,1), "items": len(r.get("items") or [])})
    print(f"daily chunk {i+1}: {ms:.0f}ms items={len(r.get('items') or [])}", flush=True)

# yearly batch availability: first 3 chunks
for i in range(3):
    t0 = time.time()
    r = avail("market.yearly", chunks[i], {"kind":"yearly","start_year":1900,"end_year":2026})
    ms = (time.time()-t0)*1000
    out["yearly_chunks"].append({"chunk": i+1, "ms": round(ms,1), "items": len(r.get("items") or [])})
    print(f"yearly chunk {i+1}: {ms:.0f}ms items={len(r.get('items') or [])}", flush=True)

# resolve_years-style probe on PROBE_CHUNK (1-based): per-year daily availability
pc = chunks[PROBE_CHUNK-1]
t0 = time.time()
daily = avail("market.daily", pc, {"kind":"daily","start_date":"1990-01-01","end_date":"2026-12-31"})
ditems = {}
for it in daily.get("items") or []:
    e = it.get("entity") or {}; k = (e.get("exchange"), e.get("code"))
    if it.get("first_record") and it.get("last_record"):
        ditems[k] = it
ys = [int(i["first_record"][:4]) for i in ditems.values()] + [int(i["last_record"][:4]) for i in ditems.values()]
probe = {"chunk": PROBE_CHUNK, "entities": len(ditems), "year_min": min(ys) if ys else None,
         "year_max": max(ys) if ys else None, "year_calls": []}
t_probe = time.time()
for year in range(min(ys), max(ys)+1):
    t0 = time.time()
    r = avail("market.daily", pc, {"kind":"daily","start_date":f"{year:04d}-01-01","end_date":f"{year:04d}-12-31"})
    probe["year_calls"].append({"year": year, "ms": round((time.time()-t0)*1000,1)})
probe["wall_s"] = round(time.time()-t_probe, 2)
out["probe"] = probe
print(f"probe chunk {PROBE_CHUNK}: {len(probe['year_calls'])} calls wall={probe['wall_s']}s", flush=True)

out["finished_at"] = datetime.now(timezone.utc).isoformat()
json.dump(out, open(OUT, "w"), indent=1)
