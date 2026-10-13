#!/usr/bin/env python3
"""Unit tests for prod_append_v6.py — no socket, no services."""
import os
import sys
import unittest
from datetime import date
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import prod_append_v6 as drv  # noqa: E402


def _head(key, last, vid="v1"):
    ex, code = key.split(".")
    return {"entity": {"exchange": ex, "code": code},
            "version_ref": {"version_id": vid}, "last_record": last,
            "first_record": "2000-01-04"}


class Whitelist(unittest.TestCase):
    def test_write_tools_are_exactly_update(self):
        self.assertEqual(drv.ALLOWED_WRITE_TOOLS, {"update_data"})

    def test_forbidden_covers_required_set(self):
        for name in ("check_official_updates", "acquire_generation",
                     "verify_generation", "activate_generation",
                     "retire_generation", "get_acquisition_task",
                     "get_acquisition_request", "sync_status",
                     "start_collection", "republish_data", "backfill_data",
                     "cancel_task", "run_worker_once",
                     "inspect_verification_task", "verify_stopped_writes",
                     "verify_data_gap"):
            self.assertIn(name, drv.FORBIDDEN_TOOLS, name)

    def test_sets_are_disjoint(self):
        self.assertFalse(drv.ALLOWED_WRITE_TOOLS & drv.ALLOWED_READ_TOOLS)
        self.assertFalse(
            drv.FORBIDDEN_TOOLS & (drv.ALLOWED_WRITE_TOOLS | drv.ALLOWED_READ_TOOLS))


class GuardedMcpTest(unittest.TestCase):
    def test_forbidden_and_unknown_calls_refused(self):
        inner = mock.Mock()
        g = drv.GuardedMcp(inner)
        for name in ("start_collection", "acquire_generation", "republish_data",
                     "backfill_data", "run_worker_once", "bogus"):
            with self.assertRaises(RuntimeError, msg=name):
                g.call(name, {})
        inner.call.assert_not_called()

    def test_whitelisted_calls_pass(self):
        inner = mock.Mock(); inner.call.return_value = {}
        g = drv.GuardedMcp(inner)
        for name in ("update_data", "get_data_availability", "get_request",
                     "list_tasks", "verify_publication_result"):
            g.call(name, {})
        self.assertEqual(inner.call.call_count, 5)


class EnvGate(unittest.TestCase):
    KEYS = ("PROD_APPEND_V6_SOCKET", "PROD_APPEND_V6_DB", "PROD_APPEND_V6_OUT",
            "PROD_APPEND_V6_PRODUCTION", "PROD_APPEND_V6_END_DATE")

    def setUp(self):
        self._saved = {k: os.environ.get(k) for k in self.KEYS}
        for k in self.KEYS:
            os.environ.pop(k, None)

    def tearDown(self):
        for k, v in self._saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v

    def _env(self, sock="/root/x-s5-stack/rv6/run/mcp.sock",
             db="/root/x-s5-stack/rv6/db/preview.sqlite",
             out="/root/x-s5-stack/out-append", prod=None,
             end="2026-10-12"):
        os.environ["PROD_APPEND_V6_SOCKET"] = sock
        os.environ["PROD_APPEND_V6_DB"] = db
        os.environ["PROD_APPEND_V6_OUT"] = out
        os.environ["PROD_APPEND_V6_END_DATE"] = end
        if prod is None:
            os.environ.pop("PROD_APPEND_V6_PRODUCTION", None)
        else:
            os.environ["PROD_APPEND_V6_PRODUCTION"] = prod

    def test_missing_envs_refused(self):
        for k in self.KEYS:
            os.environ.pop(k, None)
        with self.assertRaises(SystemExit):
            drv.resolve_env()

    def test_rehearsal_accepted(self):
        self._env()
        env = drv.resolve_env()
        self.assertEqual(env["mode"], "rehearsal")
        self.assertEqual(env["end_date"], date(2026, 10, 12))

    def test_production_needs_flag_and_canonical_db(self):
        self._env(sock=drv.PROD_SOCKET, db=drv.PROD_DB,
                  out="/root/x-s5-stack/out-append")
        with self.assertRaises(SystemExit):
            drv.resolve_env()
        os.environ["PROD_APPEND_V6_PRODUCTION"] = "1"
        self.assertEqual(drv.resolve_env()["mode"], "production")
        self._env(sock=drv.PROD_SOCKET, db="/root/other.sqlite", prod="1")
        with self.assertRaises(SystemExit):
            drv.resolve_env()

    def test_dotdot_and_tmp_refused(self):
        self._env(sock="/root/x/../x/mcp.sock")
        with self.assertRaises(SystemExit):
            drv.resolve_env()
        self._env(out="/tmp/o")
        with self.assertRaises(SystemExit):
            drv.resolve_env()

    def test_bad_end_date_refused(self):
        self._env(end="not-a-date")
        with self.assertRaises(SystemExit):
            drv.resolve_env()


class AppendItems(unittest.TestCase):
    def test_lagging_gets_item_current_skipped(self):
        heads = {
            "sh.600000": _head("sh.600000", "2026-10-08"),
            "sz.000001": _head("sz.000001", "2026-10-12"),  # already current
        }
        items = drv.build_append_items(heads, date(2026, 10, 12))
        self.assertEqual(len(items), 1)
        it = items[0]
        self.assertEqual(it["entity"], {"exchange": "sh", "code": "600000"})
        self.assertEqual(it["base_version_id"], "v1")
        self.assertEqual(it["start_date"], "2026-10-09")
        self.assertEqual(it["end_date"], "2026-10-12")

    def test_idempotent_rerun_produces_nothing(self):
        heads = {"sh.600000": _head("sh.600000", "2026-10-12")}
        self.assertEqual(drv.build_append_items(heads, date(2026, 10, 12)), [])


class YearlySet(unittest.TestCase):
    def _y(self, ex, code, stale=None, ref="v", reason=None):
        it = {"entity": {"exchange": ex, "code": code}, "version_ref": {"version_id": ref} if ref else None}
        if stale:
            it["stale"] = stale
        if reason:
            it["reason"] = reason
        return it

    def test_behind_absent_and_invalid_reasons_need_rederive(self):
        items = [self._y("sh", "1", stale="behind"),
                 self._y("sh", "2", ref=None),
                 self._y("sh", "3", reason="daily_replaced_since_derivation"),
                 self._y("sh", "4", reason="value_semantics_changed"),
                 self._y("sh", "5")]  # fresh -> not needed
        self.assertEqual(drv.yearly_needs_rederive(items),
                         {("sh", "1"), ("sh", "2"), ("sh", "3"), ("sh", "4")})


class SubmitRecovery(unittest.TestCase):
    def _g(self, update_exc=None, view=None, wait_ret=None):
        inner = mock.Mock()
        calls = {}

        def call(name, args):
            if name == "update_data":
                if update_exc:
                    raise update_exc
                return {"task_id": "t1"}
            if name == "get_request":
                return view
            raise AssertionError(name)
        inner.call.side_effect = call
        return drv.GuardedMcp(inner)

    def test_bound_recovery_adopts_task(self):
        g = self._g(update_exc=RuntimeError("request_binding_lost"),
                    view={"bound": True, "task": {"task_id": "t-orig"}})
        with mock.patch.object(drv, "wait_task", return_value=({"task": {"status": "succeeded"}}, {"terminal_seen": 1, "polls": 1, "first_progress_seen": 0})):
            rec = drv.submit_and_wait(g, "rid-1", [])
        self.assertEqual(rec["task_id"], "t-orig")

    def test_unbound_failure_stops_never_resends(self):
        g = self._g(update_exc=RuntimeError("timeout"),
                    view={"bound": False, "task": None, "terminal_result": None})
        with self.assertRaises(RuntimeError) as cm:
            drv.submit_and_wait(g, "rid-2", [])
        self.assertIn("never resent", str(cm.exception))

    def test_terminal_recovery_returns_result(self):
        g = self._g(update_exc=RuntimeError("eof"),
                    view={"bound": False, "terminal_result": {"code": "rejected"}})
        rec = drv.submit_and_wait(g, "rid-3", [])
        self.assertEqual(rec["recovered_terminal"], {"code": "rejected"})


class PreflightChecks(unittest.TestCase):
    def _mcp(self, caps=None, tasks=None, heads=None, absent=None):
        caps = caps or {"capability_revision": "generic-data-v6",
                        "production_ready": False,
                        "items": [{"tool_name": f"t{i}", "active_generation_ref": "g1"}
                                  if i == 0 else {"tool_name": f"t{i}"}
                                  for i in range(21)]}
        def call(name, args):
            if name == "get_data_capabilities":
                return caps
            if name == "list_tasks":
                return {"items": tasks or []}
            raise AssertionError(name)
        inner = mock.Mock(); inner.call.side_effect = call
        g = drv.GuardedMcp(inner)
        return g

    def _run_preflight(self, g, absent_keys, expect=None):
        heads = {f"sh.{i:06d}": _head(f"sh.{i:06d}", "2026-10-08") for i in range(1, 4)}
        absent = [{"entity": {"exchange": k.split(".")[0], "code": k.split(".")[1]}}
                  for k in absent_keys]
        with mock.patch.object(drv.s5_run, "daily_heads", return_value=(heads, absent)):
            return drv.preflight(g, [], date(2026, 10, 12), expect or {})

    def test_ok_when_clean(self):
        res = self._run_preflight(self._mcp(), ["sz.301718"],
                                {"PROD_APPEND_V6_EXPECT_DAILY_ABSENT": "sz.301718"})
        self.assertTrue(res["ok"], res["checks"])
        self.assertEqual(res["active_generation_ref"], "g1")

    def test_inflight_tasks_block(self):
        g = self._mcp(tasks=[{"task_id": "t9", "status": "running"}])
        res = self._run_preflight(g, [])
        self.assertFalse(res["ok"])
        self.assertFalse(res["checks"]["no_inflight_tasks"])

    def test_unexpected_absent_blocks(self):
        res = self._run_preflight(self._mcp(), ["sz.999999"],
                                {"PROD_APPEND_V6_EXPECT_DAILY_ABSENT": "sz.301718"})
        self.assertFalse(res["checks"]["heads_present_except_allowed_absent"])

    def test_generation_mismatch_blocks(self):
        res = self._run_preflight(self._mcp(), [],
                                {"PROD_APPEND_V6_EXPECT_GENERATION": "other-gen"})
        self.assertFalse(res["checks"]["generation_matches_expect"])

    def test_v5_capability_blocks(self):
        g = self._mcp(caps={"capability_revision": "generic-data-v5",
                            "production_ready": False, "items": []})
        res = self._run_preflight(g, [])
        self.assertFalse(res["checks"]["capability_revision_v6"])
        self.assertFalse(res["checks"]["data_tools_21"])


class ExpectedAbsent(unittest.TestCase):
    def test_parse(self):
        self.assertEqual(drv.expected_absent(
            {"PROD_APPEND_V6_EXPECT_DAILY_ABSENT": "sz.a, sz.b ,, sz.c"}),
            {"sz.a", "sz.b", "sz.c"})
        self.assertEqual(drv.expected_absent({}), set())


if __name__ == "__main__":
    unittest.main()
