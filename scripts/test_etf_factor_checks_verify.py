"""(#607 Q10) `etf_factor_checks_build.py verify` 子命令的离线测试。

**不联网、不碰生产、不写来源库**：网络访问全部走假 fetcher（`FakeFetcher`），
行情库是内存/tempdir 里的 sqlite 夹具，PDF 只有一个假的字节串，公告正文来自
`scripts/testdata/announcements/*.txt` 的文本夹具（仓库里没有 PDF）。

覆盖任务点名的九件事：

1. 只处理缺记录的事件；
2. `evidence_digest` 变了就当作新事件处理；
3. 深市官方核对通过 / 不通过；
4. 公告解析：元/10 份、元/份、全角点、拆分方向、合并（如 `0.1`）；
5. 1 tick 边界（0.0009 通过 / 0.0011 不通过）；
6. 同一个 E 上的复合事件（先 cat11 后 cat1）；
7. 达到请求上限即停，并把剩余事件写进报告；
8. `--dry-run` 不联网（假 fetcher 被调用就直接让测试失败）；
9. 记录能过 PR-B 的校验规则（复用 `convert` 的 `validate_record`）。
"""

import importlib.util
import json
import os
import sqlite3
import tempfile
import unittest
import unittest.mock
from decimal import Decimal
from pathlib import Path

SCRIPT = Path(__file__).with_name("etf_factor_checks_build.py")
spec = importlib.util.spec_from_file_location("etf_factor_checks_build", SCRIPT)
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

TESTDATA = Path(__file__).with_name("testdata") / "announcements"

CHECKED_AT = "2026-11-09"
DIGEST_A = "1" * 64
DIGEST_B = "2" * 64
DIGEST_C = "3" * 64
DIGEST_D = "4" * 64


def fixture_text(name):
    return (TESTDATA / name).read_text(encoding="utf-8")


# ---------------------------------------------------------------------------
# 夹具：行情库 + 假 fetcher
# ---------------------------------------------------------------------------


class FakeFetcher(mod.Fetcher):
    """假网络：按 URL 前缀路由，记录每一个被请求的 URL。

    `forbidden=True` 时任何调用都直接让测试失败——`--dry-run` 用它证明「一次也
    没联网」。
    """

    def __init__(self, routes=None, pdf_text="", forbidden=False, fail_times=0):
        self.routes = dict(routes or {})
        self.pdf_text = pdf_text
        self.forbidden = forbidden
        self.fail_times = fail_times
        self.urls = []
        self.pdf_downloads = 0
        self.pdf_extractions = 0

    def _record(self, url):
        if self.forbidden:
            raise AssertionError(f"dry-run 不该发请求：{url}")
        self.urls.append(url)

    def get(self, url):
        self._record(url)
        if self.fail_times > 0:
            self.fail_times -= 1
            raise mod.FetchError("模拟网络错误")
        for prefix, body in self.routes.items():
            if prefix in url:
                if callable(body):
                    return body(url)
                return body
        raise mod.FetchError(f"没有配置这个 URL：{url}")

    def get_bytes(self, url):
        self._record(url)
        if self.fail_times > 0:
            self.fail_times -= 1
            raise mod.FetchError("模拟网络错误")
        self.pdf_downloads += 1
        return b"%PDF-1.4 fake"

    def pdftotext(self, pdf_bytes, name="ann.pdf"):
        assert pdf_bytes == b"%PDF-1.4 fake"
        self.pdf_extractions += 1
        return self.pdf_text


class FakeNoticeIndex:
    """假公告索引：候选由测试直接给，不碰 `szse_etf_notice_ann`。

    `budgeted=True` 时先向 `budget` 要一次请求（模拟线上「先查索引」的那一次），
    这样请求上限与重试的测试能覆盖到真实的请求计数路径。
    """

    def __init__(self, sse=(), szse=(), budgeted=False, fail_times=0):
        self.sse = list(sse)
        self.szse = list(szse)
        self.budgeted = budgeted
        self.fail_times = fail_times
        self.budget = None
        self.calls = []

    def _index_request(self):
        if not self.budgeted or self.budget is None:
            return
        self.budget.call("https://example.invalid/index?fail=1" if self.fail_times else
                         "https://example.invalid/index")

    def sse_candidates(self, entity, effective_day):
        self.calls.append(("sse", entity, effective_day))
        self._index_request()
        return list(self.sse)

    def szse_candidates(self, entity, effective_day):
        self.calls.append(("szse", entity, effective_day))
        self._index_request()
        return list(self.szse)


def official_payload(code, qss, ks=""):
    return json.dumps(
        [
            {
                "metadata": {"tabkey": "tab2", "pagecount": 1},
                "data": [{"zqdm": code, "zqjc": "ETF", "qss": qss, "ks": ks or qss}],
            }
        ],
        ensure_ascii=False,
    )


def build_db(path, entities, events, bars):
    """建一个最小的行情库夹具。

    `entities` 是 ETF 实体列表（`instrument_class=2`），`events` 是
    `(entity, ordinal, effective_day, category, c1, c3, digest)`，`bars` 是
    `{entity: [(day, close_raw)]}`（不复权，`price_scale=3`，即 raw/1000 元）。
    """
    conn = sqlite3.connect(path)
    conn.execute(
        "CREATE TABLE market_series(series_key INTEGER PRIMARY KEY, entity TEXT, "
        "period INTEGER, adjustment INTEGER, instrument_class INTEGER, price_scale INTEGER)"
    )
    conn.execute(
        "CREATE TABLE series_bars(series_key INTEGER, trade_day INTEGER, close_cents INTEGER)"
    )
    conn.execute(
        "CREATE TABLE adjustment_events(entity TEXT, ordinal INTEGER, "
        "effective_day INTEGER, category INTEGER, c1 TEXT, c2 TEXT, c3 TEXT, c4 TEXT, "
        "basis_status TEXT, evidence_digest BLOB, PRIMARY KEY(entity, ordinal))"
    )
    for index, entity in enumerate(entities, start=1):
        conn.execute(
            "INSERT INTO market_series VALUES(?,?,1,0,2,3)", (index, entity)
        )
        for day, close in bars.get(entity, []):
            conn.execute(
                "INSERT INTO series_bars VALUES(?,?,?)",
                (index, mod.epoch_day(day), close),
            )
    for entity, ordinal, day, category, c1, c3, digest in events:
        conn.execute(
            "INSERT INTO adjustment_events VALUES(?,?,?,?,?,?,?,?,'verified',?)",
            (
                entity,
                ordinal,
                mod.epoch_day(day),
                category,
                c1,
                "0",
                c3,
                "0",
                bytes.fromhex(digest),
            ),
        )
    conn.commit()
    conn.close()


class VerifyCase(unittest.TestCase):
    """这是主要的测试基类。"""

    def setUp(self):
        self._directory = tempfile.TemporaryDirectory()
        self.directory = self._directory.name
        self.db_path = os.path.join(self.directory, "fixture.sqlite")

    def tearDown(self):
        self._directory.cleanup()

    def pending(self, existing_keys, since=None):
        """直接从库里挑待核对事件（不联网）。"""
        conn = mod.open_read_only(self.db_path)
        try:
            etf_entities, _columns, selected = mod.read_db_events(conn)
            events = mod.fetch_events(conn, selected, etf_entities)
            bars = mod.load_bars(conn, {event["entity"] for event in events})
        finally:
            conn.close()
        for event in events:
            entity_bars = bars.get(event["entity"], [])
            if entity_bars and mod.epoch_day(event["effective_day"]) <= entity_bars[0][0]:
                event["skip_reason"] = "no_effect"
        return mod.select_pending_events(events, existing_keys, since)

    def run_verify(self, fetcher, notice_index=None, existing=None, **kwargs):
        kwargs.setdefault("checked_at", CHECKED_AT)
        kwargs.setdefault("sleep", lambda seconds: None)
        return mod.verify(
            self.db_path,
            existing,
            fetcher=fetcher,
            notice_index=notice_index,
            **kwargs,
        )


# ---------------------------------------------------------------------------
# 1/2：只处理缺记录的事件；evidence_digest 变了算新事件
# ---------------------------------------------------------------------------


class PendingSelectionTest(VerifyCase):
    def setUp(self):
        super().setUp()
        build_db(
            self.db_path,
            ["sh.510050", "sz.159919"],
            [
                ("sh.510050", 1, "2025-03-31", 1, "0.661", "0", DIGEST_A),
                ("sh.510050", 2, "2025-06-16", 1, "0.5", "0", DIGEST_B),
                ("sz.159919", 3, "2025-03-31", 1, "0.661", "0", DIGEST_C),
            ],
            {
                "sh.510050": [("2025-01-02", 4111), ("2025-03-28", 4111)],
                "sz.159919": [("2025-01-02", 4111), ("2025-03-28", 4111)],
            },
        )

    def test_events_with_a_matching_record_are_not_processed(self):
        existing = {("sh.510050", 1, DIGEST_A)}
        pending, skipped = self.pending(existing)
        self.assertEqual(
            [(event["entity"], event["ordinal"]) for event in pending],
            [("sh.510050", 2), ("sz.159919", 3)],
        )
        self.assertEqual(skipped["already_checked"], ["sh.510050#1"])

    def test_changed_evidence_digest_is_a_new_event(self):
        # 同一个 (entity, ordinal) 的记录，摘要变了 → 旧记录不再匹配。
        existing = {("sh.510050", 1, DIGEST_A)}
        pending, _skipped = self.pending(existing)
        self.assertNotIn(("sh.510050", 1), [(e["entity"], e["ordinal"]) for e in pending])
        # 库里这条事件的摘要变了（同一 ordinal、新摘要）：旧记录不再匹配。
        os.unlink(self.db_path)
        build_db(
            self.db_path,
            ["sh.510050", "sz.159919"],
            [
                ("sh.510050", 1, "2025-03-31", 1, "0.661", "0", DIGEST_D),
                ("sh.510050", 2, "2025-06-16", 1, "0.5", "0", DIGEST_B),
                ("sz.159919", 3, "2025-03-31", 1, "0.661", "0", DIGEST_C),
            ],
            {
                "sh.510050": [("2025-01-02", 4111)],
                "sz.159919": [("2025-01-02", 4111)],
            },
        )
        pending, skipped = self.pending(existing)
        self.assertIn(("sh.510050", 1), [(e["entity"], e["ordinal"]) for e in pending])
        self.assertEqual(
            [(event["entity"], event["ordinal"], event["evidence_digest"]) for event in pending
             if event["entity"] == "sh.510050" and event["ordinal"] == 1],
            [("sh.510050", 1, DIGEST_D)],
        )
        self.assertEqual(skipped["already_checked"], [])

    def test_all_events_checked_means_no_request_is_made(self):
        existing = {
            ("sh.510050", 1, DIGEST_A),
            ("sh.510050", 2, DIGEST_B),
            ("sz.159919", 3, DIGEST_C),
        }
        fetcher = FakeFetcher(forbidden=True)
        records, report = self.run_verify(
            fetcher, existing=existing_keys_path_writer(existing)
        )
        self.assertEqual(records, [])
        self.assertEqual(report["requests"]["count"], 0)
        self.assertEqual(report["skipped"]["already_checked"], 3)
        self.assertEqual(report["unprocessed"]["count"], 0)

    def test_since_limits_the_window_and_is_reported(self):
        pending, skipped = self.pending(set(), since="2025-05-01")
        self.assertEqual(
            [(event["entity"], event["ordinal"]) for event in pending],
            [("sh.510050", 2)],
        )
        self.assertEqual(len(skipped["before_since"]), 2)
        for row in skipped["before_since"]:
            self.assertEqual(row["effective_day"], "2025-03-31")

    def test_event_before_the_first_bar_is_not_checked(self):
        build_db(
            os.path.join(self.directory, "before.sqlite"),
            ["sh.510010"],
            [("sh.510010", 0, "2009-12-08", 11, "0", "1.113193", DIGEST_A)],
            {"sh.510010": [("2009-12-15", 1000)]},
        )
        self.db_path = os.path.join(self.directory, "before.sqlite")
        pending, skipped = self.pending(set())
        self.assertEqual(pending, [])
        self.assertEqual(len(skipped["no_effect"]), 1)


def existing_keys_path_writer(keys):
    """把键写成 JSONL 已有记录文件（ordering: 只写匹配键用不到的字段也可）。"""
    directory = tempfile.mkdtemp()
    path = os.path.join(directory, "existing.jsonl")
    with open(path, "w", encoding="utf-8") as handle:
        for entity, ordinal, digest in sorted(keys):
            handle.write(
                json.dumps(
                    {"entity": entity, "ordinal": ordinal, "evidence_digest": digest},
                    ensure_ascii=False,
                )
            )
            handle.write("\n")
    return path


# ---------------------------------------------------------------------------
# 3：深市官方核对通过 / 不通过
# ---------------------------------------------------------------------------


class OfficialCheckTest(VerifyCase):
    def _case(self, qss):
        build_db(
            self.db_path,
            ["sz.159919"],
            [("sz.159919", 1, "2025-03-31", 1, "0.661", "0", DIGEST_A)],
            {"sz.159919": [("2025-03-28", 4111)]},
        )
        fetcher = FakeFetcher(
            {mod.SZSE_OFFICIAL_URL_TEMPLATE.split("?")[0]: official_payload("159919", qss)}
        )
        records, report = self.run_verify(fetcher, notice_index=FakeNoticeIndex())
        return records[0], report, fetcher

    def test_official_qss_within_one_tick_is_verified(self):
        # 4.111 − 0.0661 = 4.0449，官方 qss 4.045 → 差 0.0001，通过。
        record, report, _fetcher = self._case("4.045")
        self.assertEqual(record["check_status"], "verified_official")
        self.assertEqual(record["basis"], "official_qss")
        self.assertEqual(record["basis_ref"], "szse_1815_stock:2025-03-31")
        self.assertEqual(record["announced_value"], "4.045")
        self.assertEqual(record["price_delta"], "0.0001")
        self.assertTrue(record["within_one_tick"])
        self.assertEqual(mod.validate_record(record), [])
        self.assertEqual(report["requests"]["count"], 1)

    def test_official_qss_over_one_tick_stays_unverified(self):
        # 4.111 − 0.661/10 = 4.0449，官方 qss 4.046 → 差 0.0011，不通过。
        record, report, _fetcher = self._case("4.046")
        self.assertEqual(record["check_status"], "unverified")
        self.assertEqual(record["basis"], "official_qss")
        self.assertIn("delta_over_tick", record["basis_ref"])
        self.assertFalse(record["within_one_tick"])
        self.assertEqual(record["price_delta"], "0.0011")
        self.assertEqual(report["by_check_status"], {"unverified": 1})
        self.assertEqual(report["unverified_entities"], ["sz.159919"])
        self.assertEqual(mod.validate_record(record), [])

    def test_official_row_for_another_code_is_ignored(self):
        build_db(
            self.db_path,
            ["sz.159919"],
            [("sz.159919", 1, "2025-03-31", 1, "0.661", "0", DIGEST_A)],
            {"sz.159919": [("2025-03-28", 4111)]},
        )
        fetcher = FakeFetcher(
            {
                mod.SZSE_OFFICIAL_URL_TEMPLATE.split("?")[0]: official_payload(
                    "159922", "2.334", "2.461"
                )
            }
        )
        index = FakeNoticeIndex()
        records, report = self.run_verify(fetcher, notice_index=index)
        record = records[0]
        # 官方那一页没有这只 ETF 的行，也没有公告：事件不跳过，记 unverified。
        self.assertEqual(record["check_status"], "unverified")
        self.assertIn("no_announcement_with_value", record["basis_ref"])
        self.assertEqual(report["requests"]["count"], mod.SZSE_OFFICIAL_MAX_PAGES)
        self.assertEqual(mod.validate_record(record), [])


# ---------------------------------------------------------------------------
# 4：公告解析（纯函数 + 文本夹具）
# ---------------------------------------------------------------------------


class AnnouncementParsingTest(unittest.TestCase):
    def test_amount_per_10_shares(self):
        terms = mod.parse_announcement_text(
            fixture_text("sz_159919_2025-03-28.txt")
        )
        self.assertEqual(terms.mode, "per10")
        self.assertEqual(terms.amount_per10, Decimal("0.661"))
        self.assertEqual(terms.registration_day, "2025-03-28")
        self.assertEqual(terms.ex_day, "2025-03-31")
        self.assertEqual(mod.announced_value_text(terms, 1), "0.661")

    def test_amount_per_share_is_scaled_to_ten_shares(self):
        terms = mod.parse_announcement_text(fixture_text("sh_510050_2025-03-24.txt"))
        self.assertEqual(terms.mode, "per_share")
        self.assertEqual(terms.amount_per_share, Decimal("0.0129"))
        # 与 `c1` 同口径：0.0129 元/份 = 0.129 元/10 份。
        self.assertEqual(terms.payable_per10(), Decimal("0.129"))
        self.assertEqual(mod.announced_value_text(terms, 1), "0.129")

    def test_fullwidth_decimal_point_is_a_full_stop(self):
        # sh.511880 2014-12-30：公告写「42．28」，VM2-23 曾解析成 42.0。
        terms = mod.parse_announcement_text(fixture_text("sh_511880_2014-12-30.txt"))
        self.assertEqual(terms.amount_per10, Decimal("42.28"))
        self.assertEqual(mod.announced_value_text(terms, 1), "42.28")

    def test_split_direction_gives_a_ratio_above_one(self):
        # sh.510180 2009-05-20：公告「按 10：1 拆分、增加基金份额」，c3 = 10。
        terms = mod.parse_announcement_text(fixture_text("sh_510180_2009-05-20.txt"))
        self.assertEqual(terms.mode, "ratio")
        self.assertEqual(terms.ratio, Decimal("10"))
        self.assertIn("拆分", terms.note)
        self.assertEqual(mod.announced_value_text(terms, 11), "10")
        self.assertEqual(terms.ex_day, "2009-05-20")

    def test_merge_direction_gives_a_ratio_below_one(self):
        # 「1：10 合并、减少基金份额」= 10 份并 1 份 → c3 = 0.1。
        terms = mod.parse_announcement_text(
            "本基金按 1：10 合并、减少基金份额。除权日：2015 年 6 月 1 日"
        )
        self.assertEqual(terms.mode, "ratio")
        self.assertEqual(terms.ratio, Decimal("0.1"))
        self.assertIn("合并", terms.note)

    def test_a_direction_word_that_contradicts_the_numbers_is_refused(self):
        # 「10：1 合并」自相矛盾（合并要求 c3 < 1）：宁可不认，也不猜反方向。
        terms = mod.parse_announcement_text("本基金按 10：1 合并、减少基金份额。")
        self.assertIsNone(terms.ratio)
        self.assertIn("不小于 1", terms.note)

    def test_ratio_without_a_direction_word_is_read_as_a_split(self):
        terms = mod.parse_announcement_text("基金份额折算比例：1：0.4，折算日：2024-12-02")
        self.assertEqual(terms.mode, "ratio")
        self.assertEqual(terms.ratio, Decimal("2.5"))
        self.assertIn("无方向词", terms.note)

    def test_text_without_a_value_yields_nothing(self):
        terms = mod.parse_announcement_text("本基金自 2025 年 3 月 31 日起调整申赎清单。")
        self.assertEqual(terms.mode, "")
        self.assertIsNone(mod.announcement_factor(terms, 1))
        self.assertIsNone(mod.announcement_factor(terms, 11))

    def test_fixtures_carry_no_pdf_bytes(self):
        # 夹具是文本；仓库里不放 PDF。
        for path in TESTDATA.iterdir():
            self.assertEqual(path.suffix, ".txt", path)


# ---------------------------------------------------------------------------
# 4b：公告核对（用假 PDF 文本走完整流程）
# ---------------------------------------------------------------------------


class AnnouncementVerificationTest(VerifyCase):
    def _case(self, fixture, gbbq_c1, prev_close_raw, category=1, entity="sh.510050",
              c3="0"):
        build_db(
            self.db_path,
            [entity],
            [(entity, 1, "2025-03-31", category, gbbq_c1, c3, DIGEST_A)],
            {entity: [("2025-03-28", prev_close_raw)]},
        )
        fetcher = FakeFetcher(pdf_text=fixture_text(fixture))
        index = FakeNoticeIndex(
            sse=[
                mod.NoticeCandidate(
                    "2025-03-25", "xxx 收益分配公告", "https://example.invalid/a.pdf", "sse_ann"
                )
            ]
        )
        records, report = self.run_verify(fetcher, notice_index=index)
        return records[0], report, fetcher, index

    def test_announcement_value_matching_gbbq_is_verified(self):
        record, report, fetcher, index = self._case(
            "sz_159919_2025-03-28.txt", "0.661", 4111
        )
        self.assertEqual(record["check_status"], "verified_announcement")
        self.assertEqual(record["basis"], "announcement")
        self.assertEqual(record["basis_ref"], "sse_ann:2025-03-25")
        self.assertEqual(record["announced_value"], "0.661")
        self.assertEqual(record["price_delta"], "0")
        self.assertTrue(record["within_one_tick"])
        self.assertEqual(mod.validate_record(record), [])
        # `FakeNoticeIndex` 不占请求名额（线上「查索引」那一次由 budget 计），
        # 所以这里只有 PDF 下载这一次。
        self.assertEqual(report["requests"]["count"], 1)
        self.assertEqual(fetcher.pdf_downloads, 1)
        self.assertEqual(index.calls, [("sse", "sh.510050", "2025-03-31")])

    def test_announcement_value_far_from_gbbq_is_unverified_but_keeps_the_delta(self):
        record, _report, _fetcher, _index = self._case(
            "sz_159919_2025-03-28.txt", "0.700", 4111
        )
        self.assertEqual(record["check_status"], "unverified")
        self.assertEqual(record["basis"], "announcement")
        self.assertEqual(record["basis_ref"], "announcement_delta_over_tick")
        # 4.111 − 0.0700 = 4.0410，公告值 4.111 − 0.0661 = 4.0449，差 0.0039。
        self.assertEqual(record["price_delta"], "-0.0039")
        self.assertFalse(record["within_one_tick"])
        self.assertEqual(mod.validate_record(record), [])

    def test_no_announcement_with_a_value_is_unverified_not_skipped(self):
        build_db(
            self.db_path,
            ["sh.510050"],
            [("sh.510050", 1, "2025-03-31", 1, "0.661", "0", DIGEST_A)],
            {"sh.510050": [("2025-03-28", 4111)]},
        )
        fetcher = FakeFetcher(pdf_text="本基金管理人提示投资者注意投资风险。")
        index = FakeNoticeIndex(
            sse=[mod.NoticeCandidate("2025-03-25", "提示公告", "https://e.invalid/a.pdf", "sse_ann")]
        )
        records, report = self.run_verify(fetcher, notice_index=index)
        record = records[0]
        self.assertEqual(record["check_status"], "unverified")
        self.assertEqual(record["basis"], "none")
        self.assertIn("no_announcement_with_value", record["basis_ref"])
        self.assertEqual(record["evidence_sha256"], "")
        self.assertEqual(report["records"], 1)
        self.assertEqual(mod.validate_record(record), [])


# ---------------------------------------------------------------------------
# 5：1 tick 边界
# ---------------------------------------------------------------------------


class OneTickBoundaryTest(VerifyCase):
    def _official(self, qss):
        if os.path.exists(self.db_path):
            os.unlink(self.db_path)
        build_db(
            self.db_path,
            ["sz.159919"],
            [("sz.159919", 1, "2025-03-31", 1, "0.661", "0", DIGEST_A)],
            {"sz.159919": [("2025-03-28", 4111)]},
        )
        fetcher = FakeFetcher(
            {mod.SZSE_OFFICIAL_URL_TEMPLATE.split("?")[0]: official_payload("159919", qss)}
        )
        records, _report = self.run_verify(fetcher, notice_index=FakeNoticeIndex())
        return records[0]

    def test_boundary_0009_passes_and_0011_fails(self):
        passed = self._official("4.0440")  # 4.0449 − 4.0440 = 0.0009
        self.assertEqual(passed["check_status"], "verified_official")
        self.assertEqual(passed["price_delta"], "-0.0009")
        self.assertTrue(passed["within_one_tick"])
        failed = self._official("4.0438")  # 4.0449 − 4.0438 = 0.0011
        self.assertEqual(failed["check_status"], "unverified")
        self.assertEqual(failed["price_delta"], "-0.0011")
        self.assertFalse(failed["within_one_tick"])

    def test_exactly_one_tick_still_passes(self):
        record = self._official("4.0439")  # 差正好 0.001
        self.assertEqual(record["check_status"], "verified_official")
        self.assertEqual(record["price_delta"], "-0.001")
        self.assertTrue(record["within_one_tick"])


# ---------------------------------------------------------------------------
# 6：同一个 E 上的复合事件
# ---------------------------------------------------------------------------


class CompositeVerifyTest(VerifyCase):
    def setUp(self):
        super().setUp()
        build_db(
            self.db_path,
            ["sz.159922"],
            [
                ("sz.159922", 1198, "2024-12-02", 11, "0", "2.5", DIGEST_A),
                ("sz.159922", 1199, "2024-12-02", 1, "1.292", "0", DIGEST_B),
            ],
            {"sz.159922": [("2024-11-29", 6159)]},
        )

    def test_composite_is_checked_once_and_merged(self):
        # 6.159 / 2.5 − 0.1292 = 2.3344，官方 qss 2.334 → 差 0.0004。
        fetcher = FakeFetcher(
            {
                mod.SZSE_OFFICIAL_URL_TEMPLATE.split("?")[0]: official_payload(
                    "159922", "2.334", "2.461"
                )
            }
        )
        records, report = self.run_verify(fetcher, notice_index=FakeNoticeIndex())
        self.assertEqual(len(records), 2)
        for record in records:
            self.assertEqual(record["check_status"], "verified_official")
            self.assertIn("same_E_composite", record["basis_ref"])
            self.assertEqual(record["price_delta"], "-0.0004")
            self.assertTrue(record["within_one_tick"])
            self.assertEqual(mod.validate_record(record), [])
        self.assertEqual(
            {record["ordinal"]: record["gbbq_value"] for record in records},
            {1198: "2.5", 1199: "1.292"},
        )
        # 同一个 E 只取一次官方日行情（不是每条事件一次）。
        self.assertEqual(report["requests"]["count"], 1)

    def test_composite_reverse_order_would_not_match(self):
        reverse = (Decimal("6.159") - Decimal("1.292") / 10) / Decimal("2.5")
        self.assertEqual(mod.decimal_text(reverse, mod.DELTA_SCALE), "2.41192")
        self.assertFalse(mod.within_one_tick(Decimal("2.334") - reverse))

    def test_composite_without_official_row_is_unverified_for_both(self):
        fetcher = FakeFetcher({mod.SZSE_OFFICIAL_URL_TEMPLATE.split("?")[0]: json.dumps([])})
        records, report = self.run_verify(fetcher, notice_index=FakeNoticeIndex())
        self.assertEqual(len(records), 2)
        for record in records:
            self.assertEqual(record["check_status"], "unverified")
            self.assertIn("same_E_composite_no_official", record["basis_ref"])
            self.assertEqual(mod.validate_record(record), [])
        self.assertEqual(report["unverified_entities"], ["sz.159922"])

    def test_only_the_missing_event_of_a_group_is_requested(self):
        # 已有 cat11 的记录，只缺 cat1：整组仍要合并核对（一条事件单独核不出来），
        # 但报告里只写一条记录，另一条命中已有键。
        existing = {
            ("sz.159922", 1198, DIGEST_A),
        }
        fetcher = FakeFetcher(
            {
                mod.SZSE_OFFICIAL_URL_TEMPLATE.split("?")[0]: official_payload(
                    "159922", "2.334", "2.461"
                )
            }
        )
        records, report = self.run_verify(
            fetcher,
            existing=existing_keys_path_writer(existing),
            notice_index=FakeNoticeIndex(),
        )
        self.assertEqual([record["ordinal"] for record in records], [1199])
        self.assertEqual(report["skipped"]["already_checked"], 1)


# ---------------------------------------------------------------------------
# 7：请求上限
# ---------------------------------------------------------------------------


class RequestBudgetTest(VerifyCase):
    def setUp(self):
        super().setUp()
        build_db(
            self.db_path,
            ["sh.510050", "sh.510060", "sh.510070"],
            [
                ("sh.510050", 1, "2025-03-31", 1, "0.661", "0", DIGEST_A),
                ("sh.510060", 1, "2025-03-31", 1, "0.661", "0", DIGEST_B),
                ("sh.510070", 1, "2025-03-31", 1, "0.661", "0", DIGEST_C),
            ],
            {
                "sh.510050": [("2025-03-28", 4111)],
                "sh.510060": [("2025-03-28", 4111)],
                "sh.510070": [("2025-03-28", 4111)],
            },
        )

    def test_hard_cap_stops_and_the_rest_is_reported(self):
        fetcher = FakeFetcher(pdf_text="本基金管理人提示投资者注意投资风险。")
        index = FakeNoticeIndex(
            sse=[mod.NoticeCandidate("2025-03-25", "提示公告", "https://e.invalid/a.pdf", "sse_ann")]
        )
        records, report = self.run_verify(
            fetcher, notice_index=index, max_requests=2
        )
        self.assertEqual(report["requests"]["count"], 2)
        self.assertEqual(report["requests"]["max"], 2)
        self.assertEqual(len(records), 2)
        self.assertEqual(report["unprocessed"]["count"], 1)
        self.assertEqual(
            [(row["entity"], row["ordinal"]) for row in report["unprocessed"]["rows"]],
            [("sh.510070", 1)],
        )
        self.assertEqual(
            report["unprocessed"]["rows"][0]["reason"], "request_budget_exhausted"
        )
        self.assertFalse(report["ok"])
        for record in records:
            self.assertEqual(mod.validate_record(record), [])

    def test_retries_count_against_the_cap(self):
        # 索引请求失败 → 重试也计 1 次；上限 1 时重试发不出去，全部事件未处理。
        fetcher = FakeFetcher()
        index = FakeNoticeIndex(budgeted=True, fail_times=1)
        records, report = self.run_verify(
            fetcher, notice_index=index, max_requests=1
        )
        # 主请求已用掉唯一的 1 个名额，重试的 _spend 直接抛预算异常。
        self.assertEqual(report["requests"]["count"], 1)
        self.assertEqual(report["requests"]["retries"], 1)
        self.assertEqual(records, [])
        self.assertEqual(report["unprocessed"]["count"], 3)

    def test_retry_happens_once_and_then_succeeds(self):
        fetcher = FakeFetcher(
            {"https://example.invalid/index": "{}"},
            pdf_text="每 10 份基金份额派发现金 0.661 元",
        )
        index = FakeNoticeIndex(
            sse=[mod.NoticeCandidate("2025-03-25", "公告", "https://e.invalid/a.pdf", "sse_ann")],
            budgeted=True,
        )
        slept = []
        _records, report = self.run_verify(
            fetcher,
            notice_index=index,
            max_requests=10,
            sleep=lambda seconds: slept.append(seconds),
        )
        # 每个事件：索引 1 次 + PDF 1 次（提取文本不算网络请求）= 6。
        self.assertEqual(report["requests"]["count"], 6)
        self.assertEqual(report["requests"]["retries"], 0)
        self.assertEqual(fetcher.pdf_downloads, 3)
        self.assertEqual(report["requests"]["retries"], 0)

    def test_interval_and_backoff_are_waited_out(self):
        # 先让 fetcher 失败一次，看退避 ≥ 30 s，然后每次间隔 ≥ 1 s。
        slept = []
        now = [0.0]
        fetcher = FakeFetcher(fail_times=1)

        def clock():
            return now[0]

        def sleep(seconds):
            slept.append(seconds)
            now[0] += seconds

        index = FakeNoticeIndex(budgeted=True)
        self.run_verify(
            fetcher,
            notice_index=index,
            sleep=sleep,
            clock=clock,
            min_interval=1.0,
        )
        self.assertIn(mod.RETRY_BACKOFF_SECONDS, slept)
        self.assertTrue(all(seconds >= 1.0 for seconds in slept))
        self.assertGreaterEqual(mod.MIN_REQUEST_INTERVAL_SECONDS, 1.0)


# ---------------------------------------------------------------------------
# 8：--dry-run 不联网
# ---------------------------------------------------------------------------


class DryRunTest(VerifyCase):
    def setUp(self):
        super().setUp()
        build_db(
            self.db_path,
            ["sh.510050", "sz.159919"],
            [
                ("sh.510050", 1, "2025-03-31", 1, "0.661", "0", DIGEST_A),
                ("sz.159919", 1, "2025-03-31", 1, "0.661", "0", DIGEST_B),
            ],
            {
                "sh.510050": [("2025-03-28", 4111)],
                "sz.159919": [("2025-03-28", 4111)],
            },
        )

    def test_dry_run_never_touches_the_fetcher(self):
        fetcher = FakeFetcher(forbidden=True)  # 被调用就直接 AssertionError
        records, report = self.run_verify(
            fetcher, notice_index=FakeNoticeIndex(), dry_run=True
        )
        self.assertEqual(records, [])
        self.assertTrue(report["dry_run"])
        self.assertEqual(report["requests"]["count"], 0)
        self.assertEqual(fetcher.urls, [])
        self.assertEqual(report["planned_requests"]["count"], 2)
        self.assertEqual(report["planned_requests"]["by_market"], {"sh": 1, "sz": 1})
        self.assertEqual(report["unprocessed"]["count"], 2)
        self.assertTrue(
            all(row["reason"] == "dry_run" for row in report["unprocessed"]["rows"])
        )

    def test_dry_run_lists_url_templates_without_requesting(self):
        _records, report = self.run_verify(
            FakeFetcher(forbidden=True), notice_index=FakeNoticeIndex(), dry_run=True
        )
        templates = [
            template
            for row in report["planned_requests"]["rows"]
            for template in row["url_templates"]
        ]
        self.assertTrue(any("1815_stock" in item for item in templates))
        self.assertTrue(
            any("COMMON_PL_JJXX_JJGG_L" in item for item in templates)
        )
        # 模板里的日期参数没有被真实日期替换掉（它就是模板）。
        self.assertTrue(any("{E}" in item for item in templates))
        self.assertTrue(any("{E-30}" in item for item in templates))

    def test_dry_run_cli_writes_no_jsonl_and_exits_zero(self):
        out = os.path.join(self.directory, "out.jsonl")
        report_path = os.path.join(self.directory, "report.json")
        with unittest.mock.patch.object(
            mod, "HttpFetcher", side_effect=AssertionError("dry-run 不该构造 HttpFetcher")
        ):
            code = mod.main(
                [
                    "verify",
                    "--db",
                    self.db_path,
                    "--out",
                    out,
                    "--report",
                    report_path,
                    "--dry-run",
                    "--checked-at",
                    CHECKED_AT,
                ]
            )
        self.assertEqual(code, mod.EXIT_OK)
        self.assertFalse(os.path.exists(out))
        with open(report_path, encoding="utf-8") as handle:
            report = json.load(handle)
        self.assertTrue(report["dry_run"])
        self.assertEqual(report["planned_requests"]["count"], 2)


# ---------------------------------------------------------------------------
# 9：已有记录文件的读取与 CLI
# ---------------------------------------------------------------------------


class ExistingRecordsTest(VerifyCase):
    def setUp(self):
        super().setUp()
        build_db(
            self.db_path,
            ["sh.510050"],
            [("sh.510050", 1, "2025-03-31", 1, "0.661", "0", DIGEST_A)],
            {"sh.510050": [("2025-03-28", 4111)]},
        )

    def test_jsonl_existing_records_are_read_as_keys(self):
        path = os.path.join(self.directory, "existing.jsonl")
        with open(path, "w", encoding="utf-8") as handle:
            handle.write(
                json.dumps(
                    {
                        "record_version": mod.RECORD_VERSION,
                        "entity": "sh.510050",
                        "ordinal": 1,
                        "effective_day": "2025-03-31",
                        "category": 1,
                        "check_status": "verified_announcement",
                        "basis": "announcement",
                        "basis_ref": "sse_ann:2025-03-25",
                        "evidence_url": "https://example.invalid/a.pdf",
                        "evidence_sha256": DIGEST_D,
                        "announced_value": "0.661",
                        "gbbq_value": "0.661",
                        "price_delta": "",
                        "within_one_tick": None,
                        "checked_at": CHECKED_AT,
                        "evidence_digest": DIGEST_A,
                    }
                )
                + "\n"
            )
        keys = mod.load_existing_records(path)
        self.assertEqual(keys, {("sh.510050", 1, DIGEST_A)})

    def test_bad_existing_jsonl_is_an_input_error(self):
        path = os.path.join(self.directory, "bad.jsonl")
        with open(path, "w", encoding="utf-8") as handle:
            handle.write("{not json}\n")
        with self.assertRaises(mod.InputError):
            mod.load_existing_records(path)

    def test_missing_existing_file_is_an_input_error(self):
        with self.assertRaises(mod.InputError):
            mod.load_existing_records(os.path.join(self.directory, "nope.jsonl"))

    def test_store_directory_reads_manifest_and_blobs(self):
        store = os.path.join(self.directory, "store")
        os.makedirs(os.path.join(store, "blobs"))
        record = {
            "record_version": mod.RECORD_VERSION,
            "entity": "sh.510050",
            "ordinal": 1,
            "effective_day": "2025-03-31",
            "category": 1,
            "check_status": "verified_announcement",
            "basis": "announcement",
            "basis_ref": "sse_ann:2025-03-25",
            "evidence_url": "https://example.invalid/a.pdf",
            "evidence_sha256": DIGEST_D,
            "announced_value": "0.661",
            "gbbq_value": "0.661",
            "price_delta": "",
            "within_one_tick": None,
            "checked_at": CHECKED_AT,
            "evidence_digest": DIGEST_A,
        }
        body = mod.canonical_json(record).encode("utf-8")
        sha = mod.sha256_hex(body)
        with open(os.path.join(store, "blobs", sha), "wb") as handle:
            handle.write(body)
        with open(os.path.join(store, "manifest.jsonl"), "w", encoding="utf-8") as handle:
            handle.write(
                json.dumps(
                    {
                        "source_id": "etf_factor_checks",
                        "origin": "etf-factor-checks:" + sha,
                    }
                )
                + "\n"
            )
        self.assertEqual(mod.load_existing_records(store), {("sh.510050", 1, DIGEST_A)})

    def test_cli_writes_records_and_report(self):
        out = os.path.join(self.directory, "out", "checks.jsonl")
        report_path = os.path.join(self.directory, "out", "report.json")
        fetcher = FakeFetcher(
            {mod.SSE_ANNOUNCEMENT_URL_TEMPLATE.split("?")[0]: json.dumps({"result": []})}
        )
        with unittest.mock.patch.object(mod, "HttpFetcher", return_value=fetcher):
            code = mod.main(
                [
                    "verify",
                    "--db",
                    self.db_path,
                    "--out",
                    out,
                    "--report",
                    report_path,
                    "--checked-at",
                    CHECKED_AT,
                    "--request-interval",
                    "0",
                ]
            )
        self.assertEqual(code, mod.EXIT_OK)
        with open(out, encoding="utf-8") as handle:
            lines = [line for line in handle.read().splitlines() if line]
        self.assertEqual(len(lines), 1)
        record = json.loads(lines[0])
        self.assertEqual(mod.validate_record(record), [])
        self.assertEqual(tuple(record), mod.CANONICAL_FIELDS)
        with open(report_path, encoding="utf-8") as handle:
            report = json.load(handle)
        self.assertEqual(report["records"], 1)
        self.assertEqual(report["requests"]["count"], 1)
        self.assertGreaterEqual(report["requests"]["min_interval_seconds"], 1.0)

    def test_cli_exits_three_when_events_are_left_unprocessed(self):
        out = os.path.join(self.directory, "checks.jsonl")
        fetcher = FakeFetcher(
            {mod.SSE_ANNOUNCEMENT_URL_TEMPLATE.split("?")[0]: json.dumps({"result": []})}
        )
        with unittest.mock.patch.object(mod, "HttpFetcher", return_value=fetcher):
            code = mod.main(
                [
                    "verify",
                    "--db",
                    self.db_path,
                    "--out",
                    out,
                    "--checked-at",
                    CHECKED_AT,
                    "--max-requests",
                    "0",
                ]
            )
        self.assertEqual(code, mod.EXIT_NEEDS_REVIEW)

    def test_cli_missing_db_exits_two(self):
        code = mod.main(
            [
                "verify",
                "--db",
                os.path.join(self.directory, "nope.sqlite"),
                "--out",
                os.path.join(self.directory, "out.jsonl"),
            ]
        )
        self.assertEqual(code, mod.EXIT_INPUT_ERROR)


class RequestBudgetUnitTest(unittest.TestCase):
    """`RequestBudget` 的可替换时钟与 sleep。"""

    def test_it_sleeps_before_the_second_request(self):
        slept = []
        now = [0.0]

        def clock():
            return now[0]

        def sleep(seconds):
            slept.append(seconds)
            now[0] += seconds

        fetcher = FakeFetcher({"example": "ok"})
        budget = mod.RequestBudget(
            fetcher, max_requests=3, min_interval=1.0, sleep=sleep, clock=clock
        )
        budget.call("https://example.invalid/1")
        budget.call("https://example.invalid/2")
        self.assertEqual(slept, [1.0])
        self.assertEqual(budget.requests, 2)

    def test_over_budget_raises_before_calling_the_fetcher(self):
        fetcher = FakeFetcher({"example": "ok"})
        budget = mod.RequestBudget(fetcher, max_requests=1, sleep=lambda s: None)
        budget.call("https://example.invalid/1")
        with self.assertRaises(mod.RequestBudgetExceeded):
            budget.call("https://example.invalid/2")
        self.assertEqual(budget.requests, 1)
        self.assertEqual(fetcher.urls, ["https://example.invalid/1"])

    def test_retry_is_bounded(self):
        fetcher = FakeFetcher({"example": "ok"}, fail_times=5)
        budget = mod.RequestBudget(
            fetcher, max_requests=10, backoff=0.0, sleep=lambda s: None
        )
        with self.assertRaises(mod.FetchError):
            budget.call("https://example.invalid/1")
        # 主请求 1 次 + 重试 1 次。
        self.assertEqual(budget.requests, 2)
        self.assertEqual(budget.retries, 1)

    def test_user_agent_is_the_exchange_source_one(self):
        self.assertEqual(mod.USER_AGENT, "x-data-mcp-exchange-source/1")


if __name__ == "__main__":
    unittest.main()
