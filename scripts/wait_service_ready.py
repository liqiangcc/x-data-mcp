#!/usr/bin/env python3
"""Single definition of "the service is ready" for deployment and tunnel startup.

Readiness here means the unix socket accepts a connection and answers the MCP
protocol: ``initialize`` returns ``result.serverInfo.name == "x-data-mcp"`` and,
when an entry or ``--expect-tool`` is given, ``tools/list`` advertises that
tool. A socket file existing on disk is *not* readiness: ``data_service`` binds
the endpoint only after ``registry_sweep`` + ``series_startup_check``, whose
cost grows with task/sequence history (2026-10-23 cold start was 31s).

One data unit can serve several sockets (v8 phase 2, D-56): ``--endpoint
ENTRY=SOCKET`` may be repeated, and each entry has a default expected tool
(``research`` -> ``query_data``, ``ops`` -> ``plan_market_update``,
``maint`` -> ``compute_metrics``). The old single-socket form stays valid.

Usage: wait_service_ready.py <socket> [--timeout SECONDS]
                             [--entry research|ops|maint] [--expect-tool NAME]
       wait_service_ready.py --endpoint ENTRY=SOCKET [--endpoint ...]
                             [--timeout SECONDS]

Environment: ``XDM_READY_TIMEOUT`` supplies the timeout when ``--timeout`` is
absent. The default is 60s; a timeout must be > 0. For several endpoints the
budget applies to each socket in turn (the total wait can reach N x timeout).

Exit codes: 0 ready, 1 timeout, 2 protocol or argument error.
On success stdout carries one line of JSON per endpoint, in argument order:
    {"socket": "...", "entry": "ops", "elapsed_ms": 1234,
     "tool": "plan_market_update", "tools": 15}
``tool`` is the expected tool name, or null when no tool check was requested;
``tools`` is the number of tools ``tools/list`` advertised (null when the check
was skipped). Errors go to stderr and always name the socket and the timeout.

This probe is read-only: it writes no files, creates no tasks and infers no
task state.
"""
import argparse
import json
import os
import socket
import sys
import time

SERVER_NAME = "x-data-mcp"
DEFAULT_TIMEOUT = 60.0
# Per-attempt I/O budget. A stalled attempt is retried until the overall
# deadline instead of consuming the whole wait.
ATTEMPT_TIMEOUT = 5.0
POLL_INTERVAL = 0.1
MAX_FRAME_BYTES = 8 * 1024 * 1024
# Default readiness tool per data entry (v8 phase 2 §3): each socket proves the
# surface it is meant to serve, not just that a data server answered.
ENTRY_TOOLS = {
    "research": "query_data",
    "ops": "plan_market_update",
    "maint": "compute_metrics",
}


class ProtocolError(Exception):
    """The peer answered, but not with the expected MCP contract."""


class TransportError(Exception):
    """The connection ended before a complete response; retry until deadline."""


class NotReady(Exception):
    """The deadline passed without a valid readiness answer."""


def _describe(path, timeout):
    if isinstance(timeout, float):
        return "socket=%s timeout=%gs" % (path, timeout)
    return "socket=%s timeout=%s" % (path, timeout)


def _exchange(conn, request_id, method, params, deadline):
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise NotReady("deadline reached before %s" % method)
    conn.settimeout(max(0.001, min(ATTEMPT_TIMEOUT, remaining)))
    request = {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}
    conn.sendall(json.dumps(request).encode() + b"\n")
    frame = bytearray()
    while b"\n" not in frame:
        chunk = conn.recv(65536)
        if not chunk:
            raise TransportError("connection closed before a complete %s response" % method)
        frame += chunk
        if len(frame) > MAX_FRAME_BYTES:
            raise ProtocolError("%s response exceeds %d bytes" % (method, MAX_FRAME_BYTES))
    line = bytes(frame).split(b"\n", 1)[0]
    try:
        response = json.loads(line)
    except json.JSONDecodeError as e:
        raise ProtocolError("%s response is not JSON (%s): %r" % (method, e, line[:200]))
    if not isinstance(response, dict) or response.get("id") != request_id:
        raise ProtocolError("%s response envelope mismatch: %r" % (method, line[:200]))
    if "error" in response:
        raise ProtocolError("%s returned JSON-RPC error: %s" % (method, response["error"]))
    return response.get("result")


def _attempt(path, expect_tool, deadline):
    """One connection attempt; returns {tool, tools} for the readiness answer."""
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as conn:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise NotReady("deadline reached before connect")
        conn.settimeout(max(0.001, min(ATTEMPT_TIMEOUT, remaining)))
        conn.connect(path)
        result = _exchange(conn, 1, "initialize", {}, deadline)
        info = result.get("serverInfo") if isinstance(result, dict) else None
        name = info.get("name") if isinstance(info, dict) else None
        if name != SERVER_NAME:
            raise ProtocolError(
                "initialize serverInfo.name is %r, expected %r" % (name, SERVER_NAME)
            )
        if expect_tool is None:
            return {"tool": None, "tools": None}
        result = _exchange(conn, 2, "tools/list", {}, deadline)
        tools = result.get("tools") if isinstance(result, dict) else None
        if not isinstance(tools, list):
            raise ProtocolError("tools/list result has no tools array")
        names = {t.get("name") for t in tools if isinstance(t, dict)}
        if expect_tool not in names:
            raise ProtocolError(
                "tools/list lacks required tool %r (%d tools advertised)"
                % (expect_tool, len(names))
            )
        return {"tool": expect_tool, "tools": len(names)}


def wait_until_ready(path, timeout, expect_tool):
    """Poll the socket until it answers the readiness contract or time runs out."""
    start = time.monotonic()
    deadline = start + timeout
    last = "no attempt completed"
    while True:
        if time.monotonic() >= deadline:
            raise NotReady(last)
        try:
            return _attempt(path, expect_tool, deadline), time.monotonic() - start
        except (TransportError, NotReady) as e:
            last = str(e)
        except OSError as e:
            last = "%s: %s" % (type(e).__name__, e)
        if time.monotonic() >= deadline:
            raise NotReady(last)
        time.sleep(min(POLL_INTERVAL, max(0.0, deadline - time.monotonic())))


def _fail(code, path, timeout, detail):
    sys.stderr.write("wait_service_ready: %s (%s)\n" % (detail, _describe(path, timeout)))
    return code


def _parse_endpoint(spec):
    entry, sep, path = spec.partition("=")
    if not sep or not entry or not path:
        raise ValueError("--endpoint wants ENTRY=SOCKET, got %r" % spec)
    if entry not in ENTRY_TOOLS:
        raise ValueError(
            "unknown --endpoint entry %r (%s)" % (entry, "|".join(ENTRY_TOOLS))
        )
    return entry, path


def _plan(args):
    """[(entry, socket, expected_tool)] from the CLI, validating the modes."""
    if args.endpoint:
        if args.socket is not None:
            raise ValueError("give either <socket> or --endpoint, not both")
        if args.entry is not None or args.expect_tool is not None:
            raise ValueError("--entry/--expect-tool apply to the single-socket form only")
        plan = []
        seen_paths = set()
        seen_entries = set()
        for spec in args.endpoint:
            entry, path = _parse_endpoint(spec)
            if path in seen_paths:
                raise ValueError("socket %r is probed twice" % path)
            if entry in seen_entries:
                raise ValueError("entry %r is probed twice" % entry)
            seen_paths.add(path)
            seen_entries.add(entry)
            plan.append((entry, path, ENTRY_TOOLS[entry]))
        return plan
    if args.socket is None:
        raise ValueError("a socket path (or --endpoint ENTRY=SOCKET) is required")
    expected = args.expect_tool
    if expected is None and args.entry is not None:
        expected = ENTRY_TOOLS[args.entry]
    return [(args.entry, args.socket, expected)]


def main(argv=None):
    parser = argparse.ArgumentParser(
        prog="wait_service_ready.py",
        description="Wait until one or more x-data-mcp unix sockets answer MCP "
        "initialize (and advertise the entry's tool, when known).",
    )
    parser.add_argument(
        "socket", nargs="?", help="unix socket path of the service endpoint"
    )
    parser.add_argument(
        "--endpoint",
        action="append",
        default=[],
        metavar="ENTRY=SOCKET",
        help="repeatable multi-socket form; entry picks the expected tool "
        "(research|ops|maint)",
    )
    parser.add_argument(
        "--timeout",
        default=None,
        metavar="SECONDS",
        help="overall wait budget per socket (default 60s, env XDM_READY_TIMEOUT)",
    )
    parser.add_argument(
        "--entry",
        choices=tuple(ENTRY_TOOLS),
        default=None,
        help="single-socket form: expected tool defaults to this entry's tool",
    )
    parser.add_argument(
        "--expect-tool",
        dest="expect_tool",
        default=None,
        metavar="NAME",
        help="also require tools/list to advertise NAME (overrides --entry)",
    )
    args = parser.parse_args(argv)

    try:
        plan = _plan(args)
    except ValueError as e:
        sys.stderr.write("wait_service_ready: %s\n" % e)
        return 2

    raw = args.timeout if args.timeout is not None else os.environ.get("XDM_READY_TIMEOUT")
    if raw is None or raw == "":
        timeout = DEFAULT_TIMEOUT
    else:
        try:
            timeout = float(raw)
        except ValueError:
            return _fail(2, plan[0][1], raw, "timeout is not a number: %r" % (raw,))
    if not timeout > 0:
        return _fail(2, plan[0][1], timeout, "timeout must be > 0")

    answers = []
    for entry, path, expect_tool in plan:
        try:
            result, elapsed = wait_until_ready(path, timeout, expect_tool)
        except ProtocolError as e:
            return _fail(2, path, timeout, "protocol error: %s" % e)
        except NotReady as e:
            return _fail(1, path, timeout, "not ready: %s" % e)
        answers.append(
            {
                "socket": path,
                "entry": entry,
                "elapsed_ms": int(round(elapsed * 1000)),
                "tool": result["tool"],
                "tools": result["tools"],
            }
        )
    for answer in answers:
        print(json.dumps(answer))
    return 0


if __name__ == "__main__":
    sys.exit(main())
