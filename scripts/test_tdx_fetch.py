#!/usr/bin/env python3
"""Tests for `scripts/tdx_fetch.py` (#625 progress reporting).

The helper is the only thing the acquire service shells out to, so its
stdout contract is what the service's `bytes_done` depends on: `head` prints
the head object; `fetch` prints one `{"bytes": N}` line per landed chunk and
the result object last.

The fixture is a local HTTP server on 127.0.0.1 with a wall-clock pause
between chunks, so the test can prove the progress lines leave the helper
*while the download is still running* rather than all at the end. No outbound
network.
"""
import hashlib
import json
import os
import subprocess
import sys
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

SCRIPT = Path(__file__).with_name("tdx_fetch.py")

# The helper reads 1 MiB at a time (`CHUNK`), so the payload must span several
# chunks for more than one progress line to appear.
PAYLOAD = bytes(range(256)) * (3 * 4096)  # 3 MiB
CHUNK = 1 << 20
PAUSE = 0.15


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):  # keep the test output quiet
        pass

    def _declared(self):
        """Only `/base.zip` exists; anything else is a 404 like the feed."""
        if self.path == "/base.zip":
            return True
        self.send_error(404)
        return False

    def do_HEAD(self):
        if not self._declared():
            return
        self.send_response(200)
        self.send_header("Content-Length", str(len(PAYLOAD)))
        self.send_header("ETag", '"fixture"')
        self.end_headers()

    def do_GET(self):
        if not self._declared():
            return
        self.send_response(200)
        self.send_header("Content-Length", str(len(PAYLOAD)))
        self.send_header("ETag", '"fixture"')
        self.end_headers()
        for start in range(0, len(PAYLOAD), CHUNK):
            self.wfile.write(PAYLOAD[start : start + CHUNK])
            self.wfile.flush()
            time.sleep(PAUSE)


class HelperProgress(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        cls.url = f"http://127.0.0.1:{cls.server.server_address[1]}/base.zip"
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()

    def setUp(self):
        import tempfile

        self.dir = tempfile.TemporaryDirectory()
        self.dest = Path(self.dir.name) / "base.zip"

    def tearDown(self):
        self.dir.cleanup()

    def test_head_prints_one_object(self):
        result = subprocess.run(
            [sys.executable, str(SCRIPT), "head", self.url],
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        head = json.loads(result.stdout)
        self.assertEqual(head["status"], 200)
        self.assertEqual(head["content_length"], len(PAYLOAD))
        self.assertEqual(head["etag"], '"fixture"')

    def test_fetch_reports_progress_while_the_download_runs(self):
        proc = subprocess.Popen(
            [sys.executable, str(SCRIPT), "fetch", self.url, str(self.dest)],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        lines = []
        seen_before_exit = 0
        # Read stdout with the child still alive: this is what the service
        # does, and it is the property the old `output()` call broke.
        for line in proc.stdout:
            line = line.strip()
            if not line:
                continue
            payload = json.loads(line)
            if "sha256" in payload:
                lines.append(payload)
                break
            seen_before_exit += 1
            self.assertIsNone(proc.poll(), "a progress line arrived after the exit")
            lines.append(payload)
        stderr = proc.stderr.read()
        self.assertEqual(proc.wait(), 0, stderr)
        proc.stdout.close()
        proc.stderr.close()

        progress = [line for line in lines if "sha256" not in line]
        self.assertGreaterEqual(
            len(progress),
            3,
            f"one progress line per landed chunk, got {lines}",
        )
        self.assertEqual(
            [line["bytes"] for line in progress],
            sorted(line["bytes"] for line in progress),
            "progress must be monotonic",
        )
        self.assertGreater(seen_before_exit, 0, "no progress during the download")
        result = lines[-1]
        self.assertEqual(result["bytes"], len(PAYLOAD))
        self.assertEqual(result["sha256"], hashlib.sha256(PAYLOAD).hexdigest())
        self.assertEqual(result["content_length"], len(PAYLOAD))
        self.assertEqual(self.dest.read_bytes(), PAYLOAD)

    def test_a_failed_fetch_prints_no_result_object(self):
        destination = Path(self.dir.name) / "missing.zip"
        result = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "fetch",
                self.url.rsplit("/", 1)[0] + "/nope.zip",
                str(destination),
            ],
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("404", result.stderr)
        self.assertNotIn('"sha256"', result.stdout)
        self.assertFalse(destination.exists())

    def test_a_truncated_body_fails_closed(self):
        # The head promises more than the server sends: the size check must
        # reject it rather than publish a short file.
        short = len(PAYLOAD) + 4096

        class Truncating(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *args):
                pass

            def do_HEAD(self):
                self.send_response(200)
                self.send_header("Content-Length", str(short))
                self.end_headers()

            def do_GET(self):
                self.send_response(200)
                self.send_header("Content-Length", str(short))
                self.end_headers()
                self.wfile.write(PAYLOAD)
                self.wfile.flush()
                self.close_connection = True

        server = ThreadingHTTPServer(("127.0.0.1", 0), Truncating)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        try:
            destination = Path(self.dir.name) / "short.zip"
            result = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "fetch",
                    f"http://127.0.0.1:{server.server_address[1]}/hsjday.zip",
                    str(destination),
                ],
                capture_output=True,
                text=True,
            )
        finally:
            server.shutdown()
            server.server_close()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("size mismatch", result.stderr)
        self.assertFalse(destination.exists(), "a rejected fetch leaves no file")


if __name__ == "__main__":
    unittest.main()
