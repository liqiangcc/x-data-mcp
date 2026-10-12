#!/usr/bin/env python3
"""Unit tests for the S5 kit — no service, no socket, no network.

Covers the forbidden-root guard (incl. `..`/symlink bypass), the
write-loop Phase timing record, and the outcome merge used by the
phase JSONs. Run: `python3 -B -m unittest discover -s evidence/perf-g8/s5 -v`.
"""
import os
import sys
import tempfile
import time
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import s5_common
from s5_common import (
    FORBIDDEN_ROOTS,
    Phase,
    chunked,
    forbidden_hit,
    merge_outcome_totals,
    require_safe_path,
)


class ForbiddenRootTests(unittest.TestCase):
    def test_forbidden_hit(self):
        for root in FORBIDDEN_ROOTS:
            self.assertEqual(forbidden_hit(root), root)
            self.assertEqual(forbidden_hit(root + "/sub/dir"), root)
        # glob semantics: /root/x-perf-g8* covers sibling trees
        self.assertEqual(forbidden_hit("/root/x-perf-g8-xp015/run/mcp.sock"),
                         "/root/x-perf-g8")
        self.assertEqual(forbidden_hit("/root/x-perf-g8-deploy31/db/x.sqlite"),
                         "/root/x-perf-g8")
        self.assertIsNone(forbidden_hit("/root/s5-private/run/mcp.sock"))
        self.assertIsNone(forbidden_hit("/opt/other/dir"))
        # /opt/x-data-mcp-preview-extended also refused (prefix match)
        self.assertEqual(forbidden_hit("/opt/x-data-mcp-preview-old/db"),
                         "/opt/x-data-mcp-preview")

    def test_require_safe_path_exits(self):
        with self.assertRaises(SystemExit):
            require_safe_path("/root/x-perf-g8/svc/run/mcp.sock", "sock")
        with self.assertRaises(SystemExit):
            require_safe_path("/opt/x-data-mcp-preview/run/mcp.sock", "sock")
        with self.assertRaises(SystemExit):
            require_safe_path("", "sock")

    def test_dotdot_bypass_rejected(self):
        with self.assertRaises(SystemExit):
            require_safe_path("/root/safe/../x-perf-g8/run/mcp.sock", "sock")

    def test_symlink_parent_rejected(self):
        with tempfile.TemporaryDirectory(dir="/root") as td:
            link = os.path.join(td, "link")
            os.symlink("/root/x-perf-g8", link)
            with self.assertRaises(SystemExit):
                require_safe_path(os.path.join(link, "sock"), "sock")

    def test_safe_path_returns_realpath(self):
        with tempfile.TemporaryDirectory(dir="/root") as td:
            p = os.path.join(td, "a", "..")
            self.assertEqual(require_safe_path(p, "d"), os.path.realpath(p))


class TimingTests(unittest.TestCase):
    def test_phase_write_loop_fields(self):
        ph = Phase("t")
        ph.start()
        ph.record_task({"task_id": "t1", "wall_s": 0.0, "outcomes": {}})
        time.sleep(0.01)
        out = ph.finish()
        self.assertEqual(out["phase"], "t")
        self.assertGreaterEqual(out["write_loop_mono_s"], 0.01)
        self.assertIn("start", out["write_loop_wall_clock"])
        self.assertIn("end", out["write_loop_wall_clock"])
        self.assertEqual(len(out["tasks"]), 1)

    def test_chunked(self):
        self.assertEqual(chunked(list(range(205)), 100),
                         [list(range(100)), list(range(100, 200)), list(range(200, 205))])
        self.assertEqual(chunked([], 100), [])


class MergeOutcomeTests(unittest.TestCase):
    def test_merge_totals(self):
        tasks = [
            {"outcomes": {"considered": 100, "published": 99, "no_change": 0,
                          "failed": 1, "needs_verification": 0, "unknown": 0}},
            {"outcomes": {"considered": 50, "published": 0, "no_change": 48,
                          "failed": 0, "needs_verification": 1, "unknown": 1}},
            {"outcomes": None},
        ]
        t = merge_outcome_totals(tasks)
        self.assertEqual(t["considered"], 150)
        self.assertEqual(t["published"], 99)
        self.assertEqual(t["no_change"], 48)
        self.assertEqual(t["needs_verification"], 1)
        # unknown stays a distinct counter, never folded into success
        self.assertEqual(t["unknown"], 1)
        self.assertEqual(t["failed"], 1)


class ClassifyReuseTests(unittest.TestCase):
    """The XP-015 outcome classifier is imported, not re-implemented."""

    def test_classify_range_results(self):
        sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                        "..", "drivers"))
        from xp015_common import classify_range_results

        out = classify_range_results({
            "total": 3,
            "failed": 0,
            "task": {"status": "succeeded"},
            "range_results": [{"no_change": True}, {"no_change": False}, {}],
        })
        self.assertEqual(out["published"], 1)
        self.assertEqual(out["no_change"], 1)
        self.assertEqual(out["unknown"], 1)


if __name__ == "__main__":
    unittest.main()
