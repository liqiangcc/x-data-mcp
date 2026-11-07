#!/usr/bin/env python3
"""Tests for check_daily_run.py on small in-memory fixtures (no real data).

The fixture schema is built by running the repository's own
`migrations/tasks/*.sql`, so table and column names cannot drift from the
product. Foreign keys are not enforced (SQLite default), which lets the
fixtures skip evidence/binding rows the checker never reads.
"""
from __future__ import annotations

import contextlib
import io
import json
import sqlite3
import sys
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_daily_run as cdr  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
SINCE = datetime(2026, 11, 5, 8, 0, tzinfo=timezone.utc)
TARGET = "2026-11-05"
TARGET_DAY = (datetime(2026, 11, 5).date() - datetime(1970, 1, 1).date()).days
OLD_DAY = TARGET_DAY - 1
GEN_NEW = "c642468f" + "0" * 56
GEN_OLD = "4c17de16" + "0" * 56


def stamp(minutes: float) -> str:
    """Rust-style RFC 3339 with nanoseconds, as the product writes it."""
    return (SINCE + timedelta(minutes=minutes)).strftime("%Y-%m-%dT%H:%M:%S.%f") + "000Z"


def digest(n: int) -> bytes:
    return n.to_bytes(32, "big")


class Fixture:
    def __init__(self) -> None:
        self.conn = sqlite3.connect(":memory:")
        for path in sorted((ROOT / "migrations" / "tasks").glob("*.sql")):
            self.conn.executescript(path.read_text(encoding="utf-8"))
        self.acquire = sqlite3.connect(":memory:")
        self.acquire.executescript(
            "CREATE TABLE generations(generation_id TEXT PRIMARY KEY,state TEXT NOT NULL,"
            "manifest_sha256 TEXT NOT NULL,consistency_status TEXT,installed_at TEXT NOT NULL);"
            "CREATE TABLE seen(role TEXT PRIMARY KEY,etag TEXT,last_modified TEXT,"
            "content_length INTEGER,seen_at TEXT NOT NULL);"
        )
        self.n = 0
        self.members: list[str] = []

    def nxt(self) -> int:
        self.n += 1
        return self.n

    # -- market facts --------------------------------------------------
    def member(self, entity: str, version: str = "cat-1") -> None:
        exchange, code = entity.split(".")
        self.conn.execute("INSERT OR IGNORE INTO catalog_versions VALUES(?,?,?,?,?,?)",
                          (version, "t", "s", "t", "{}", "d"))
        self.conn.execute("INSERT INTO catalog_members VALUES(?,?,?,?,?,?,?,?,?)",
                          (version, exchange, code, "n", "id", "assigned", None, None, None))
        self.conn.execute("INSERT OR REPLACE INTO catalog_head VALUES(1,?)", (version,))
        self.members.append(entity)

    def series(self, entity: str, period: int, adjustment: int, klass: int, last_day: int,
               seq: int = 1, dig: int | None = None) -> int:
        d = digest(dig if dig is not None else self.nxt())
        cur = self.conn.execute(
            "INSERT INTO market_series(entity,period,adjustment,instrument_class,price_scale,version_id,"
            "content_digest,digest_format,record_count,first_day,last_day,coverage_status,"
            "source_binding_digest,evidence_digest,last_event_seq,created_at,updated_at) "
            "VALUES(?,?,?,?,?,lower(hex(?)),?,'series-fold-v2',5,?,?,1,?,?,?,'t','t')",
            (entity, period, adjustment, klass, 2 if klass == 0 else 3, d, d,
             last_day - 4, last_day, d, d, seq))
        return cur.lastrowid

    def stock(self, entity: str, last_day: int = TARGET_DAY, yearly: str = "current") -> None:
        """Member + forward daily series with its event chain.
        yearly: current | missing | behind (append after base) | replaced | semantics."""
        self.member(entity)
        first = self.event(entity, "first_publish")
        head = first if yearly in ("current", "missing") else self.event(
            entity, {"behind": "append", "replaced": "replace", "semantics": "append"}[yearly],
            semantics=1 if yearly == "semantics" else 0)
        dig = self.nxt()
        self.series(entity, 1, 1, 0, last_day, seq=head, dig=dig)
        if yearly == "missing":
            return
        key = self.series(entity, 2, 1, 0, last_day, seq=first)
        self.conn.execute(
            "INSERT INTO yearly_series_lineage VALUES(?,?,?,1,0,'a',?,?,?)",
            (key, digest(dig if yearly == "current" else 900 + dig), first, last_day - 4, last_day, last_day))

    def event(self, entity: str, kind: str, semantics: int = 0) -> int:
        seq = self.nxt()
        self.conn.execute(
            "INSERT INTO market_events(seq,entity,period,adjustment,kind,outcome,metadata_changed,"
            "semantics_changed,new_content_digest,old_content_digest,evidence_digest,task_id,item_id,"
            "task_revision,result_id,result_digest,created_at) VALUES(?,?,1,1,?,2,0,?,?,?,?,?,?,0,?,?,'t')",
            (seq, entity, kind, semantics, digest(seq), None if kind == "first_publish" else digest(seq + 5000),
             digest(1), f"ev-task-{seq}", "i", f"r{seq}", digest(1)))
        return seq

    def etf_series(self, entity: str, last_day: int) -> None:
        self.series(entity, 1, 0, 2, last_day)

    def etf_member(self, entity: str, listing: int | None = None) -> None:
        exchange, code = entity.split(".")
        self.conn.execute(
            "INSERT INTO security_master(exchange,code,effective_from,effective_to,instrument_class,"
            "issuer_key,issuer_resolution,listing_date,listing_source,delisting_source,"
            "delisting_reason_class,listing_conflict,delisting_conflict,source_binding_digest,"
            "evidence_digest) VALUES(?,?,0,NULL,2,NULL,'code_date',?,'exchange_list','unknown',"
            "'unknown',0,0,?,?)",
            (exchange, code, listing, digest(1), digest(1)))

    # -- tasks ---------------------------------------------------------
    def task(self, operation: str, dataset: str, cv: str, minutes: tuple[float, float],
             items: list[dict], status: str = "succeeded", evidence: dict | None = None) -> str:
        tid = f"task-{self.nxt():032x}"
        ok = sum(1 for i in items if i["success"])
        frozen = json.dumps({"items": [], "scopes": [], "shared_evidence": evidence})
        self.conn.execute(
            "INSERT INTO tasks(task_id,command_key_json,status,state_revision,accepted_run_id,created_at,"
            "last_progress_at,frozen_work_json,item_count,items_pending,items_running,items_finished,"
            "items_succeeded) VALUES(?,?,?,1,'run',?,?,?,?,0,0,?,?)",
            (tid, json.dumps({"operation": operation, "dataset": dataset, "contract_version": cv}),
             status, stamp(minutes[0]), stamp(minutes[1]), frozen, len(items), len(items), ok))
        for item in items:
            outcome = {"success": item["success"], "error": item.get("error")}
            self.conn.execute(
                "INSERT INTO task_items VALUES(?,?,?,?,?,?, 'finished', ?)",
                (tid, item["id"], json.dumps({"symbol": item.get("symbol"), "period": "day"}),
                 "security_period", json.dumps(item.get("frozen", {})), "[]", json.dumps(outcome)))
        return tid

    def a_share_run(self, entities: list[str], failed: dict[str, str] | None = None,
                    minutes: tuple[float, float] = (30, 33)) -> str:
        failed = failed or {}
        items = [{"id": f'["market","{e}","day"]', "symbol": e, "success": e not in failed,
                  "error": failed.get(e)} for e in entities]
        items += [{"id": "derived:calendar", "success": True}, {"id": "derived:index:sh.000001", "success": True}]
        evidence = {"plan_id": "p" * 64, "target_day": TARGET,
                    "counts": {"eligible": len(entities), "already_current": 0, "absent": 0, "ineligible": 0}}
        return self.task("update_data", "market.daily", cdr.PLAN_RUN_CV, minutes, items,
                         "partially_failed" if failed else "succeeded", evidence)

    def backfill(self, entities: list[str], minutes=(34, 35), status="succeeded") -> str:
        items = [{"id": f'["market","{e}","day"]', "symbol": e, "success": True,
                  "frozen": {"range": {"start": "2026-11-04", "end": TARGET}}} for e in entities]
        return self.task("update_data", "market.daily", cdr.EXPLICIT_DAILY_CV, minutes, items, status)

    def etf_run(self, entities: list[str], minutes=(36, 37)) -> str:
        items = [{"id": f'["market","{e}","day"]', "symbol": e, "success": True,
                  "frozen": {"series": "etf_raw"}} for e in entities]
        return self.task("update_data", "market.daily", cdr.PLAN_RUN_CV, minutes, items, "succeeded",
                         {"plan_id": "e" * 64, "target_day": TARGET, "counts": {"eligible": len(entities)}})

    def yearly_run(self, entities: list[str], reasons: list[dict] | None = None, minutes=(38, 41)) -> str:
        items = [{"id": f'["market","{e}","year"]', "symbol": e, "success": True} for e in entities]
        return self.task("update_data", "market.yearly", cdr.PLAN_RUN_CV, minutes, items, "succeeded",
                         {"plan_id": "y" * 64, "target_day": TARGET, "counts": {"eligible": len(entities)},
                          "ineligible_reasons": reasons or []})

    def republish(self, entities: list[str], minutes=(42, 43)) -> str:
        items = [{"id": f'["market","{e}","year"]', "symbol": e, "success": True} for e in entities]
        return self.task("update_data", "market.yearly", cdr.EXPLICIT_YEARLY_CV, minutes, items)

    def tail(self, minutes: float = 10, status: str = "succeeded", failed_windows: int = 0) -> None:
        self.conn.execute(
            "INSERT INTO source_refresh_runs(run_id,mode,status,scope,started_at,finished_at,requests,"
            "failed_windows) VALUES(?, 'tail', ?, 'all', ?, ?, 5, ?)",
            (f"run-{self.nxt()}", status, stamp(minutes), stamp(minutes + 1), failed_windows))

    def seen_day(self, minutes: float, etag: str) -> None:
        self.acquire.execute("INSERT OR REPLACE INTO seen VALUES('day',?,NULL,NULL,?)", (etag, stamp(minutes)))

    def generation(self, state: str, gid: str) -> None:
        self.acquire.execute("INSERT INTO generations VALUES(?,?,?,?,?)",
                             (gid, state, "m", "VERIFIED_COMPATIBLE", "t"))

    archive_members: set[str] | None = None

    def check(self, activated: bool = True, session: cdr.Session | None = None) -> dict:
        active = {"generation_id": GEN_NEW, "previous_generation_id": GEN_OLD,
                  "activated_at": stamp(5 if activated else -600)}
        return cdr.run_checks(self.conn, SINCE, active, {"etag": '"abc"', "bytes": 1}, self.acquire, session,
                              self.archive_members)


def full_day(fx: Fixture) -> None:
    """A complete, correct day: one revision failure, backfilled; ETF batch; yearly run."""
    for entity in ("sh.600000", "sz.000001", "sz.000002"):
        fx.stock(entity)
    fx.stock("sh.600001", last_day=OLD_DAY)  # the revision-unverified entity, healed below
    fx.generation("active", GEN_NEW)
    fx.generation("superseded", GEN_OLD)
    fx.generation("evidence_only", "e" * 64)
    fx.tail()
    fx.a_share_run(["sh.600000", "sz.000001", "sz.000002", "sh.600001"],
                   failed={"sh.600001": "RevisionUnverified: history rewritten"})
    fx.backfill(["sh.600001"])
    fx.conn.execute("UPDATE market_series SET last_day=? WHERE entity='sh.600001' AND period=1", (TARGET_DAY,))
    fx.etf_series("sh.510300", TARGET_DAY)
    fx.etf_member("sh.510300")
    fx.etf_run(["sh.510300"])
    fx.yearly_run(["sh.600000", "sz.000001"],
                  [{"reason": "daily_replaced_since_derivation", "count": 1}])
    fx.republish(["sh.600001"])


def by_id(report: dict) -> dict:
    return {s["id"]: s for s in report["steps"]}


class CompleteDay(unittest.TestCase):
    def test_all_steps_pass(self) -> None:
        fx = Fixture()
        full_day(fx)
        report = fx.check()
        steps = by_id(report)
        self.assertTrue(report["ok"], json.dumps(report["steps"], ensure_ascii=False, indent=1))
        self.assertEqual(report["missing_steps"], [])
        for sid in ("2", "3.0", "3.2", "3.3", "3.3a", "3.5", "3.6"):
            self.assertEqual(steps[sid]["status"], cdr.EXECUTED, sid)
        self.assertEqual(steps["3.3a-new"]["status"], cdr.NO_ACTION)
        self.assertEqual(steps["converge"]["status"], cdr.NO_ACTION)
        self.assertEqual(report["failed_by_reason"], {"REVISION_UNVERIFIED": 1})
        self.assertEqual(steps["3.3"]["details"]["required"], 1)

    def test_readings(self) -> None:
        fx = Fixture()
        full_day(fx)
        r = fx.check()["readings"]
        self.assertEqual(r["d63_g1"]["write_loop_seconds"], 180.0)
        self.assertTrue(r["d63_g1"]["under_gate"])
        self.assertTrue(r["d63_g1"]["counts_toward_d63"])
        self.assertEqual(r["d69_yearly"]["write_loop_seconds"], 180.0)
        self.assertTrue(r["d69_yearly"]["single_task_form"])
        self.assertEqual(r["etf_timing"]["write_loop_seconds"], 60.0)
        self.assertEqual(r["hot_generations"]["hot"], 2)
        self.assertEqual(r["hot_generations"]["by_state"]["evidence_only"], 1)
        self.assertFalse(r["hot_generations"]["at_capacity"])

    def test_slow_run_resets_count(self) -> None:
        fx = Fixture()
        full_day(fx)
        fx.conn.execute("UPDATE tasks SET last_progress_at=? WHERE task_id=?",
                        (stamp(30 + 11), fx.conn.execute(
                            "SELECT task_id FROM tasks WHERE command_key_json LIKE '%market-plan-run%' "
                            "AND command_key_json LIKE '%market.daily%' ORDER BY item_count DESC LIMIT 1"
                        ).fetchone()[0]))
        g1 = fx.check()["readings"]["d63_g1"]
        self.assertGreaterEqual(g1["write_loop_seconds"], 600)
        self.assertFalse(g1["counts_toward_d63"])
        self.assertTrue(g1["resets_count"])


class MissedSteps(unittest.TestCase):
    def ids(self, fx: Fixture, **kw) -> set[str]:
        return set(fx.check(**kw)["missing_steps"])

    def test_no_backfill(self) -> None:
        fx = Fixture()
        full_day(fx)
        fx.conn.execute("DELETE FROM task_items WHERE task_id IN (SELECT task_id FROM tasks "
                        "WHERE command_key_json LIKE '%explicit-range%')")
        fx.conn.execute("DELETE FROM tasks WHERE command_key_json LIKE '%explicit-range%'")
        report = fx.check()
        self.assertIn("3.3", report["missing_steps"])
        self.assertFalse(report["ok"])
        self.assertEqual(by_id(report)["3.3"]["details"]["uncovered"], ["sh.600001"])

    def test_backfill_for_wrong_entity(self) -> None:
        fx = Fixture()
        full_day(fx)
        fx.conn.execute("UPDATE task_items SET target_json=json_set(target_json,'$.symbol','sz.000002') "
                        "WHERE task_id=(SELECT task_id FROM tasks WHERE command_key_json LIKE '%explicit-range%')")
        step = by_id(fx.check())["3.3"]
        self.assertEqual(step["status"], cdr.MISSING)
        self.assertEqual(step["details"]["uncovered"], ["sh.600001"])

    def test_backfill_not_terminal(self) -> None:
        fx = Fixture()
        full_day(fx)
        fx.conn.execute("UPDATE tasks SET status='running' WHERE command_key_json LIKE '%explicit-range%'")
        self.assertIn("3.3", self.ids(fx))

    def test_no_tail(self) -> None:
        fx = Fixture()
        full_day(fx)
        fx.conn.execute("UPDATE source_refresh_runs SET started_at=?", (stamp(-60),))
        self.assertEqual(by_id(fx.check())["3.0"]["status"], cdr.MISSING)

    def test_tail_after_run_warns(self) -> None:
        fx = Fixture()
        full_day(fx)
        fx.conn.execute("UPDATE source_refresh_runs SET started_at=?", (stamp(31),))
        report = fx.check()
        self.assertEqual(by_id(report)["3.0"]["status"], cdr.EXECUTED)
        self.assertTrue(any("tail 晚于" in w for w in report["warnings"]))

    def test_failed_tail_is_reported_not_missing(self) -> None:
        fx = Fixture()
        full_day(fx)
        fx.conn.execute("UPDATE source_refresh_runs SET status='failed', failed_windows=2")
        report = fx.check()
        self.assertEqual(by_id(report)["3.0"]["status"], cdr.EXECUTED)
        self.assertTrue(any("来源刷新失败" in w for w in report["warnings"]))

    def test_no_a_share_run(self) -> None:
        fx = Fixture()
        full_day(fx)
        fx.conn.execute("DELETE FROM tasks WHERE task_id IN (SELECT task_id FROM tasks "
                        "WHERE item_count=6 OR item_count=4)")  # drop the A-share run
        step = by_id(fx.check())["3.2"]
        self.assertEqual(step["status"], cdr.MISSING)

    def test_etf_batch_missing(self) -> None:
        fx = Fixture()
        full_day(fx)
        fx.conn.execute("DELETE FROM tasks WHERE task_id IN (SELECT task_id FROM task_items "
                        "WHERE frozen_inputs_json LIKE '%etf_raw%')")
        fx.etf_series("sh.510500", OLD_DAY)  # series exists but a day behind
        self.assertEqual(by_id(fx.check())["3.3a"]["status"], cdr.MISSING)

    def test_new_etf_member_without_series(self) -> None:
        fx = Fixture()
        full_day(fx)
        fx.etf_member("sz.159999", listing=TARGET_DAY - 3)
        step = by_id(fx.check())["3.3a-new"]
        self.assertEqual(step["status"], cdr.MISSING)
        self.assertEqual(step["details"]["missing"], ["sz.159999"])

    def test_new_etf_member_absent_from_the_official_archive_is_a_known_gap(self) -> None:
        fx = Fixture()
        full_day(fx)
        fx.etf_member("sh.526000", listing=TARGET_DAY - 3)   # not in the day archive: nothing can collect it
        fx.archive_members = {"sh510300", "sz159999"}
        step = by_id(fx.check())["3.3a-new"]
        self.assertEqual(step["status"], cdr.NO_ACTION, step)
        self.assertEqual(step["details"]["absent_from_official_archive"], 1)
        self.assertEqual(step["details"]["absent_sample"], ["sh.526000"])

    def test_new_etf_member_in_the_archive_without_series_is_still_flagged(self) -> None:
        fx = Fixture()
        full_day(fx)
        fx.etf_member("sz.159999", listing=TARGET_DAY - 3)
        fx.etf_member("sh.526000", listing=TARGET_DAY - 3)
        fx.archive_members = {"sz159999"}                      # one is in the archive, one is not
        step = by_id(fx.check())["3.3a-new"]
        self.assertEqual(step["status"], cdr.MISSING)
        self.assertEqual(step["details"]["missing"], ["sz.159999"])
        self.assertEqual(step["details"]["absent_from_official_archive"], 1)

    def test_unreadable_archive_infers_nothing_from_it(self) -> None:
        fx = Fixture()
        full_day(fx)
        fx.etf_member("sh.526000", listing=TARGET_DAY - 3)
        fx.archive_members = None
        self.assertEqual(by_id(fx.check())["3.3a-new"]["status"], cdr.MISSING)

    def test_new_etf_member_known_gaps_are_not_flagged(self) -> None:
        fx = Fixture()
        full_day(fx)
        fx.etf_member("sz.159998", listing=TARGET_DAY - 3)   # proven gap
        fx.etf_member("sz.159997", listing=TARGET_DAY + 5)   # not listed yet
        fx.etf_member("sz.159996", listing=TARGET_DAY - 3)   # snapshot lacks the member
        fx.conn.execute(
            "INSERT INTO proven_gaps VALUES('g','sz.159998','day','2026-11-01','2026-11-02','k','e','d','t')")
        fx.task("start_collection", "market.daily", "v2", (1, 2), [
            {"id": "x", "symbol": "sz.159996", "success": False,
             "error": "member absent or ambiguous: expected exactly one of [..], found 0"}], "partially_failed")
        step = by_id(fx.check())["3.3a-new"]
        self.assertEqual(step["status"], cdr.NO_ACTION, step)
        self.assertEqual(step["details"]["known_gap"], ["sz.159996", "sz.159998"])
        self.assertEqual(step["details"]["listed_after_target"], ["sz.159997"])

    def test_yearly_run_missing(self) -> None:
        fx = Fixture()
        full_day(fx)
        fx.stock("sz.000003", yearly="behind")
        fx.conn.execute("DELETE FROM tasks WHERE command_key_json LIKE '%market.yearly%'")
        report = fx.check()
        self.assertEqual(by_id(report)["3.5"]["status"], cdr.MISSING)
        self.assertEqual(by_id(report)["converge"]["status"], cdr.MISSING)

    def test_republish_missing(self) -> None:
        fx = Fixture()
        full_day(fx)
        fx.conn.execute("DELETE FROM tasks WHERE command_key_json LIKE '%explicit-yearly%'")
        step = by_id(fx.check())["3.6"]
        self.assertEqual(step["status"], cdr.MISSING)
        self.assertEqual((step["details"]["expected"], step["details"]["covered"]), (1, 0))

    def test_not_converged_when_invalid_yearly_left(self) -> None:
        fx = Fixture()
        full_day(fx)
        fx.stock("sz.000009", yearly="replaced")
        step = by_id(fx.check())["converge"]
        self.assertEqual(step["status"], cdr.MISSING)
        self.assertIn("daily_replaced_since_derivation", step["details"]["yearly_open"])

    def test_no_new_generation_and_no_probe(self) -> None:
        fx = Fixture()
        full_day(fx)
        self.assertEqual(by_id(fx.check(activated=False))["2"]["status"], cdr.MISSING)

    def test_unchanged_day_probe_is_checked_no_action(self) -> None:
        fx = Fixture()
        fx.stock("sh.600000")
        fx.generation("active", GEN_NEW)
        fx.seen_day(20, '"abc"')
        report = fx.check(activated=False)
        self.assertEqual(by_id(report)["2"]["status"], cdr.NO_ACTION)
        self.assertTrue(report["ok"], report["missing_steps"])  # Skill §1: stop, nothing else required

    def test_pending_day_probe_without_activation(self) -> None:
        fx = Fixture()
        fx.stock("sh.600000")
        fx.seen_day(20, '"different"')
        self.assertEqual(by_id(fx.check(activated=False))["2"]["status"], cdr.MISSING)


class NoActionRules(unittest.TestCase):
    def test_no_failures_means_no_backfill_needed(self) -> None:
        fx = Fixture()
        for e in ("sh.600000", "sz.000001"):
            fx.stock(e)
        fx.tail()
        fx.a_share_run(["sh.600000", "sz.000001"])
        fx.yearly_run(["sh.600000"])
        report = fx.check()
        self.assertEqual(by_id(report)["3.3"]["status"], cdr.NO_ACTION)
        self.assertEqual(by_id(report)["3.6"]["status"], cdr.NO_ACTION)
        self.assertEqual(by_id(report)["3.3a"]["status"], cdr.NO_ACTION)  # no ETF series
        self.assertTrue(report["ok"])

    def test_no_bars_failure_needs_no_backfill_or_convergence(self) -> None:
        fx = Fixture()
        fx.stock("sh.600000")
        fx.stock("sz.000004", last_day=OLD_DAY)
        fx.tail()
        fx.a_share_run(["sh.600000", "sz.000004"],
                       failed={"sz.000004": "no bars inside the accepted range"})
        fx.yearly_run(["sh.600000"])
        report = fx.check()
        self.assertEqual(report["failed_by_reason"], {"NO_BARS_IN_RANGE": 1})
        self.assertEqual(by_id(report)["3.3"]["status"], cdr.NO_ACTION)
        self.assertEqual(by_id(report)["converge"]["status"], cdr.NO_ACTION)

    def test_session_plan_with_zero_eligible(self) -> None:
        fx = Fixture()
        fx.stock("sh.600000")
        fx.etf_series("sh.510300", OLD_DAY)
        fx.etf_member("sh.510300")
        fx.tail()
        fx.a_share_run(["sh.600000"])
        fx.yearly_run(["sh.600000"])
        plan_call = cdr.Call(0, "c1", "plan_market_update",
                             json.dumps({"dataset": "market.daily", "scope": "sh_sz_etf_current"}))
        session = cdr.Session([plan_call], {"c1": 'ok {"counts": {"eligible": 0}}'}, "")
        step = by_id(fx.check(session=session))["3.3a"]
        self.assertEqual((step["status"], step["basis"]), (cdr.NO_ACTION, "session"))


def make_session(tools: list[tuple[str, dict]], final: str) -> cdr.Session:
    calls = [cdr.Call(i, f"c{i}", name, json.dumps(args)) for i, (name, args) in enumerate(tools)]
    return cdr.Session(calls, {}, final)


READBACK = [("get_data_availability", {"dataset": "market.daily"}),
            ("query_data", {"dataset": "market.daily"}),
            ("get_data_availability", {"dataset": "market.daily", "adjustment": "raw"}),
            *[("list_data_versions", {"dataset": d}) for d in cdr.M2_DATASETS]]


class SessionChecks(unittest.TestCase):
    def test_complete_session(self) -> None:
        fx = Fixture()
        full_day(fx)
        session = make_session([("run_market_update", {}), *READBACK], "Daily sync completed.")
        report = fx.check(session=session)
        self.assertEqual(by_id(report)["4"]["status"], cdr.EXECUTED)
        self.assertTrue(report["ok"])
        self.assertFalse(report["session"]["false_completed"])

    def test_readback_before_last_write_does_not_count(self) -> None:
        fx = Fixture()
        full_day(fx)
        session = make_session([*READBACK, ("update_data", {})], "completed")
        step = by_id(fx.check(session=session))["4"]
        self.assertEqual(step["status"], cdr.MISSING)

    def test_missing_etf_readback(self) -> None:
        fx = Fixture()
        full_day(fx)
        tools = [t for t in READBACK if "adjustment" not in t[1]]
        step = by_id(fx.check(session=make_session(tools, "completed")))["4"]
        self.assertEqual(step["status"], cdr.MISSING)
        self.assertTrue(any("ETF" in m for m in step["details"]["missing"]))

    def test_false_completed_is_flagged(self) -> None:
        fx = Fixture()
        full_day(fx)
        fx.conn.execute("DELETE FROM source_refresh_runs")
        report = fx.check(session=make_session([("run_market_update", {}), *READBACK], "状态：completed"))
        self.assertIn("3.0", report["missing_steps"])
        self.assertTrue(report["session"]["false_completed"])
        self.assertFalse(report["ok"])
        self.assertIn("completed", cdr.summary(report, color=False))

    def test_honest_incomplete_report_is_not_false_completed(self) -> None:
        fx = Fixture()
        full_day(fx)
        fx.conn.execute("DELETE FROM source_refresh_runs")
        report = fx.check(session=make_session([("run_market_update", {}), *READBACK],
                                                "Not completed: tail was skipped."))
        self.assertFalse(report["session"]["claims_completed"])
        self.assertFalse(report["session"]["false_completed"])
        self.assertFalse(report["ok"])

    def test_read_session_jsonl_and_codemode_scripts(self) -> None:
        lines = [
            {"type": "session", "version": 3},
            {"type": "message", "message": {"role": "assistant", "content": [
                {"type": "toolCall", "id": "a", "name": "mcp__x-data-mcp__plan_market_update",
                 "arguments": {"dataset": "market.daily"}},
                {"type": "toolCall", "id": "b", "name": "codemode",
                 "arguments": {"script": "await tools.mcp__x_data_mcp__list_data_versions({dataset:'market.security_master'})"}},
            ]}},
            {"type": "message", "message": {"role": "toolResult", "toolCallId": "a",
                                            "content": [{"type": "text", "text": '{"counts":{"eligible":7}}'}]}},
            {"type": "message", "message": {"role": "assistant", "content": [
                {"type": "text", "text": "All done, completed."}]}},
        ]
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "s.jsonl"
            path.write_text("\n".join(json.dumps(x) for x in lines), encoding="utf-8")
            session = cdr.read_session(path)
        self.assertEqual([c.tool for c in session.calls], ["plan_market_update", "list_data_versions"])
        self.assertEqual(session.plan("market.daily", None), {"eligible": 7})
        self.assertIn("completed", session.final_text)


class Cli(unittest.TestCase):
    def build(self, tmp: Path, fx: Fixture) -> list[str]:
        db = tmp / "preview.sqlite"
        fx.conn.commit()  # backup() blocks on an open write transaction
        fx.acquire.commit()
        disk = sqlite3.connect(db)
        fx.conn.backup(disk)
        disk.close()
        gens = tmp / "gens"
        (gens / "generations" / GEN_NEW).mkdir(parents=True)
        (gens / "active-generation.json").write_text(json.dumps({
            "schema": "x-data-mcp.active-generation.v1", "generation_id": GEN_NEW,
            "manifest_sha256": "m", "activated_at": stamp(5), "previous_generation_id": GEN_OLD,
            "activation_request_id": "r"}))
        (gens / "generations" / GEN_NEW / "snapshot-manifest.json").write_text(json.dumps(
            {"schema": "s", "files": [{"role": "day", "etag": '"abc"', "bytes": 1}]}))
        store = sqlite3.connect(gens / "acquire.sqlite")
        fx.acquire.backup(store)
        store.close()
        return ["--db", str(db), "--generations", str(gens), "--since", "2026-11-05T08:00:00Z"]

    def run_cli(self, args: list[str]) -> tuple[int, dict | None, str]:
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = cdr.main(args)
        return code, (json.loads(out.getvalue()) if out.getvalue() else None), err.getvalue()

    def test_exit_codes_and_read_only(self) -> None:
        for broken, expected in ((False, 0), (True, 1)):
            fx = Fixture()
            full_day(fx)
            if broken:
                fx.conn.execute("DELETE FROM source_refresh_runs")
            with tempfile.TemporaryDirectory() as tmp:
                args = self.build(Path(tmp), fx)
                before = (Path(tmp) / "preview.sqlite").read_bytes()
                code, report, text = self.run_cli(args)
                self.assertEqual(code, expected, text)
                self.assertEqual(report["ok"], not broken)
                self.assertIn("日更核对", text)
                self.assertEqual((Path(tmp) / "preview.sqlite").read_bytes(), before)
                self.assertFalse(list(Path(tmp).glob("preview.sqlite-*")))

    def test_connection_refuses_writes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "x.sqlite"
            sqlite3.connect(path).executescript("CREATE TABLE t(a)")
            conn = cdr.open_readonly(path)
            with self.assertRaises(sqlite3.OperationalError):
                conn.execute("INSERT INTO t VALUES(1)")

    def test_unusable_inputs_exit_2(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            empty = Path(tmp) / "empty.sqlite"
            sqlite3.connect(empty).executescript("CREATE TABLE t(a)")
            gens = Path(tmp) / "g"
            gens.mkdir()
            base = ["--db", str(empty), "--generations", str(gens)]
            self.assertEqual(self.run_cli([*base, "--since", "2026-11-05T08:00:00Z"])[0], 2)
            self.assertEqual(self.run_cli([*base, "--since", "yesterday"])[0], 2)
            self.assertEqual(self.run_cli(["--db", str(Path(tmp) / "nope"), "--generations", str(gens),
                                           "--since", "2026-11-05T08:00:00Z"])[0], 2)


class Helpers(unittest.TestCase):
    def test_timestamps_with_nanoseconds(self) -> None:
        self.assertEqual(cdr.parse_ts("2026-11-05T08:40:27.123456789Z").second, 27)
        self.assertEqual(cdr.parse_ts("2026-11-05T08:40:27+08:00").hour, 0)
        self.assertEqual(cdr.parse_ts("2026-11-05T08:40:27").tzinfo, timezone.utc)
        self.assertIsNone(cdr.parse_ts("garbage"))

    def test_reason_code_mirrors_product(self) -> None:
        self.assertEqual(cdr.reason_code("x RevisionUnverified y"), "REVISION_UNVERIFIED")
        self.assertEqual(cdr.reason_code("PUBLISH_PRECONDITION"), "PUBLISH_PRECONDITION")
        self.assertEqual(cdr.reason_code("no bars inside the accepted range"), "NO_BARS_IN_RANGE")
        self.assertEqual(cdr.reason_code(None), "OTHER")

    def test_reason_codes_match_rust_source(self) -> None:
        text = (ROOT / "src/data_supply/task_v2.rs").read_text(encoding="utf-8")
        for needle in ('"RevisionUnverified"', '"PUBLISH_PRECONDITION"', '"no bars inside the accepted range"'):
            self.assertIn(f"contains({needle})", text)


if __name__ == "__main__":
    unittest.main()
