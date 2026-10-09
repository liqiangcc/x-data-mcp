#!/usr/bin/env python3
# XP-010 A: idle get_task_progress latency samples on completed update tasks.
import json, socket, sys, time
from datetime import datetime, timezone
SOCK = "/root/x-perf-g8/svc/run/mcp.sock"
OUT = sys.argv[1]
TASKS = sys.argv[2].split(",")          # comma-separated task_ids
SAMPLES = int(sys.argv[3]) if len(sys.argv) > 3 else 12

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

mcp = Mcp(SOCK)
out = {"socket": SOCK, "page_size": 100, "samples": []}
for i in range(SAMPLES):
    tid = TASKS[i % len(TASKS)]
    t0 = time.time()
    p = mcp.call("get_task_progress", {"task_id": tid, "page_size": 100})
    ms = (time.time() - t0) * 1000
    out["samples"].append({"i": i, "task_id": tid, "ms": round(ms, 1),
        "status": p["task"]["status"], "total": p.get("total"),
        "range_results": len(p.get("range_results") or []),
        "publications": len(p.get("publication_versions") or [])})
    print(f"sample {i}: {ms:.0f}ms task={tid[:16]} status={p['task']['status']}", flush=True)
out["finished_at"] = datetime.now(timezone.utc).isoformat()
json.dump(out, open(OUT, "w"), indent=1)
