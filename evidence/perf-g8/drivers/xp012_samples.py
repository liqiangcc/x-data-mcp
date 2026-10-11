import json, socket, sys, time
# XP-012 small-gate driver: serial get_task_progress samples on a pinned
# completed 100-item task. perf_counter around EACH complete
# tools/call request/response; any EOF / JSON-RPC error / tool isError /
# contract violation fails the run instead of recording a sample.
# Per-sample record: task_id, sequence, elapsed_seconds, returned status,
# dataset/operation, total/finished/succeeded/failed, range/yearly result
# counts, and response bytes.
import os as _os, sys as _sys
_sys.path.insert(0, _os.path.dirname(_os.path.abspath(__file__)))
from xp015_common import private_sock
SOCK = private_sock()  # XP-015: env XP015_SOCKET only, under /root/x-perf-g8-xp015/
EXPECT_DATASET = "market.daily"
EXPECT_OPERATION = "update_data"

s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect(SOCK)
f = s.makefile("r", encoding="utf-8")
n = 0


def fail(msg):
    print(f"FAIL: {msg}", file=sys.stderr)
    sys.exit(2)


def call(name, args):
    global n
    n += 1
    s.sendall((json.dumps({"jsonrpc": "2.0", "id": n, "method": "tools/call",
                           "params": {"name": name, "arguments": args}},
                          separators=(",", ":")) + "\n").encode())
    while True:
        line = f.readline()
        if not line:
            fail("EOF on response stream")
        nbytes = len(line.encode())
        resp = json.loads(line)
        if resp.get("id") != n:
            continue
        if resp.get("error") is not None:
            fail(f"JSON-RPC error: {resp['error']}")
        result = resp.get("result")
        if not isinstance(result, dict):
            fail("missing result object")
        if result.get("isError"):
            fail(f"tool error: {result.get('content')}")
        return result.get("structuredContent"), nbytes


tid = sys.argv[1]
reps = int(sys.argv[2]) if len(sys.argv) > 2 else 10
res = []
for i in range(reps):
    t0 = time.perf_counter()
    p, nbytes = call("get_task_progress", {"task_id": tid, "page_size": 100})
    elapsed = time.perf_counter() - t0
    task = p.get("task") or {}
    if task.get("task_id") != tid:
        fail(f"task_id mismatch: {task.get('task_id')!r} != {tid!r}")
    if task.get("dataset") != EXPECT_DATASET:
        fail(f"dataset {task.get('dataset')!r} != {EXPECT_DATASET!r}")
    if task.get("operation") != EXPECT_OPERATION:
        fail(f"operation {task.get('operation')!r} != {EXPECT_OPERATION!r}")
    if task.get("status") != "succeeded":
        fail(f"task status {task.get('status')!r} != 'succeeded'")
    if p.get("finished") != 100 or p.get("total") != 100:
        fail(f"expected 100/100 completed items, "
             f"got finished={p.get('finished')} total={p.get('total')}")
    res.append({
        "task_id": tid,
        "seq": i,
        "elapsed_seconds": round(elapsed, 6),
        "status": task.get("status"),
        "dataset": task.get("dataset"),
        "operation": task.get("operation"),
        "total": p.get("total"),
        "finished": p.get("finished"),
        "succeeded": p.get("succeeded"),
        "failed": p.get("failed"),
        "range_results": len(p.get("range_results") or []),
        "yearly_results": len(p.get("yearly_results") or []),
        "tool_error": None,
        "response_bytes": nbytes,
    })
    print(f"call {i}: {elapsed * 1000:.0f}ms", flush=True)
print(json.dumps(res, indent=1))
