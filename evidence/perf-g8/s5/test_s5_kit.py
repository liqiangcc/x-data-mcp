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


import watch_s5


class WatchdogTerminateTests(unittest.TestCase):
    """watch_s5.terminate must really stop the run — the 2026-10-13 r5
    detached run showed a bare SIGINT is a no-op on a process launched as
    a shell background job (SIGINT inherited SIG_IGN)."""

    def _spawn(self, code):
        return subprocess.Popen([sys.executable, "-c", code],
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def test_sigint_ignoring_process_is_stopped(self):
        # POSIX background-launch disposition: SIGINT ignored -> the watchdog
        # must escalate; SIGTERM's default disposition still kills.
        p = subprocess.Popen(
            [sys.executable, "-c",
             "import signal,sys,time;"
             "signal.signal(signal.SIGINT,signal.SIG_IGN);"
             "sys.stdout.write('ready\\n');sys.stdout.flush();time.sleep(60)"],
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        try:
            self.assertEqual(p.stdout.readline(), b"ready\n")  # SIG_IGN installed
            t0 = time.time()
            level = watch_s5.terminate(p.pid)
            self.assertIn(level, ("sigterm", "sigkill"))
            self.assertFalse(watch_s5._alive(p.pid))
            self.assertLess(time.time() - t0, 20)
        finally:
            if watch_s5._alive(p.pid):
                p.kill()
        p.wait()

    def test_polite_process_stops_at_sigint(self):
        # Default SIGINT disposition (what s5_run now installs explicitly)
        # raises KeyboardInterrupt -> no escalation needed.
        p = self._spawn("import time;time.sleep(60)")
        try:
            level = watch_s5.terminate(p.pid)
            self.assertEqual(level, "sigint")
            self.assertFalse(watch_s5._alive(p.pid))
        finally:
            if watch_s5._alive(p.pid):
                p.kill()
        p.wait()

    def test_descendants_finds_children(self):
        child = self._spawn("import time;time.sleep(60)")
        parent = self._spawn(
            "import subprocess,sys,time;"
            "subprocess.Popen([sys.executable,'-c','import time;time.sleep(60)']);"
            "time.sleep(60)")
        try:
            kids = []
            for _ in range(50):  # grandchild fork is async; poll briefly
                kids = watch_s5.descendants(parent.pid)
                if kids:
                    break
                time.sleep(0.1)
            self.assertEqual(len(kids), 1)
            self.assertNotIn(child.pid, kids)
        finally:
            for p in (parent, child):
                if watch_s5._alive(p.pid):
                    p.kill()
                p.wait()
            for k in kids:  # grandchild is reparented when parent dies
                try:
                    os.kill(k, 9)
                except OSError:
                    pass

    def test_pattern_pids_matches_cmdline_only(self):
        marker = "s5watch-marker-9f3b"
        p = self._spawn("import time;time.sleep(60) # " + marker)
        try:
            self.assertEqual(watch_s5.pattern_pids(marker), [p.pid])
            self.assertEqual(watch_s5.pattern_pids(marker, exclude={p.pid}), [])
        finally:
            if watch_s5._alive(p.pid):
                p.kill()
            p.wait()


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
