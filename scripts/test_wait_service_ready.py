"""CI-runnable checks for scripts/wait_service_ready.py (TASK-OPS-T1).

Each test binds a real unix socket in a tempdir and speaks newline-framed
JSON-RPC to it, so the probe's retry/validation logic runs unmodified. No
test touches /opt, a systemd unit or a live service.
"""
import json
import os
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SCRIPT = REPO / "scripts" / "wait_service_ready.py"


def reply(payload):
    return (json.dumps(payload) + "\n").encode()


def mcp_handler(name="x-data-mcp", tools=(), raw_line=None):
    """Answer initialize/tools/list like the real services do."""

    def handle(line):
        if raw_line is not None:
            return raw_line
        request = json.loads(line)
        if request["method"] == "initialize":
            return reply(
                {
                    "jsonrpc": "2.0",
                    "id": request["id"],
                    "result": {
                        "protocolVersion": "2025-06-18",
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": name, "version": "test"},
                    },
                }
            )
        if request["method"] == "tools/list":
            return reply(
                {
                    "jsonrpc": "2.0",
                    "id": request["id"],
                    "result": {"tools": [{"name": t} for t in tools]},
                }
            )
        return reply(
            {
                "jsonrpc": "2.0",
                "id": request["id"],
                "error": {"code": -32601, "message": "method_not_found"},
            }
        )

    return handle


class FakeService:
    """A unix socket server that optionally appears late and records methods."""

    def __init__(self, path, handler, delay=0.0):
        self.path = path
        self.handler = handler
        self.delay = delay
        self.methods = []
        self._stop = threading.Event()
        self._thread = None

    def __enter__(self):
        def run():
            if self.delay:
                time.sleep(self.delay)
            server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            try:
                server.bind(str(self.path))
                server.listen(8)
                server.settimeout(0.05)
                while not self._stop.is_set():
                    try:
                        conn, _ = server.accept()
                    except socket.timeout:
                        continue
                    except OSError:
                        break
                    threading.Thread(target=self._serve, args=(conn,), daemon=True).start()
            finally:
                server.close()

        self._thread = threading.Thread(target=run, daemon=True)
        self._thread.start()
        return self

    def _serve(self, conn):
        with conn:
            buffered = b""
            while True:
                while b"\n" not in buffered:
                    chunk = conn.recv(65536)
                    if not chunk:
                        return
                    buffered += chunk
                line, buffered = buffered.split(b"\n", 1)
                self.methods.append(json.loads(line)["method"])
                answer = self.handler(line)
                if answer is None:
                    return
                conn.sendall(answer)

    def __exit__(self, *exc):
        self._stop.set()
        if self._thread is not None:
            self._thread.join(timeout=5)
        if self.path.exists():
            self.path.unlink()


def run_probe(*args, env_extra=None):
    env = dict(os.environ)
    env.pop("XDM_READY_TIMEOUT", None)
    env.update(env_extra or {})
    return subprocess.run(
        [sys.executable, "-B", str(SCRIPT), *args],
        capture_output=True,
        text=True,
        env=env,
        timeout=60,
    )


class WaitServiceReady(unittest.TestCase):
    def socket_path(self, td):
        return Path(td) / "mcp.sock"

    def test_success_reports_json_line(self):
        with tempfile.TemporaryDirectory() as td:
            path = self.socket_path(td)
            handler = mcp_handler(tools=["query_data", "plan_market_update"])
            with FakeService(path, handler) as svc:
                proc = run_probe(str(path), "--timeout", "10",
                                 "--expect-tool", "plan_market_update")
            self.assertEqual(proc.returncode, 0, proc.stderr)
            lines = proc.stdout.strip().splitlines()
            self.assertEqual(len(lines), 1, proc.stdout)
            payload = json.loads(lines[0])
            self.assertEqual(payload["socket"], str(path))
            self.assertIsInstance(payload["elapsed_ms"], int)
            self.assertGreaterEqual(payload["elapsed_ms"], 0)
            self.assertEqual(payload["tool"], "plan_market_update")
            self.assertEqual(svc.methods, ["initialize", "tools/list"])

    def test_success_without_expect_tool_skips_tools_list(self):
        with tempfile.TemporaryDirectory() as td:
            path = self.socket_path(td)
            with FakeService(path, mcp_handler()) as svc:
                proc = run_probe(str(path), "--timeout", "10")
            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertIsNone(json.loads(proc.stdout)["tool"])
            self.assertEqual(svc.methods, ["initialize"])

    def test_waits_for_a_late_socket(self):
        # Regression guard: a socket that appears after the old 10s/20s
        # fixed waits must still be waited for, not declared ready early.
        with tempfile.TemporaryDirectory() as td:
            path = self.socket_path(td)
            with FakeService(path, mcp_handler(), delay=0.6):
                proc = run_probe(str(path), "--timeout", "10")
            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertGreaterEqual(json.loads(proc.stdout)["elapsed_ms"], 400)

    def test_timeout_when_nothing_listens(self):
        with tempfile.TemporaryDirectory() as td:
            path = self.socket_path(td)
            proc = run_probe(str(path), "--timeout", "0.3")
            self.assertEqual(proc.returncode, 1, proc.stdout)
            self.assertIn(str(path), proc.stderr)
            self.assertIn("0.3", proc.stderr)
            self.assertEqual(proc.stdout, "")

    def test_timeout_on_stale_socket_file(self):
        with tempfile.TemporaryDirectory() as td:
            path = self.socket_path(td)
            stale = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            stale.bind(str(path))
            stale.close()  # file remains, nothing listens
            proc = run_probe(str(path), "--timeout", "0.3")
            self.assertEqual(proc.returncode, 1, proc.stdout)
            self.assertIn(str(path), proc.stderr)
            self.assertIn("ConnectionRefusedError", proc.stderr)

    def test_server_name_mismatch_is_a_protocol_error(self):
        with tempfile.TemporaryDirectory() as td:
            path = self.socket_path(td)
            with FakeService(path, mcp_handler(name="x-data-acquire")):
                proc = run_probe(str(path), "--timeout", "10")
            self.assertEqual(proc.returncode, 2, proc.stdout)
            self.assertIn("x-data-acquire", proc.stderr)
            self.assertIn(str(path), proc.stderr)

    def test_invalid_json_is_a_protocol_error(self):
        with tempfile.TemporaryDirectory() as td:
            path = self.socket_path(td)
            with FakeService(path, mcp_handler(raw_line=b"not json\n")):
                proc = run_probe(str(path), "--timeout", "10")
            self.assertEqual(proc.returncode, 2, proc.stdout)
            self.assertIn("not JSON", proc.stderr)
            self.assertIn(str(path), proc.stderr)

    def test_missing_expected_tool_is_a_protocol_error(self):
        with tempfile.TemporaryDirectory() as td:
            path = self.socket_path(td)
            with FakeService(path, mcp_handler(tools=["query_data"])):
                proc = run_probe(str(path), "--timeout", "10",
                                 "--expect-tool", "plan_market_update")
            self.assertEqual(proc.returncode, 2, proc.stdout)
            self.assertIn("plan_market_update", proc.stderr)
            self.assertIn(str(path), proc.stderr)

    def test_timeout_must_be_positive(self):
        with tempfile.TemporaryDirectory() as td:
            path = self.socket_path(td)
            for value in ("0", "-1", "soon"):
                proc = run_probe(str(path), "--timeout", value)
                self.assertEqual(proc.returncode, 2, (value, proc.stdout, proc.stderr))
                self.assertIn(str(path), proc.stderr)
            self.assertIn("soon", run_probe(str(path), "--timeout", "soon").stderr)

    def test_env_timeout_override(self):
        with tempfile.TemporaryDirectory() as td:
            path = self.socket_path(td)
            proc = run_probe(str(path), env_extra={"XDM_READY_TIMEOUT": "0.3"})
            self.assertEqual(proc.returncode, 1, proc.stdout)
            self.assertIn("0.3", proc.stderr)
            proc = run_probe(str(path), env_extra={"XDM_READY_TIMEOUT": "0"})
            self.assertEqual(proc.returncode, 2, proc.stdout)
            # An explicit --timeout wins over the environment.
            with FakeService(path, mcp_handler()):
                proc = run_probe(str(path), "--timeout", "10",
                                 env_extra={"XDM_READY_TIMEOUT": "0"})
            self.assertEqual(proc.returncode, 0, proc.stderr)

    def test_missing_socket_argument_is_usage_error(self):
        proc = run_probe()
        self.assertEqual(proc.returncode, 2)
        self.assertIn("socket", proc.stderr)


if __name__ == "__main__":
    unittest.main()
