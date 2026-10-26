#!/usr/bin/env python3
"""Single definition of "the service is ready" for deployment and tunnel startup.

Readiness here means the unix socket accepts a connection and answers the MCP
protocol: ``initialize`` returns ``result.serverInfo.name == "x-data-mcp"`` and,
when ``--expect-tool`` is given, ``tools/list`` advertises that tool. A socket
file existing on disk is *not* readiness: ``data_service`` binds the endpoint
only after ``registry_sweep`` + ``series_startup_check``, whose cost grows with
task/sequence history (2026-10-23 cold start was 31s).

Usage: wait_service_ready.py <socket> [--timeout SECONDS] [--expect-tool NAME]

Environment: ``XDM_READY_TIMEOUT`` supplies the timeout when ``--timeout`` is
absent. The default is 60s; a timeout must be > 0.

Exit codes: 0 ready, 1 timeout, 2 protocol or argument error.
On success stdout carries one line of JSON:
    {"socket": "...", "elapsed_ms": 1234, "tool": "plan_market_update"}
``tool`` is the expected tool name, or null when ``--expect-tool`` was omitted.
Errors go to stderr and always name the socket and the timeout.

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
    """One connection attempt; returns the expected tool name (or None)."""
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
            return None
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
        return expect_tool


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


def main(argv=None):
    parser = argparse.ArgumentParser(
        prog="wait_service_ready.py",
        description="Wait until an x-data-mcp unix socket answers MCP initialize "
        "(and advertises --expect-tool, when given).",
    )
    parser.add_argument("socket", help="unix socket path of the service endpoint")
    parser.add_argument(
        "--timeout",
        default=None,
        metavar="SECONDS",
        help="overall wait budget (default 60s, env XDM_READY_TIMEOUT overrides)",
    )
    parser.add_argument(
        "--expect-tool",
        dest="expect_tool",
        default=None,
        metavar="NAME",
        help="also require tools/list to advertise NAME",
    )
    args = parser.parse_args(argv)

    raw = args.timeout if args.timeout is not None else os.environ.get("XDM_READY_TIMEOUT")
    if raw is None or raw == "":
        timeout = DEFAULT_TIMEOUT
    else:
        try:
            timeout = float(raw)
        except ValueError:
            return _fail(2, args.socket, raw, "timeout is not a number: %r" % (raw,))
    if not timeout > 0:
        return _fail(2, args.socket, timeout, "timeout must be > 0")

    try:
        tool, elapsed = wait_until_ready(args.socket, timeout, args.expect_tool)
    except ProtocolError as e:
        return _fail(2, args.socket, timeout, "protocol error: %s" % e)
    except NotReady as e:
        return _fail(1, args.socket, timeout, "not ready: %s" % e)
    print(json.dumps({"socket": args.socket, "elapsed_ms": int(round(elapsed * 1000)), "tool": tool}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
