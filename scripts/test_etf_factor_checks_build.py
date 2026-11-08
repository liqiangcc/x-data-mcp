"""`etf_factor_checks_build.py` 的单元测试（#607）。

只用内存 sqlite 夹具、临时目录里的小 sqlite 和小 CSV，不依赖真实行情库、不读
生产库、不写任何来源库、不发网络请求。

覆盖任务点名的五件事：

1. `day` 与 `X_day` 错一天（以及 `X_day` 不等于 E 的两种情形）时，仍然按库里的
   E 正确匹配，`ordinal` / `evidence_digest` 从库里读；
2. 对不上的事件（v3 没有 / 库里没有 / 匹配到多条 / 类别不一致）全部进
   `unmatched`，并且对应实体计入「被阻断」；
3. D-92 的 1 tick 判定（`sh.510500` 的 0.0008 通过；边界上 0.0009 通过、
   0.0011 不通过）；
4. `sz.159922` 同 E 复合事件（先 cat11 后 cat1，2.3344 对官方 2.334）；
5. 输出能过 PR-B 的校验规则：`validate_record` 是 Rust `validate` 的 Python
   复刻；如果本机有 `source_store` 二进制，再真跑一次
   `import --format etf-factor-checks-jsonl --dry-run` 逐行验证。
"""

import importlib.util
import json
from collections import Counter
import os
import shutil
import sqlite3
import subprocess
import tempfile
import unittest
from decimal import Decimal
from pathlib import Path

SCRIPT = Path(__file__).with_name("etf_factor_checks_build.py")
spec = importlib.util.spec_from_file_location("etf_factor_checks_build", SCRIPT)
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

DOC_SHA = "a" * 64
DIGEST_A = "1" * 64
DIGEST_B = "2" * 64

CSV_HEADER = (
    "entity,cat,day,X_day,final_status,prev_close,official_qss,official_ks,"
    "ann_amount,ann_amount_per10,ann_ratio,ann_url,ann_sha256,ann_publish,attribution_v2"
)


def csv_text(*rows):
    """把若干 `(entity, cat, day, X_day, final_status, extra...)` 写成 CSV 文本。"""
    lines = [CSV_HEADER]
    for row in rows:
        values = {
            "entity": row[0],
            "cat": row[1],
            "day": row[2],
            "X_day": row[3],
            "final_status": row[4],
            "prev_close": "",
            "official_qss": "",
            "official_ks": "",
            "ann_amount": "",
            "ann_amount_per10": "",
            "ann_ratio": "",
            "ann_url": "",
            "ann_sha256": "",
            "ann_publish": "",
            "attribution_v2": "",
        }
        values.update(row[5] if len(row) > 5 else {})
        lines.append(",".join(values[column] for column in CSV_HEADER.split(",")))
    return "\n".join(lines) + "\n"


def load_rows(text):
    with tempfile.TemporaryDirectory() as directory:
        path = os.path.join(directory, "events-v3.csv")
        with open(path, "w", encoding="utf-8") as handle:
            handle.write(text)
        rows, warnings = mod.load_events_v3(path)
    return rows, warnings


def memory_db(events, etf_entities=None):
    """内存 sqlite 夹具：`market_series`（ETF）+ `adjustment_events`。"""
    conn = sqlite3.connect(":memory:")
    conn.execute(
        "CREATE TABLE market_series(series_key INTEGER PRIMARY KEY, entity TEXT, "
        "period INTEGER, adjustment INTEGER, instrument_class INTEGER)"
    )
    conn.execute(
        "CREATE TABLE adjustment_events(entity TEXT, ordinal INTEGER, "
        "effective_day INTEGER, category INTEGER, c1 TEXT, c2 TEXT, c3 TEXT, c4 TEXT, "
        "basis_status TEXT, evidence_digest BLOB, PRIMARY KEY(entity, ordinal))"
    )
    entities = etf_entities if etf_entities is not None else {item["entity"] for item in events}
    for index, entity in enumerate(sorted(entities), start=1):
        conn.execute(
            "INSERT INTO market_series VALUES(?,?,?,?,?)", (index, entity, 1, 1, 2)
        )
    for event in events:
        digest = bytes.fromhex(event.get("evidence_digest", DIGEST_A))
        conn.execute(
            "INSERT INTO adjustment_events VALUES(?,?,?,?,?,?,?,?,?,?)",
            (
                event["entity"],
                event["ordinal"],
                mod.epoch_day(event["effective_day"]),
                event["category"],
                event.get("c1", "0"),
                "0",
                event.get("c3", "0"),
                "0",
                event.get("basis_status", "verified"),
                digest,
            ),
        )
    conn.commit()
    return conn


def convert_with(conn, rows, checked_at="2026-11-09"):
    etf_entities, _columns, selected = mod.read_db_events(conn)
    entities = etf_entities | {row["entity"] for row in rows}
    db_events = mod.fetch_events(conn, selected, entities)
    return mod.build_records(db_events, rows, checked_at)


def by_ordinal(records):
    return {record["ordinal"]: record for record in records}


class MatchingTest(unittest.TestCase):
    """陷阱 1：v3 的 `day` 比 E 早一天，匹配键是库里的 E。"""

    def test_day_off_by_one_still_matches_by_ex_day(self):
        rows, _warnings = load_rows(
            csv_text(
                (
                    "sz.159922",
                    "11",
                    "2024-12-01",
                    "2024-12-02",
                    "verified_official",
                    {"prev_close": "6.159", "official_qss": "2.4636", "official_ks": "2.46"},
                )
            )
        )
        # 库里的 E 是 2024-12-02（epoch 日序号 20058），v3 的 day 是 12-01。
        conn = memory_db(
            [
                {
                    "entity": "sz.159922",
                    "ordinal": 7,
                    "effective_day": "2024-12-02",
                    "category": 11,
                    "c3": "2.5",
                    "evidence_digest": DIGEST_B,
                }
            ]
        )
        records, report = convert_with(conn, rows)
        self.assertEqual(len(records), 1)
        record = records[0]
        self.assertEqual(record["effective_day"], "2024-12-02")
        self.assertEqual(record["ordinal"], 7)
        self.assertEqual(record["evidence_digest"], DIGEST_B)
        self.assertEqual(record["check_status"], "verified_official")
        self.assertEqual(report["unmatched"]["count"], 0)
        self.assertEqual(report["warnings"]["v3_matched_by_x_day"], 1)

    def test_x_day_that_is_not_e_falls_back_to_day_plus_one(self):
        # 首根 bar 之前的事件：X_day 是第一根 bar（2013-04-18），E 是 day+1。
        rows, warnings = load_rows(
            csv_text(
                (
                    "sh.511880",
                    "11",
                    "2013-04-01",
                    "2013-04-18",
                    "no_effect",
                    {"attribution_v2": "event_before_first_bar"},
                )
            )
        )
        self.assertEqual(len(warnings["v3_x_day_not_day_plus_one"]), 1)
        conn = memory_db(
            [
                {
                    "entity": "sh.511880",
                    "ordinal": 328,
                    "effective_day": "2013-04-02",
                    "category": 11,
                    "c3": "0.01",
                }
            ]
        )
        records, report = convert_with(conn, rows)
        self.assertEqual(len(records), 1)
        self.assertEqual(records[0]["effective_day"], "2013-04-02")
        self.assertEqual(records[0]["check_status"], "no_effect")
        self.assertEqual(report["unmatched"]["count"], 0)
        self.assertEqual(
            report["warnings"]["v3_matched_by_day_plus_one"][0]["v3_ex_day"], "2013-04-18"
        )

    def test_weekend_registration_day_uses_x_day_as_e(self):
        # sz.159922 2024-11-30 是周六，E = 2024-12-02 = day + 2，不能用 day+1。
        rows, _warnings = load_rows(
            csv_text(
                (
                    "sz.159922",
                    "11",
                    "2024-11-30",
                    "2024-12-02",
                    "no_effect",
                    {"attribution_v2": "event_before_first_bar"},
                )
            )
        )
        conn = memory_db(
            [
                {
                    "entity": "sz.159922",
                    "ordinal": 0,
                    "effective_day": "2024-12-02",
                    "category": 11,
                    "c3": "2.5",
                }
            ]
        )
        records, report = convert_with(conn, rows)
        self.assertEqual(len(records), 1)
        self.assertEqual(records[0]["effective_day"], "2024-12-02")
        self.assertEqual(report["unmatched"]["count"], 0)


class UnmatchedTest(unittest.TestCase):
    """陷阱 2：对不上的一条都不静默跳过，并且阻断对应实体。"""

    def test_every_kind_of_mismatch_is_listed(self):
        rows, _warnings = load_rows(
            csv_text(
                # 库里没有这个事件。
                ("sz.159001", "1", "2020-01-06", "2020-01-07", "verified_announcement"),
                # 库里同一 E 上的类别是 11，v3 说是 1。
                ("sz.159002", "1", "2020-01-06", "2020-01-07", "verified_announcement"),
                # v3 两行指向同一个事件。
                ("sz.159003", "11", "2020-01-06", "2020-01-07", "no_effect"),
                ("sz.159003", "11", "2020-01-05", "2020-01-07", "no_effect"),
            )
        )
        conn = memory_db(
            [
                {
                    "entity": "sz.159002",
                    "ordinal": 0,
                    "effective_day": "2020-01-07",
                    "category": 11,
                    "c3": "2",
                },
                {
                    "entity": "sz.159003",
                    "ordinal": 1,
                    "effective_day": "2020-01-07",
                    "category": 11,
                    "c3": "2",
                },
                # 库里多出来的事件，v3 没有。
                {
                    "entity": "sz.159004",
                    "ordinal": 2,
                    "effective_day": "2021-05-06",
                    "category": 1,
                    "c1": "0.3",
                },
                # 库里同 (entity, E, category) 两条。
                {
                    "entity": "sz.159005",
                    "ordinal": 3,
                    "effective_day": "2022-03-08",
                    "category": 11,
                    "c3": "2",
                },
                {
                    "entity": "sz.159005",
                    "ordinal": 4,
                    "effective_day": "2022-03-08",
                    "category": 11,
                    "c3": "3",
                },
            ]
        )
        rows, _warnings = load_rows(
            csv_text(
                ("sz.159001", "1", "2020-01-06", "2020-01-07", "verified_announcement"),
                ("sz.159002", "1", "2020-01-06", "2020-01-07", "verified_announcement"),
                ("sz.159003", "11", "2020-01-06", "2020-01-07", "no_effect"),
                ("sz.159003", "11", "2020-01-05", "2020-01-07", "no_effect"),
                ("sz.159005", "11", "2022-03-07", "2022-03-08", "no_effect"),
            )
        )
        records, report = convert_with(conn, rows)
        reasons = Counter(item["reason"] for item in report["unmatched"]["rows"])
        self.assertEqual(
            reasons,
            Counter(
                {
                    # v3 说 cat1、库里同一 E 是 cat11：两个方向各记一条。
                    "category_mismatch": 1,
                    "no_v3_row": 2,
                    # v3 两行指向同一个事件。
                    "duplicate_v3_rows": 2,
                    "multiple_v3_rows": 1,
                    # 库里同 (entity, E, category) 两条事件。
                    "multiple_db_events": 1,
                    # v3 有、库里没有。
                    "no_db_event": 1,
                }
            ),
        )
        self.assertEqual(report["unmatched"]["count"], 8)
        self.assertFalse(report["ok"])
        blocked = {item["entity"] for item in report["entities"]["blocked_entities"]}
        self.assertEqual(
            blocked,
            {"sz.159001", "sz.159002", "sz.159003", "sz.159004", "sz.159005"},
        )
        self.assertEqual(records, [])
        self.assertEqual(report["entities"]["deliverable"], 0)

    def test_missing_record_blocks_the_entity_but_others_stay_deliverable(self):
        rows, _warnings = load_rows(
            csv_text(
                (
                    "sz.159001",
                    "11",
                    "2020-01-06",
                    "2020-01-07",
                    "no_effect",
                    {"attribution_v2": "event_before_first_bar"},
                )
            )
        )
        conn = memory_db(
            [
                {
                    "entity": "sz.159001",
                    "ordinal": 0,
                    "effective_day": "2020-01-07",
                    "category": 11,
                    "c3": "2",
                },
                {
                    "entity": "sz.159002",
                    "ordinal": 1,
                    "effective_day": "2021-05-06",
                    "category": 1,
                    "c1": "0.3",
                },
            ]
        )
        _records, report = convert_with(conn, rows)
        self.assertEqual(report["unmatched"]["count"], 1)
        self.assertEqual(
            [item["entity"] for item in report["entities"]["blocked_entities"]],
            ["sz.159002"],
        )
        self.assertEqual(report["entities"]["deliverable"], 1)


class OneTickTest(unittest.TestCase):
    """陷阱 3：D-92 的 1 tick 判定。"""

    def _convert_unverified(self, entity, category, factors, announced, prev_close):
        row = {
            "prev_close": prev_close,
            "ann_url": "https://example.invalid/ann.pdf",
            "ann_sha256": DOC_SHA,
            "ann_publish": "2015-04-03",
        }
        row.update(announced)
        rows, _warnings = load_rows(
            csv_text(
                (entity, str(category), "2015-04-14", "2015-04-15", "unverified", row)
            )
        )
        event = {
            "entity": entity,
            "ordinal": 0,
            "effective_day": "2015-04-15",
            "category": category,
        }
        event.update(factors)
        conn = memory_db([event])
        records, report = convert_with(conn, rows)
        return records[0], report

    def test_sh510500_announced_ratio_is_within_one_tick(self):
        # 真实数字：2.243 / 0.28032482 = 8.0014321，2.243 / 0.28035332 = 8.0006186，
        # 差 0.0008134 → 通过。
        record, report = self._convert_unverified(
            "sh.510500",
            11,
            {"c3": "0.28032482"},
            {"ann_ratio": "0.28035332"},
            "2.243",
        )
        self.assertEqual(record["check_status"], "verified_announcement")
        self.assertTrue(record["within_one_tick"])
        self.assertEqual(record["price_delta"], "0.000813405076")
        self.assertIn("D-92", record["basis_ref"])
        self.assertEqual(report["d92"]["verified"], 1)
        self.assertEqual(report["d92"]["rows"][0]["price_delta"], "0.000813405076")

    def test_boundary_0009_passes_and_0011_fails(self):
        # 前收 10.00：gbbq c1=2.00 → 9.8；公告 c1=1.991 → 9.8009，差 0.0009（通过）。
        passed, _ = self._convert_unverified(
            "sh.510500", 1, {"c1": "2"}, {"ann_amount": "1.991"}, "10"
        )
        self.assertEqual(passed["check_status"], "verified_announcement")
        self.assertEqual(passed["price_delta"], "-0.0009")
        self.assertTrue(passed["within_one_tick"])
        # 公告 c1=1.989 → 9.8011，差 0.0011（不通过，保持 unverified）。
        failed, report = self._convert_unverified(
            "sh.510500", 1, {"c1": "2"}, {"ann_amount": "1.989"}, "10"
        )
        self.assertEqual(failed["check_status"], "unverified")
        self.assertEqual(failed["price_delta"], "-0.0011")
        self.assertFalse(failed["within_one_tick"])
        self.assertIn("D-92", failed["basis_ref"])
        self.assertEqual(report["d92"]["unverified"], 1)

    def test_amount_per10_wins_over_amount(self):
        # 公告写每 1 份 2.54 元、每 10 份 25.4 元时，用每 10 份的 25.4 对 c1=25.4。
        record, _report = self._convert_unverified(
            "sh.511880",
            1,
            {"c1": "25.4"},
            {"ann_amount": "2.54", "ann_amount_per10": "25.4"},
            "104.404",
        )
        self.assertEqual(record["check_status"], "verified_announcement")
        self.assertEqual(record["price_delta"], "0")

    def test_delta_is_short_enough_for_rust_decimal(self):
        # 159943 的除法余数有 35 位小数；写进记录前必须收到 12 位以内。
        record, _report = self._convert_unverified(
            "sz.159943",
            11,
            {"c3": "11.817462"},
            {"ann_ratio": "11.817461846"},
            "16.914",
        )
        self.assertTrue(record["within_one_tick"])
        text = record["price_delta"]
        self.assertRegex(text, r"^[+-]?\d+(\.\d{1,12})?$")


class CompositeTest(unittest.TestCase):
    """陷阱 3 的复合事件：`sz.159922` 同一个 E 上的 cat11 + cat1。"""

    def _rows(self):
        rows, _warnings = load_rows(
            csv_text(
                (
                    "sz.159922",
                    "11",
                    "2024-11-30",
                    "2024-12-02",
                    "unverified",
                    {"prev_close": "6.159", "official_qss": "2.334", "official_ks": "2.461"},
                ),
                (
                    "sz.159922",
                    "1",
                    "2024-12-01",
                    "2024-12-02",
                    "unverified",
                    {"prev_close": "6.159", "official_qss": "2.334", "official_ks": "2.461"},
                ),
            )
        )
        return rows

    def _conn(self):
        return memory_db(
            [
                {
                    "entity": "sz.159922",
                    "ordinal": 1198,
                    "effective_day": "2024-12-02",
                    "category": 11,
                    "c3": "2.5",
                },
                {
                    "entity": "sz.159922",
                    "ordinal": 1199,
                    "effective_day": "2024-12-02",
                    "category": 1,
                    "c1": "1.292",
                },
            ]
        )

    def test_both_records_are_verified_official_and_note_the_composite(self):
        records, report = convert_with(self._conn(), self._rows())
        self.assertEqual(len(records), 2)
        for record in records:
            self.assertEqual(record["check_status"], "verified_official")
            self.assertEqual(record["basis"], "official_qss")
            self.assertIn("same_E_composite", record["basis_ref"])
            self.assertEqual(record["price_delta"], "-0.0004")
            self.assertTrue(record["within_one_tick"])
            self.assertEqual(record["announced_value"], "2.334")
        self.assertEqual(
            {record["ordinal"]: record["gbbq_value"] for record in records},
            {1198: "2.5", 1199: "1.292"},
        )
        self.assertEqual(report["d92"]["verified"], 2)
        self.assertEqual(report["unmatched"]["count"], 0)

    def test_reverse_order_is_not_within_one_tick(self):
        # 先 cat1 后 cat11：(6.159 - 0.1292) / 2.5 = 2.4119，对 2.334 差 0.0779。
        # `compose_price` 固定先 cat11 后 cat1（D-91），所以反次序在这里手算。
        reverse = (Decimal("6.159") - Decimal("1.292") / 10) / Decimal("2.5")
        self.assertEqual(mod.decimal_text(reverse, mod.DELTA_SCALE), "2.41192")
        self.assertFalse(mod.within_one_tick(Decimal("2.334") - reverse))
        # 先 cat11 后 cat1：2.3344。
        cat11 = (11, Decimal("0"), Decimal("2.5"))
        cat1 = (1, Decimal("1.292"), Decimal("0"))
        proper = mod.compose_price(Decimal("6.159"), [cat1, cat11])
        self.assertEqual(mod.decimal_text(proper, mod.DELTA_SCALE), "2.3344")
        self.assertTrue(mod.within_one_tick(Decimal("2.334") - proper))


class StatusMappingTest(unittest.TestCase):
    """`final_status` → `check_status` 的其余映射。"""

    def test_no_effect_keeps_its_verdict_without_evidence(self):
        rows, _warnings = load_rows(
            csv_text(
                (
                    "sh.510010",
                    "11",
                    "2009-12-07",
                    "2009-12-15",
                    "no_effect",
                    {"attribution_v2": "event_before_first_bar"},
                )
            )
        )
        conn = memory_db(
            [
                {
                    "entity": "sh.510010",
                    "ordinal": 0,
                    "effective_day": "2009-12-08",
                    "category": 11,
                    "c3": "1.113193",
                }
            ]
        )
        records, report = convert_with(conn, rows)
        record = records[0]
        self.assertEqual(record["check_status"], "no_effect")
        self.assertEqual(record["basis"], "none")
        self.assertEqual(record["basis_ref"], "no_effect:event_before_first_bar")
        self.assertEqual(record["evidence_url"], "")
        self.assertEqual(record["evidence_sha256"], "")
        self.assertEqual(record["announced_value"], "")
        self.assertEqual(record["gbbq_value"], "1.113193")
        self.assertEqual(record["price_delta"], "")
        self.assertIsNone(record["within_one_tick"])
        self.assertEqual(report["entities"]["blocked"], 0)

    def test_announcement_keeps_url_and_sha_and_no_price_delta(self):
        rows, _warnings = load_rows(
            csv_text(
                (
                    "sh.510050",
                    "1",
                    "2016-11-27",
                    "2016-11-28",
                    "verified_announcement",
                    {
                        "ann_amount": "0.52",
                        "ann_url": "https://example.invalid/510050.pdf",
                        "ann_sha256": DOC_SHA,
                        "ann_publish": "2016-11-24",
                    },
                )
            )
        )
        conn = memory_db(
            [
                {
                    "entity": "sh.510050",
                    "ordinal": 5,
                    "effective_day": "2016-11-28",
                    "category": 1,
                    "c1": "0.52",
                }
            ]
        )
        records, _report = convert_with(conn, rows)
        record = records[0]
        self.assertEqual(record["check_status"], "verified_announcement")
        self.assertEqual(record["basis"], "announcement")
        self.assertEqual(record["basis_ref"], "sse_ann:2016-11-24")
        self.assertEqual(record["evidence_url"], "https://example.invalid/510050.pdf")
        self.assertEqual(record["evidence_sha256"], DOC_SHA)
        self.assertEqual(record["announced_value"], "0.52")
        self.assertEqual(record["price_delta"], "")
        self.assertIsNone(record["within_one_tick"])

    def test_float_noise_in_the_announced_value_is_cleaned(self):
        # v3 的 ann_amount_per10 偶尔带浮点尾巴（sh.511880 2020-12-30）。
        rows, _warnings = load_rows(
            csv_text(
                (
                    "sh.511880",
                    "1",
                    "2020-12-30",
                    "2020-12-31",
                    "verified_announcement",
                    {
                        "ann_amount": "2.015",
                        "ann_amount_per10": "20.150000000000002",
                        "ann_url": "https://example.invalid/511880.pdf",
                        "ann_sha256": DOC_SHA,
                        "ann_publish": "2020-12-28",
                    },
                )
            )
        )
        conn = memory_db(
            [
                {
                    "entity": "sh.511880",
                    "ordinal": 337,
                    "effective_day": "2020-12-31",
                    "category": 1,
                    "c1": "20.15",
                }
            ]
        )
        records, _report = convert_with(conn, rows)
        self.assertEqual(records[0]["announced_value"], "20.15")

    def test_official_uses_the_x_day_qss_and_the_events_own_e(self):
        rows, _warnings = load_rows(
            csv_text(
                (
                    "sz.159209",
                    "1",
                    "2025-07-14",
                    "2025-07-15",
                    "verified_official",
                    {
                        "prev_close": "1.025",
                        "official_qss": "1.024",
                        "official_ks": "1.024",
                    },
                )
            )
        )
        conn = memory_db(
            [
                {
                    "entity": "sz.159209",
                    "ordinal": 9,
                    "effective_day": "2025-07-15",
                    "category": 1,
                    "c1": "0.03",
                }
            ]
        )
        records, report = convert_with(conn, rows)
        record = records[0]
        self.assertEqual(record["basis"], "official_qss")
        self.assertEqual(record["basis_ref"], "szse_1815_stock:2025-07-15;delta_over_tick")
        self.assertIn("txtQueryDate=2025-07-15", record["evidence_url"])
        self.assertEqual(record["announced_value"], "1.024")
        self.assertEqual(record["price_delta"], "0.002")
        self.assertFalse(record["within_one_tick"])
        # v3 说是 official_match 但差 2 分钱：库里的因子和证据对不上，fail-closed。
        self.assertEqual(record["check_status"], "unverified")
        self.assertEqual(len(report["warnings"]["official_delta_over_tick"]), 1)

    def test_official_without_prev_close_is_not_verified(self):
        rows, _warnings = load_rows(
            csv_text(
                (
                    "sz.159209",
                    "1",
                    "2025-07-14",
                    "2025-07-15",
                    "verified_official",
                    {"official_qss": "1.022"},
                )
            )
        )
        conn = memory_db(
            [
                {
                    "entity": "sz.159209",
                    "ordinal": 9,
                    "effective_day": "2025-07-15",
                    "category": 1,
                    "c1": "0.03",
                }
            ]
        )
        records, report = convert_with(conn, rows)
        record = records[0]
        self.assertEqual(record["check_status"], "unverified")
        self.assertEqual(record["basis_ref"], "szse_1815_stock:2025-07-15;no_prev_close")
        self.assertEqual(record["price_delta"], "")
        self.assertIsNone(record["within_one_tick"])
        self.assertEqual(len(report["warnings"]["no_prev_close"]), 1)


class ValidationTest(unittest.TestCase):
    """输出必须能被 PR-B 原样接受。"""

    def _fixture(self, directory):
        csv_path = os.path.join(directory, "events-v3.csv")
        with open(csv_path, "w", encoding="utf-8") as handle:
            handle.write(
                csv_text(
                    (
                        "sz.159922",
                        "11",
                        "2024-11-30",
                        "2024-12-02",
                        "unverified",
                        {"prev_close": "6.159", "official_qss": "2.334"},
                    ),
                    (
                        "sz.159922",
                        "1",
                        "2024-12-01",
                        "2024-12-02",
                        "unverified",
                        {"prev_close": "6.159", "official_qss": "2.334"},
                    ),
                    (
                        "sh.510050",
                        "1",
                        "2016-11-27",
                        "2016-11-28",
                        "verified_announcement",
                        {
                            "ann_amount": "0.52",
                            "ann_url": "https://example.invalid/510050.pdf",
                            "ann_sha256": DOC_SHA,
                            "ann_publish": "2016-11-24",
                        },
                    ),
                    (
                        "sh.510010",
                        "11",
                        "2009-12-07",
                        "2009-12-15",
                        "no_effect",
                        {"attribution_v2": "event_before_first_bar"},
                    ),
                )
            )
        db_path = os.path.join(directory, "fixture.sqlite")
        conn = sqlite3.connect(db_path)
        conn.execute(
            "CREATE TABLE market_series(series_key INTEGER PRIMARY KEY, entity TEXT, "
            "period INTEGER, adjustment INTEGER, instrument_class INTEGER)"
        )
        conn.execute(
            "CREATE TABLE adjustment_events(entity TEXT, ordinal INTEGER, "
            "effective_day INTEGER, category INTEGER, c1 TEXT, c2 TEXT, c3 TEXT, c4 TEXT, "
            "evidence_digest BLOB, PRIMARY KEY(entity, ordinal))"
        )
        for index, entity in enumerate(("sh.510010", "sh.510050", "sz.159922"), start=1):
            conn.execute(
                "INSERT INTO market_series VALUES(?,?,?,?,?)", (index, entity, 1, 1, 2)
            )
        for event in (
            ("sz.159922", 1198, "2024-12-02", 11, "0", "2.5", DIGEST_A),
            ("sz.159922", 1199, "2024-12-02", 1, "1.292", "0", DIGEST_B),
            ("sh.510050", 5, "2016-11-28", 1, "0.52", "0", DIGEST_A),
            ("sh.510010", 0, "2009-12-08", 11, "0", "1.113193", DIGEST_B),
        ):
            conn.execute(
                "INSERT INTO adjustment_events VALUES(?,?,?,?,?,?,?,?,?)",
                (
                    event[0],
                    event[1],
                    mod.epoch_day(event[2]),
                    event[3],
                    event[4],
                    "0",
                    event[5],
                    "0",
                    bytes.fromhex(event[6]),
                ),
            )
        conn.commit()
        conn.close()
        return csv_path, db_path

    def test_every_record_passes_the_python_copy_of_the_pr_b_rules(self):
        with tempfile.TemporaryDirectory() as directory:
            csv_path, db_path = self._fixture(directory)
            records, report = mod.convert(csv_path, db_path, checked_at="2026-11-09")
            self.assertEqual(report["unmatched"]["count"], 0)
            self.assertEqual(report["validation"]["failures"], [])
            self.assertEqual(len(records), 4)
            for record in records:
                self.assertEqual(mod.validate_record(record), [])
                canonical = json.loads(mod.canonical_json(record))
                self.assertEqual(tuple(canonical.keys()), mod.CANONICAL_FIELDS)
                self.assertEqual(mod.record_id(record), mod.sha256_hex(mod.canonical_json(record).encode()))

    def test_validation_rejects_the_cases_the_rust_rules_reject(self):
        base = {
            "record_version": mod.RECORD_VERSION,
            "entity": "sz.159922",
            "ordinal": 1,
            "effective_day": "2024-12-02",
            "category": 11,
            "check_status": "verified_announcement",
            "basis": "announcement",
            "basis_ref": "szse_ann:2024-11-29",
            "evidence_url": "https://example.invalid/a.pdf",
            "evidence_sha256": DOC_SHA,
            "announced_value": "2.5",
            "gbbq_value": "2.5",
            "price_delta": "0.0004",
            "within_one_tick": True,
            "checked_at": "2026-11-09",
            "evidence_digest": DIGEST_A,
        }
        self.assertEqual(mod.validate_record(base), [])
        cases = {
            "record_version": {"record_version": "etf-factor-checks-v2"},
            "entity": {"entity": "sz159922"},
            "ordinal": {"ordinal": -1},
            "category": {"category": 2},
            "effective_day": {"effective_day": "2024-12-32"},
            "checked_at": {"checked_at": ""},
            "check_status": {"check_status": "verified"},
            "basis": {"basis": "qss"},
            "verified_without_basis": {"basis": "none", "evidence_sha256": ""},
            "basis_ref": {"basis_ref": " "},
            "evidence_url": {"evidence_url": ""},
            "evidence_sha256": {"evidence_sha256": "not-a-sha"},
            "basis_none_with_sha": {
                "check_status": "no_effect",
                "basis": "none",
            },
            "status_basis_pair": {"basis": "official_qss"},
            "gbbq_value": {"gbbq_value": "2,5"},
            "within_one_tick": {"price_delta": ""},
            "evidence_digest": {"evidence_digest": "abc"},
        }
        for name, patch in cases.items():
            record = dict(base)
            record.update(patch)
            self.assertTrue(mod.validate_record(record), f"{name} 应该被拒绝")

    def test_output_is_accepted_by_the_real_importer_when_it_is_built(self):
        binary = _source_store_binary()
        if binary is None:
            self.skipTest("没有 source_store 二进制（先 cargo build --bin source_store）")
        if not _binary_supports_check_import(binary):
            self.skipTest(f"{binary} 不认识 etf-factor-checks-jsonl（不是本分支构建的）")
        with tempfile.TemporaryDirectory() as directory:
            csv_path, db_path = self._fixture(directory)
            out = os.path.join(directory, "checks.jsonl")
            report_path = os.path.join(directory, "report.json")
            code = mod.main(
                [
                    "convert",
                    "--events-v3",
                    csv_path,
                    "--db",
                    db_path,
                    "--out",
                    out,
                    "--report",
                    report_path,
                    "--checked-at",
                    "2026-11-09",
                ]
            )
            self.assertEqual(code, mod.EXIT_OK)
            with open(report_path, encoding="utf-8") as handle:
                report = json.load(handle)
            self.assertEqual(report["records"], 4)
            result = subprocess.run(
                [
                    binary,
                    "import",
                    "--store",
                    os.path.join(directory, "store"),
                    "--index",
                    out,
                    "--blobs",
                    os.path.join(directory, "blobs"),
                    "--format",
                    "etf-factor-checks-jsonl",
                    "--dry-run",
                ],
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            imported = json.loads(result.stdout)
            self.assertEqual(imported["skipped_unverified"], [])
            self.assertEqual(len(imported["imported"]), 4)
            self.assertEqual(imported["manifest_lines_added"], 4)


def _binary_supports_check_import(binary):
    """二进制是否带 PR-B 的导入格式（同一个 target 目录可能被别的分支构建过）。"""
    result = subprocess.run(
        [binary, "import", "--format", "etf-factor-checks-jsonl"],
        capture_output=True,
        text=True,
        check=False,
    )
    return "unknown import format" not in (result.stderr or "") + (result.stdout or "")


def _source_store_binary():
    candidates = []
    target = os.environ.get("CARGO_TARGET_DIR")
    if target:
        candidates.append(os.path.join(target, "debug", "source_store"))
    root = Path(__file__).resolve().parent.parent
    candidates.append(str(root / "target" / "debug" / "source_store"))
    for candidate in candidates:
        if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
            return candidate
    found = shutil.which("source_store")
    return found


class ReadOnlyTest(unittest.TestCase):
    """库只读打开：不复制、不写。"""

    def test_the_database_is_opened_read_only(self):
        with tempfile.TemporaryDirectory() as directory:
            db_path = os.path.join(directory, "fixture.sqlite")
            conn = sqlite3.connect(db_path)
            conn.execute("CREATE TABLE market_series(entity TEXT, instrument_class INTEGER)")
            conn.execute(
                "CREATE TABLE adjustment_events(entity TEXT, ordinal INTEGER, "
                "effective_day INTEGER, category INTEGER, c1 TEXT, c3 TEXT, "
                "evidence_digest BLOB)"
            )
            conn.commit()
            conn.close()
            before = os.stat(db_path)
            conn = mod.open_read_only(db_path)
            with self.assertRaises(sqlite3.OperationalError):
                conn.execute("INSERT INTO market_series VALUES('sz.159922', 2)")
            conn.close()
            after = os.stat(db_path)
            self.assertEqual(before.st_size, after.st_size)
            self.assertEqual(before.st_mtime_ns, after.st_mtime_ns)
            self.assertEqual(os.listdir(directory), ["fixture.sqlite"])

    def test_missing_database_is_an_input_error(self):
        with self.assertRaises(mod.InputError):
            mod.open_read_only("/nonexistent/does-not-exist.sqlite")


class CommandLineTest(unittest.TestCase):
    """CLI 的退出码与文件写出。"""

    def _fixture(self, directory, extra_rows=()):
        csv_path = os.path.join(directory, "events-v3.csv")
        rows = [
            (
                "sz.159922",
                "11",
                "2024-11-30",
                "2024-12-02",
                "unverified",
                {"prev_close": "6.159", "official_qss": "2.334"},
            ),
            (
                "sz.159922",
                "1",
                "2024-12-01",
                "2024-12-02",
                "unverified",
                {"prev_close": "6.159", "official_qss": "2.334"},
            ),
        ]
        rows.extend(extra_rows)
        with open(csv_path, "w", encoding="utf-8") as handle:
            handle.write(csv_text(*rows))
        db_path = os.path.join(directory, "fixture.sqlite")
        conn = sqlite3.connect(db_path)
        conn.execute(
            "CREATE TABLE market_series(series_key INTEGER PRIMARY KEY, entity TEXT, "
            "period INTEGER, adjustment INTEGER, instrument_class INTEGER)"
        )
        conn.execute(
            "CREATE TABLE adjustment_events(entity TEXT, ordinal INTEGER, "
            "effective_day INTEGER, category INTEGER, c1 TEXT, c2 TEXT, c3 TEXT, c4 TEXT, "
            "evidence_digest BLOB, PRIMARY KEY(entity, ordinal))"
        )
        conn.execute("INSERT INTO market_series VALUES(1,'sz.159922',1,1,2)")
        conn.execute(
            "INSERT INTO adjustment_events VALUES('sz.159922',1198,?,11,'0','0','2.5','0',?)",
            (mod.epoch_day("2024-12-02"), bytes.fromhex(DIGEST_A)),
        )
        conn.execute(
            "INSERT INTO adjustment_events VALUES('sz.159922',1199,?,1,'1.292','0','0','0',?)",
            (mod.epoch_day("2024-12-02"), bytes.fromhex(DIGEST_B)),
        )
        conn.commit()
        conn.close()
        return csv_path, db_path

    def test_clean_run_writes_jsonl_and_report(self):
        with tempfile.TemporaryDirectory() as directory:
            csv_path, db_path = self._fixture(directory)
            out = os.path.join(directory, "out", "checks.jsonl")
            report_path = os.path.join(directory, "out", "report.json")
            code = mod.main(
                [
                    "convert",
                    "--events-v3",
                    csv_path,
                    "--db",
                    db_path,
                    "--out",
                    out,
                    "--report",
                    report_path,
                    "--checked-at",
                    "2026-11-09",
                ]
            )
            self.assertEqual(code, mod.EXIT_OK)
            with open(out, encoding="utf-8") as handle:
                lines = [line for line in handle.read().splitlines() if line]
            self.assertEqual(len(lines), 2)
            for line in lines:
                self.assertEqual(mod.validate_record(json.loads(line)), [])
            with open(report_path, encoding="utf-8") as handle:
                report = json.load(handle)
            self.assertEqual(report["records"], 2)
            self.assertEqual(report["by_check_status"], {"verified_official": 2})
            self.assertEqual(report["by_market_and_status"], {"sz": {"verified_official": 2}})
            self.assertEqual(report["d92"]["verified"], 2)
            self.assertEqual(report["entities"]["deliverable"], 1)
            self.assertEqual(report["checked_at"], "2026-11-09")

    def test_unmatched_exits_three_unless_allowed(self):
        with tempfile.TemporaryDirectory() as directory:
            csv_path, db_path = self._fixture(
                directory,
                extra_rows=(
                    (
                        "sh.510050",
                        "1",
                        "2016-11-27",
                        "2016-11-28",
                        "verified_announcement",
                    ),
                ),
            )
            out = os.path.join(directory, "checks.jsonl")
            arguments = [
                "convert",
                "--events-v3",
                csv_path,
                "--db",
                db_path,
                "--out",
                out,
                "--checked-at",
                "2026-11-09",
            ]
            self.assertEqual(mod.main(arguments), mod.EXIT_NEEDS_REVIEW)
            self.assertTrue(os.path.exists(out))
            self.assertEqual(
                mod.main(arguments + ["--allow-unmatched"]), mod.EXIT_OK
            )

    def test_missing_input_exits_two(self):
        with tempfile.TemporaryDirectory() as directory:
            code = mod.main(
                [
                    "convert",
                    "--events-v3",
                    os.path.join(directory, "nope.csv"),
                    "--db",
                    os.path.join(directory, "nope.sqlite"),
                    "--out",
                    os.path.join(directory, "out.jsonl"),
                ]
            )
            self.assertEqual(code, mod.EXIT_INPUT_ERROR)
            self.assertFalse(os.path.exists(os.path.join(directory, "out.jsonl")))

    def test_bad_checked_at_exits_two(self):
        with tempfile.TemporaryDirectory() as directory:
            csv_path, db_path = self._fixture(directory)
            code = mod.main(
                [
                    "convert",
                    "--events-v3",
                    csv_path,
                    "--db",
                    db_path,
                    "--out",
                    os.path.join(directory, "out.jsonl"),
                    "--checked-at",
                    "2026-13-01",
                ]
            )
            self.assertEqual(code, mod.EXIT_INPUT_ERROR)


if __name__ == "__main__":
    unittest.main()
