#!/usr/bin/env python3
"""Unit tests for prod_rebuild_v6.py — no socket, no services."""
import os
import sys
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import prod_rebuild_v6 as drv  # noqa: E402


class Whitelist(unittest.TestCase):
    def test_write_tools_are_exactly_collection_and_update(self):
        self.assertEqual(drv.ALLOWED_WRITE_TOOLS, {"start_collection", "update_data"})

    def test_forbidden_covers_acquisition_and_out_of_path_mutations(self):
        for name in (
            "check_official_updates", "acquire_generation", "verify_generation",
            "activate_generation", "retire_generation", "get_acquisition_task",
            "sync_status", "republish_data", "backfill_data", "cancel_task",
            "run_worker_once", "inspect_verification_task",
            "verify_publication_result", "verify_stopped_writes", "verify_data_gap",
        ):
            self.assertIn(name, drv.FORBIDDEN_TOOLS, name)

    def test_sets_are_disjoint(self):
        self.assertFalse(drv.ALLOWED_WRITE_TOOLS & drv.ALLOWED_READ_TOOLS)
        self.assertFalse(drv.FORBIDDEN_TOOLS & (drv.ALLOWED_WRITE_TOOLS | drv.ALLOWED_READ_TOOLS))


class GuardedMcpTest(unittest.TestCase):
    def _inner(self):
        inner = mock.Mock()
        inner.call.return_value = {"ok": True}
        return inner

    def test_whitelisted_calls_pass_through(self):
        inner = self._inner()
        g = drv.GuardedMcp(inner)
        for name in ("start_collection", "update_data", "query_data",
                     "get_data_capabilities", "get_data_availability"):
            g.call(name, {})
        self.assertEqual(inner.call.call_count, 5)

    def test_forbidden_and_unknown_calls_refused(self):
        inner = self._inner()
        g = drv.GuardedMcp(inner)
        for name in ("acquire_generation", "republish_data", "run_worker_once",
                     "drop_everything"):
            with self.assertRaises(RuntimeError, msg=name):
                g.call(name, {})
        inner.call.assert_not_called()


class EnvGate(unittest.TestCase):
    ENV_KEYS = ("PROD_REBUILD_V6_SOCKET", "PROD_REBUILD_V6_DB",
                "PROD_REBUILD_V6_OUT", "PROD_REBUILD_V6_PRODUCTION")

    def setUp(self):
        self._saved = {k: os.environ.get(k) for k in self.ENV_KEYS}

    def tearDown(self):
        for k, v in self._saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v

    def _env(self, sock="/root/x-s5-stack/r5/run/mcp.sock", db="/root/x-s5-stack/r5/db/preview.sqlite",
             out="/root/x-s5-stack/out-rehearsal", prod=None):
        os.environ["PROD_REBUILD_V6_SOCKET"] = sock
        os.environ["PROD_REBUILD_V6_DB"] = db
        os.environ["PROD_REBUILD_V6_OUT"] = out
        if prod is None:
            os.environ.pop("PROD_REBUILD_V6_PRODUCTION", None)
        else:
            os.environ["PROD_REBUILD_V6_PRODUCTION"] = prod

    def test_missing_envs_refused(self):
        for k in self.ENV_KEYS[:3]:
            os.environ.pop(k, None)
        with self.assertRaises(SystemExit):
            drv.resolve_env()

    def test_rehearsal_socket_accepted(self):
        self._env()
        env = drv.resolve_env()
        self.assertEqual(env["mode"], "rehearsal")
        self.assertTrue(os.path.exists(env["out"]))

    def test_production_socket_needs_explicit_flag(self):
        self._env(sock=drv.PROD_SOCKET, db=drv.PROD_DB, out="/root/x-s5-stack/out-prod")
        with self.assertRaises(SystemExit):
            drv.resolve_env()
        os.environ["PROD_REBUILD_V6_PRODUCTION"] = "0"
        with self.assertRaises(SystemExit):
            drv.resolve_env()
        os.environ["PROD_REBUILD_V6_PRODUCTION"] = "1"
        env = drv.resolve_env()
        self.assertEqual(env["mode"], "production")

    def test_dotdot_refused(self):
        self._env(sock="/root/x-s5-stack/../x-s5-stack/r5/run/mcp.sock")
        with self.assertRaises(SystemExit):
            drv.resolve_env()

    def test_tmp_out_refused(self):
        self._env(out="/tmp/out")
        with self.assertRaises(SystemExit):
            drv.resolve_env()

    def test_production_db_must_be_canonical(self):
        self._env(sock=drv.PROD_SOCKET, db="/root/x-s5-stack/r5/db/preview.sqlite",
                  out="/root/x-s5-stack/out-prod", prod="1")
        with self.assertRaises(SystemExit):
            drv.resolve_env()


class Expectations(unittest.TestCase):
    def test_expected_table(self):
        self.assertEqual(drv.EXPECTED["catalog_entities"], 5228)
        self.assertEqual(drv.EXPECTED["daily_published"], 5224)
        self.assertEqual(drv.EXPECTED["yearly_published"], 5224)
        self.assertEqual(sorted(drv.EXPECTED["daily_absent"]),
                         ["sz.001381", "sz.301569", "sz.301660", "sz.301718"])
        self.assertEqual(drv.EXPECTED["user_version"], 21)
        self.assertEqual(drv.EXPECTED["staged_records"], 0)


class Preflight(unittest.TestCase):
    def _mcp(self, catalog_items=None, avail_items=None, cap_over=None):
        caps = {"capability_revision": "generic-data-v6",
                "production_ready": False,
                "items": [{"tool_name": f"t{i}"} for i in range(21)]
                + [{"tool_name": "t0", "dataset": "market.yearly"}]}
        if cap_over:
            caps.update(cap_over)
        def call(name, args):
            if name == "get_data_capabilities":
                return caps
            if name == "list_data_versions":
                return {"items": catalog_items if catalog_items is not None else []}
            if name == "get_data_availability":
                return {"items": avail_items if avail_items is not None else
                        [{"entity": dict(e), "status": "absent", "version_ref": None}
                         for e in drv.PROBE_ENTITIES]}
            raise AssertionError(name)
        inner = mock.Mock()
        inner.call.side_effect = call
        return drv.GuardedMcp(inner)

    def test_ok_on_empty_target(self):
        res = drv.preflight(self._mcp())
        self.assertTrue(res["ok"])
        self.assertTrue(all(res["checks"].values()))

    def test_refuses_existing_catalog_version(self):
        res = drv.preflight(self._mcp(catalog_items=[{"version_id": "v"}]))
        self.assertFalse(res["ok"])
        self.assertFalse(res["checks"]["catalog_versions_empty"])

    def test_refuses_existing_daily_head(self):
        res = drv.preflight(self._mcp(avail_items=[
            {"entity": {"exchange": "sh", "code": "600000"},
             "status": "present", "version_ref": {"version_id": "v"}}]))
        self.assertFalse(res["ok"])
        self.assertFalse(res["checks"]["daily_heads_empty"])

    def test_refuses_wrong_capability(self):
        res = drv.preflight(self._mcp(cap_over={"capability_revision": "generic-data-v5"}))
        self.assertFalse(res["ok"])
        self.assertFalse(res["checks"]["capability_revision_expected"])

    def test_refuses_wrong_tool_count(self):
        res = drv.preflight(self._mcp(cap_over={"items": []}))
        self.assertFalse(res["ok"])
        self.assertFalse(res["checks"]["data_tools_expected"])

    def test_v7_expectations_via_env(self):
        env = {"PROD_REBUILD_V6_EXPECT_CAPABILITY": "generic-data-v7",
               "PROD_REBUILD_V6_EXPECT_DATA_TOOLS": "23"}
        caps = {"capability_revision": "generic-data-v7",
                "items": [{"tool_name": f"t{i}"} for i in range(23)]}
        with mock.patch.dict("os.environ", env):
            res = drv.preflight(self._mcp(cap_over=caps))
        self.assertTrue(res["ok"], res["checks"])
        self.assertEqual(res["expected"]["capability_revision"], "generic-data-v7")
        self.assertEqual(res["expected"]["data_tools"], 23)

    def test_v7_defaults_refused(self):
        # Without the env override a v7 service must still fail the gate.
        caps = {"capability_revision": "generic-data-v7",
                "items": [{"tool_name": f"t{i}"} for i in range(23)]}
        res = drv.preflight(self._mcp(cap_over=caps))
        self.assertFalse(res["ok"])
        self.assertFalse(res["checks"]["capability_revision_expected"])
        self.assertFalse(res["checks"]["data_tools_expected"])


if __name__ == "__main__":
    unittest.main()
