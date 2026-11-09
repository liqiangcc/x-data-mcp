#!/usr/bin/env python3
"""Tests for `scripts/tdx_fetch.py`'s #625 progress delegation.

The helper is the only thing the acquire service shells out to, so its
contract is: `head` prints the head object; `fetch` prints one `{"bytes": N}`
line per landed chunk (when a native streaming helper answers) and the result
object last; without `X_DATA_MCP_TDX_FETCH_NATIVE` nothing changes.

No network: the "native" helper is a small script the test writes, so the
delegation, the progress relay and the byte/digest re-check are exercised
exactly as the service does.
"""
import json
import os
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("tdx_fetch.py")


def run_helper(args, env=None):
    merged = {k: v for k, v in os.environ.items() if k != "X_DATA_MCP_TDX_FETCH_NATIVE"}
    merged.update(env or {})
    return subprocess.run(
        [sys.executable, str(SCRIPT)] + list(args),
        capture_output=True,
        text=True,
        env=merged,
    )


class NativeDelegation(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.root = Path(self.dir.name)
        self.payload = bytes(range(256)) * 3

    def tearDown(self):
        self.dir.cleanup()

    def write_native(self, body):
        path = self.root / "native.py"
        path.write_text("#!/usr/bin/env python3\n" + textwrap.dedent(body), encoding="utf-8")
        path.chmod(0o755)
        return path

    def test_without_the_variable_the_helper_runs_as_before(self):
        # `head` never delegates: the variable must not change that path, so a
        # URL the local machine cannot reach still fails inside the helper's
        # own urllib code rather than in the delegation.
        result = run_helper(
            ["head", "https://127.0.0.1:9/never-listening"],
            {"X_DATA_MCP_TDX_FETCH_NATIVE": "/nonexistent/native"},
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("_head", result.stderr, "the helper's own head path ran")
        self.assertNotIn("nonexistent", result.stderr)

    def test_progress_lines_are_relayed_and_the_result_is_validated(self):
        payload = self.payload
        dest = self.root / "base.zip"
        native = self.write_native(
            f"""
            import hashlib, json, sys
            payload = bytes(range(256)) * 3
            url, dest = sys.argv[1], sys.argv[2]
            with open(dest, "wb") as out:
                for index in range(0, len(payload), 128):
                    out.write(payload[index:index + 128])
                    print(json.dumps({{"bytes": min(index + 128, len(payload))}}), flush=True)
            print(json.dumps({{"status": 200, "etag": '"abc"', "last_modified": None,
                              "content_length": len(payload), "bytes": len(payload),
                              "sha256": hashlib.sha256(payload).hexdigest()}}), flush=True)
            """
        )
        result = run_helper(
            ["fetch", "https://data.tdx.com.cn/vipdoc/base.zip", str(dest)],
            {"X_DATA_MCP_TDX_FETCH_NATIVE": str(native)},
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        lines = [json.loads(line) for line in result.stdout.splitlines() if line.strip()]
        self.assertEqual(len(lines), 7, lines)
        self.assertTrue(all(set(line) == {"bytes"} for line in lines[:-1]), lines)
        self.assertEqual([line["bytes"] for line in lines[:-1]], sorted(line["bytes"] for line in lines[:-1]))
        self.assertEqual(lines[-1]["bytes"], len(payload))
        self.assertEqual(lines[-1]["sha256"], __import__("hashlib").sha256(payload).hexdigest())
        self.assertEqual(dest.read_bytes(), payload)

    def test_a_mismatched_digest_fails_closed(self):
        dest = self.root / "base.zip"
        native = self.write_native(
            """
            import json, sys
            payload = bytes(range(256)) * 3
            with open(sys.argv[2], "wb") as out:
                out.write(payload)
            print(json.dumps({"status": 200, "etag": None, "last_modified": None,
                              "content_length": len(payload), "bytes": len(payload),
                              "sha256": "0" * 64}), flush=True)
            """
        )
        result = run_helper(
            ["fetch", "https://data.tdx.com.cn/vipdoc/base.zip", str(dest)],
            {"X_DATA_MCP_TDX_FETCH_NATIVE": str(native)},
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("native result mismatch", result.stderr)
        # The helper prints no result object, so the service never records the
        # bad bytes as a fetched file.
        self.assertNotIn('"sha256"', result.stdout)

    def test_a_failing_native_helper_fails_the_fetch(self):
        dest = self.root / "base.zip"
        native = self.write_native(
            """
            import sys
            sys.stderr.write("upstream refused\\n")
            sys.exit(3)
            """
        )
        result = run_helper(
            ["fetch", "https://data.tdx.com.cn/vipdoc/base.zip", str(dest)],
            {"X_DATA_MCP_TDX_FETCH_NATIVE": str(native)},
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("upstream refused", result.stderr)

    def test_a_missing_result_line_is_an_error(self):
        dest = self.root / "base.zip"
        native = self.write_native(
            """
            import json, sys
            with open(sys.argv[2], "wb") as out:
                out.write(b"x")
            print(json.dumps({"bytes": 1}), flush=True)
            """
        )
        result = run_helper(
            ["fetch", "https://data.tdx.com.cn/vipdoc/base.zip", str(dest)],
            {"X_DATA_MCP_TDX_FETCH_NATIVE": str(native)},
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no result line", result.stderr)


if __name__ == "__main__":
    unittest.main()
