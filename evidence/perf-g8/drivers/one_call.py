import json, socket, sys, time
# XP-011/012 driver (rev.2, round-2 rework): perf_counter timing; any EOF,
# JSON-RPC error or tool-level isError fails the run; a sample is recorded
# only after the response validates as the pinned completed 100-item task.
SOCK = "/root/x-perf-g8/svc/run/mcp.sock"

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
        return result.get("structuredContent")


tid = sys.argv[1]
reps = int(sys.argv[2]) if len(sys.argv) > 2 else 1
res = []
for i in range(reps):
    t0 = time.perf_counter()
    p = call("get_task_progress", {"task_id": tid, "page_size": 100})
    ms = (time.perf_counter() - t0) * 1000
    # An error response is not a fast progress success: validate the
    # pinned completed-task shape before recording the sample.
    task = p.get("task") or {}
    if task.get("task_id") != tid:
        fail(f"task_id mismatch: {task.get('task_id')!r} != {tid!r}")
    if task.get("status") != "succeeded":
        fail(f"task status {task.get('status')!r} != 'succeeded'")
    if p.get("finished") != 100 or p.get("total") != 100:
        fail(f"expected 100/100 completed items, "
             f"got finished={p.get('finished')} total={p.get('total')}")
    res.append({"i": i, "ms": round(ms, 1)})
    print(f"call {i}: {ms:.0f}ms", flush=True)
print(json.dumps(res))
