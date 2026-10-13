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
import zipfile
import struct
import subprocess

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



import s5_run


class FakeAcq:
    def __init__(self, replies):
        self.replies = replies
        self.calls = []

    def call(self, name, args):
        self.calls.append((name, args))
        r = self.replies[name]
        return r.pop(0) if isinstance(r, list) else r


class AcquireWireTests(unittest.TestCase):
    """Shapes follow src/acquire/wire.rs (state, not status; generation_id on the
    task view; active pointer under `active`)."""

    def replies(self, verify_state="consistency_verified", act_state="active"):
        return {
            "acquire_generation": {"request_id": "r", "task_id": "t1", "state": "accepted", "reused": False},
            "get_acquisition_task": [{"task_id": "t1", "state": "running", "generation_id": None},
                                     {"task_id": "t1", "state": "succeeded", "generation_id": "g1"}],
            "verify_generation": {"generation_id": "g1", "state": verify_state},
            "sync_status": {"active": {"generation_id": "gprev"}},
            "activate_generation": {"generation_id": "g1", "state": act_state, "reason_code": None},
        }

    def test_chain_ok(self):
        acq = FakeAcq(self.replies())
        orig = s5_run.time.sleep
        s5_run.time.sleep = lambda s: None
        try:
            rec = s5_run.acquire_and_activate(acq, "p", "gena")
        finally:
            s5_run.time.sleep = orig
        self.assertEqual(rec["acquire_task"]["generation_id"], "g1")
        act = [a for n, a in acq.calls if n == "activate_generation"][0]
        self.assertEqual(act["expected_previous"], "gprev")

    def test_not_verified_and_refused_raise(self):
        orig = s5_run.time.sleep
        s5_run.time.sleep = lambda s: None
        try:
            with self.assertRaises(RuntimeError):
                s5_run.acquire_and_activate(FakeAcq(self.replies(verify_state="not_verified")), "p", "x")
            with self.assertRaises(RuntimeError):
                s5_run.acquire_and_activate(FakeAcq(self.replies(act_state="refused")), "p", "x")
        finally:
            s5_run.time.sleep = orig


class ResolveYearsTests(unittest.TestCase):
    def test_window_future_and_unresolved(self):
        k = ("sh", "600000")
        d = {k: {"first_record": "2001-03-01", "last_record": "2026-10-08"},
             ("sz", "000001"): {"first_record": "2001-03-01", "last_record": "2026-10-08"},
             ("sz", "000002"): {"first_record": "2005-01-01", "last_record": "2006-01-01"}}
        pres = {"items": [
            {"entity": {"exchange": "sh", "code": "600000"}, "status": "resolved",
             "present_years": [2000, 2001, 2002, 2026, 9999]},
            {"entity": {"exchange": "sz", "code": "000001"}, "status": "unresolved", "reason_code": "x"},
            {"entity": {"exchange": "sz", "code": "000002"}, "status": "resolved", "present_years": [1999]},
            {"entity": {"exchange": "bj", "code": "1"}, "status": "resolved", "present_years": []},
        ]}
        resolved, empty, unavail = s5_run.resolve_years(pres, d)
        self.assertEqual(resolved[k], [2001, 2002, 2026])
        self.assertEqual([e["reason_code"] for e in empty], ["empty_present_years"])
        codes = sorted(u["reason_code"] for u in unavail)
        self.assertEqual(codes, ["future_years_in_response", "unrequested_item", "x"])


class MirrorTests(unittest.TestCase):
    def test_only_declared_roles(self):
        with tempfile.TemporaryDirectory() as src, tempfile.TemporaryDirectory() as mir:
            for n in ("base.zip", "gbbq.zip", "hsjday.zip", "snapshot-manifest.json"):
                open(os.path.join(src, n), "w").write(n)
            open(os.path.join(mir, "stale.zip"), "w").write("x")
            got = s5_run.mirror_inputs(mir, src)
            self.assertEqual(got, ["base.zip", "gbbq.zip", "hsjday.zip"])
            self.assertEqual(sorted(os.listdir(mir)), ["base.zip", "gbbq.zip", "hsjday.zip"])
            os.remove(os.path.join(src, "gbbq.zip"))
            with self.assertRaises(RuntimeError):
                s5_run.mirror_inputs(mir, src)


class TruncKTests(unittest.TestCase):
    def _day(self, dates):
        return b"".join(struct.pack("<I", d) + b"\0" * 28 for d in dates)

    def test_truncate_and_keep(self):
        import make_trunc_k
        with tempfile.TemporaryDirectory() as d:
            src, dst = os.path.join(d, "a.zip"), os.path.join(d, "b.zip")
            with zipfile.ZipFile(src, "w") as z:
                z.writestr("sh\\lday\\sh600000.day", self._day(range(20260901, 20260909)))
                z.writestr("sh\\lday\\sh880312.day", self._day(range(20260901, 20260909)))
                z.writestr("sz\\lday\\sz000001.day", self._day([20260901, 20260902]))
            st = make_trunc_k.truncate(src, dst, 5, ["sh/lday/sh880312.day"])
            with zipfile.ZipFile(dst) as z:
                n = {i.filename: len(z.read(i.filename)) // 32 for i in z.infolist()}
            self.assertEqual(n["sh\\lday\\sh600000.day"], 3)
            self.assertEqual(n["sh\\lday\\sh880312.day"], 8)
            self.assertEqual(n["sz\\lday\\sz000001.day"], 2)
            self.assertEqual((st["truncated"], len(st["day_too_small"])), (1, 1))


class DbSizeTests(unittest.TestCase):
    def test_snapshot_and_missing(self):
        import sqlite3
        with tempfile.TemporaryDirectory() as d:
            db = os.path.join(d, "x.sqlite")
            con = sqlite3.connect(db)
            con.execute("create table t(a)")
            con.executemany("insert into t values(?)", [(i,) for i in range(1000)])
            con.commit()
            con.close()
            snap = s5_common.db_size_snapshot(db)
            self.assertGreater(snap["main_bytes"], 0)
            self.assertEqual(snap["page_size"] * snap["page_count"], snap["main_bytes"])
            miss = s5_common.db_size_snapshot(os.path.join(d, "none.sqlite"))
            self.assertIsNone(miss["main_bytes"])
            self.assertIn("pragma_error", miss)


if __name__ == "__main__":
    unittest.main()
