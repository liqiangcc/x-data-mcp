"""S5 acceptance-kit shared helpers (measurement tooling, no product code).

Path discipline (TASK-PAR-1 B): every socket/db/work path the kit touches is
resolved with realpath and rejected when it sits under a forbidden root —
the production deploy dir or any /root/x-perf-g8* fixture tree. The check
runs on BOTH the given path and its realpath so `..` segments and symlinked
parents cannot smuggle a forbidden target past the guard.

Reuses `classify_range_results`/`POLL_S` from the XP-015 drivers
(sys.path is extended at call site, not here).
"""
from __future__ import annotations

import json
import os
import socket
import sys
import time
from datetime import datetime, timezone

# Forbidden roots for every path the kit resolves (glob semantics of
# "/root/x-perf-g8*" — the prefix itself matches all sibling trees such as
# x-perf-g8-xp015 or x-perf-g8-deploy31).
FORBIDDEN_ROOTS = ("/opt/x-data-mcp-preview", "/root/x-perf-g8", "/root/x-data-mcp-owner")

POLL_S = 0.25  # same polling granularity as the XP-015 drivers
TERMINAL = {
    "succeeded",
    "partially_failed",
    "failed",
    "cancelled",
    "needs_verification",
    "interrupted",
}


def forbidden_hit(path: str) -> str | None:
    """Return the forbidden root `path` resolves under, or None."""
    for root in FORBIDDEN_ROOTS:
        if path == root or path.startswith(root):
            return root
    return None


def require_safe_path(path: str, kind: str = "path") -> str:
    """Reject paths inside forbidden roots. Returns the realpath."""
    if not path:
        sys.exit(f"{kind}: empty path")
    real = os.path.realpath(path)
    hit = forbidden_hit(real) or forbidden_hit(os.path.abspath(path))
    if hit:
        sys.exit(f"refusing {kind} {path!r}: resolves under forbidden root {hit}")
    return real


def guard_paths(named: dict) -> dict:
    """Apply require_safe_path to a {name: path} mapping; returns realpaths."""
    return {name: require_safe_path(path, name) for name, path in named.items()}


def iso(ts: float) -> str:
    return datetime.fromtimestamp(ts, timezone.utc).isoformat()


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat()


def write_json(path: str, obj) -> None:
    tmp = f"{path}.tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(obj, f, ensure_ascii=False, indent=1, sort_keys=True)
        f.write("\n")
    os.replace(tmp, path)


class Mcp:
    """Line-delimited JSON-RPC over a unix socket (same frames as the XP-015
    drivers' inline clients). One call at a time; ids are per connection."""

    def __init__(self, path: str):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(path)
        self.f = self.sock.makefile("r", encoding="utf-8")
        self.next_id = 0

    def call(self, name: str, arguments: dict):
        self.next_id += 1
        rid = self.next_id
        req = {
            "jsonrpc": "2.0",
            "id": rid,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        }
        self.sock.sendall((json.dumps(req, separators=(",", ":")) + "\n").encode())
        while True:
            line = self.f.readline()
            if not line:
                raise RuntimeError(f"{name} EOF")
            resp = json.loads(line)
            if resp.get("id") != rid:
                continue
            if "error" in resp:
                raise RuntimeError(f"{name} rpc {resp['error']}")
            r = resp["result"]
            if r.get("isError"):
                raise RuntimeError(
                    f"{name} tool {json.dumps(r.get('structuredContent') or r.get('content'))[:400]}"
                )
            return r.get("structuredContent") or {}

    def close(self) -> None:
        try:
            self.f.close()
        finally:
            self.sock.close()


def wait_task(client: Mcp, task_id: str, timeout_s: float = 1800.0):
    """Poll get_task_progress until a terminal status. Returns
    (progress, meta) where meta carries polls/first_seen/terminal_seen."""
    t0 = time.time()
    polls = 0
    first_seen = None
    last = None
    while time.time() - t0 < timeout_s:
        tq = time.time()
        progress = client.call("get_task_progress", {"task_id": task_id, "page_size": 1000})
        polls += 1
        if first_seen is None:
            first_seen = tq
        status = (progress.get("task") or {}).get("status")
        last = status
        if status in TERMINAL:
            return progress, {"polls": polls, "first_progress_seen": first_seen,
                              "terminal_seen": time.time()}
        time.sleep(POLL_S)
    raise RuntimeError(f"task {task_id} timeout last_status={last}")


def server_window(task: dict):
    """created_at -> last_progress_at seconds, or None when unparseable."""
    try:
        ca = datetime.fromisoformat(task["created_at"]).timestamp()
        lp = datetime.fromisoformat(task["last_progress_at"]).timestamp()
        return lp - ca
    except Exception:
        return None


def chunked(items, size: int):
    return [items[i : i + size] for i in range(0, len(items), size)]


class Phase:
    """write-loop timing record (XP-015 口径): monotonic clock from just
    before the first write submission to the last task terminal observed.
    Wall-clock endpoints are recorded alongside for the report."""

    def __init__(self, name: str):
        self.name = name
        self.m0 = None
        self.w0 = None
        self.tasks = []

    def start(self) -> None:
        self.m0 = time.monotonic()
        self.w0 = time.time()

    def record_task(self, entry: dict) -> None:
        self.tasks.append(entry)

    def finish(self) -> dict:
        return {
            "phase": self.name,
            "write_loop_mono_s": time.monotonic() - self.m0,
            "write_loop_wall_clock": {"start": iso(self.w0), "end": iso(time.time())},
            "tasks": self.tasks,
        }


def merge_outcome_totals(per_task: list) -> dict:
    """Sum the per-task outcome dicts (considered/published/no_change/
    failed/needs_verification/unknown). unknown is never folded into
    success — it is a distinct counter by contract."""
    totals = {"considered": 0, "published": 0, "no_change": 0, "failed": 0,
              "needs_verification": 0, "unknown": 0}
    for t in per_task:
        out = t.get("outcomes") or {}
        for k in totals:
            totals[k] += int(out.get(k) or 0)
    return totals
