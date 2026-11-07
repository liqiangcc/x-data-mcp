"""Unit tests for the offline SSE ETF short-name extractor (PR-4, XDM-203).

All fixtures are synthetic text or synthetic files in a tempdir: no PDF bytes
and no network are needed. The extraction rule must never substitute a fund's
full name from an announcement title, and an unavailable name must stay empty.

The `V2*` cases are the real line shapes from the XDM-201 §3 audit of the 82
SSE termination PDFs: the nine wrong v1 names (prose, 以下简称, 基金代码,
扩位简称, full-width comma, 、, a wrapped line) and the two names v1 missed
(sh.510110, sh.511230), plus the page-boundary bug where v1 probed page 3 of a
two-page PDF.
"""

import csv
import hashlib
import importlib.util
import tempfile
import unittest
import unittest.mock
from pathlib import Path

SCRIPT = Path(__file__).with_name("extract_etf_short_names.py")
spec = importlib.util.spec_from_file_location("extract_etf_short_names", SCRIPT)
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)


def csv_bytes(rows, header=None):
    header = header or list(mod.D72_REQUIRED) + ["last_operating_day", "registration_date"]
    out = []
    import io

    buf = io.StringIO(newline="")
    writer = csv.DictWriter(buf, fieldnames=header)
    writer.writeheader()
    for row in rows:
        writer.writerow({key: row.get(key, "") for key in header})
    out = buf.getvalue().encode("utf-8")
    return out


def sse_row(entity="sh.510070", sha="a" * 64, **overrides):
    row = {
        "entity": entity,
        "delisting_date": "2020-11-20",
        "announcement_date": "2020-11-13",
        "source_id": "sse_fund_delist_ann",
        "doc_url": "https://example.invalid/a.pdf",
        "doc_sha256": sha,
        "page_no": "1",
        "snippet": "终止上市日：2020年11月20日",
        "extractor": "d72-v1",
        "extracted_at": "2026-11-05T12:00:00Z",
        "confidence": "high",
        "last_operating_day": "2020-11-19",
        "registration_date": "",
        **overrides,
    }
    return row


class CleanShortName(unittest.TestCase):
    def test_layout_spaces_are_removed(self):
        self.assertEqual(mod.clean_short_name("民企 ETF"), "民企ETF")
        self.assertEqual(mod.clean_short_name("永赢中证 500ETF"), "永赢中证500ETF")
        self.assertEqual(mod.clean_short_name("京津冀基"), "京津冀基")
        self.assertEqual(mod.clean_short_name("汽配 ETF"), "汽配ETF")

    def test_empty_and_too_long_are_refused(self):
        self.assertIsNone(mod.clean_short_name("   "))
        self.assertIsNone(mod.clean_short_name("x" * 21))

    def test_fund_full_name_is_never_a_short_name(self):
        self.assertIsNone(
            mod.clean_short_name("鹏扬国证财富管理交易型开放式指数证券投资基金")
        )
        self.assertIsNone(mod.clean_short_name("上证民营企业50交易型开放式指数证券投资基金"))

    def test_trailing_double_space_columns_are_cut(self):
        self.assertEqual(mod.clean_short_name("民企 ETF    2020-11-13"), "民企ETF")


class ExtractFromText(unittest.TestCase):
    def test_full_width_colon(self):
        found = mod.extract_from_text("基金终止上市公告\n场内简称：民企 ETF\n二〇二〇年")
        self.assertEqual(found[0], "民企ETF")
        self.assertIn("场内简称", found[1])

    def test_no_colon(self):
        found = mod.extract_from_text("场内简称 永赢中证 500ETF\n")
        self.assertEqual(found[0], "永赢中证500ETF")

    def test_half_width_colon_and_spaces(self):
        found = mod.extract_from_text("场内简称 : 财富管理 ETF\n")
        self.assertEqual(found[0], "财富管理ETF")

    def test_no_label_returns_none(self):
        self.assertIsNone(
            mod.extract_from_text("关于XX交易型开放式指数证券投资基金终止上市的公告")
        )

    def test_fund_short_name_label_is_not_used_as_fallback(self):
        # A PDF can contain both labels (取证 §6, sz.159503). Only 场内简称
        # is the on-exchange short name; 基金简称 is a different, longer name.
        text = "基金简称：鹏扬国证财富管理 ETF\n场内简称：财富管理 ETF\n"
        self.assertEqual(mod.extract_from_text(text)[0], "财富管理ETF")
        self.assertIsNone(mod.extract_from_text("基金简称：鹏扬国证财富管理 ETF\n"))

    def test_full_name_capture_is_refused(self):
        text = "场内简称：上证民营企业50交易型开放式指数证券投资基金\n"
        self.assertIsNone(mod.extract_from_text(text))


class ProcessCsv(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.pdfs = self.root / "pdfs"
        self.pdfs.mkdir()
        self.csv_in = self.root / "in.csv"
        self.csv_out = self.root / "out.csv"

    def tearDown(self):
        self.tmp.cleanup()

    def _fake_pdf(self, sha, text, name=None):
        """A fake PDF blob plus a monkeypatched pdftotext that returns `text`."""
        path = self.pdfs / (name or f"{sha}.pdf")
        path.write_bytes(b"%PDF-1.4 synthetic\n")
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        original = mod.page_text
        mod.page_text = lambda pdf, page: text if page == 1 else ""
        self.addCleanup(lambda: setattr(mod, "page_text", original))
        return digest

    def test_sse_row_gets_name_and_all_evidence_columns(self):
        body = b"%PDF-1.4 synthetic\n"
        digest = hashlib.sha256(body).hexdigest()
        (self.pdfs / f"{digest}.pdf").write_bytes(body)
        row = sse_row(sha=digest)
        self.csv_in.write_bytes(csv_bytes([row]))
        original = mod.page_text
        mod.page_text = lambda pdf, page: "场内简称：民企 ETF\n" if page == 1 else ""
        self.addCleanup(lambda: setattr(mod, "page_text", original))
        stats = mod.process(self.csv_in, self.pdfs, self.csv_out)
        self.assertEqual(stats["named"], 1)
        self.assertEqual(stats["unavailable"], 0)
        with self.csv_out.open(encoding="utf-8", newline="") as handle:
            out = list(csv.DictReader(handle))
        self.assertEqual(out[0]["short_name"], "民企ETF")
        self.assertEqual(out[0]["short_name_page_no"], "1")
        self.assertIn("场内简称", out[0]["short_name_snippet"])
        self.assertEqual(out[0]["short_name_extractor"], mod.EXTRACTOR_VERSION)
        self.assertEqual(out[0]["doc_sha256"], digest)
        self.assertEqual(out[0]["entity"], "sh.510070")
        self.assertEqual(out[0]["delisting_date"], "2020-11-20")
        for name in mod.ADDED:
            self.assertIn(name, out[0])

    def test_unavailable_name_stays_empty_and_is_counted(self):
        body = b"%PDF-1.4 synthetic\n"
        digest = hashlib.sha256(body).hexdigest()
        (self.pdfs / f"{digest}.pdf").write_bytes(body)
        self.csv_in.write_bytes(csv_bytes([sse_row(sha=digest)]))
        original = mod.page_text
        mod.page_text = lambda pdf, page: "关于XX证券投资基金终止上市的公告\n"
        self.addCleanup(lambda: setattr(mod, "page_text", original))
        stats = mod.process(self.csv_in, self.pdfs, self.csv_out)
        self.assertEqual(stats["named"], 0)
        self.assertEqual(stats["unavailable"], 1)
        with self.csv_out.open(encoding="utf-8", newline="") as handle:
            out = list(csv.DictReader(handle))
        self.assertEqual(out[0]["short_name"], "")
        self.assertEqual(out[0]["short_name_page_no"], "")
        self.assertEqual(out[0]["short_name_snippet"], "")
        self.assertEqual(out[0]["short_name_extractor"], "")

    def test_szse_rows_are_left_alone(self):
        row = sse_row(
            entity="sz.159503",
            source_id="szse_etf_notice_ann",
            sha="d" * 64,
        )
        self.csv_in.write_bytes(csv_bytes([row]))
        stats = mod.process(self.csv_in, self.pdfs, self.csv_out)
        self.assertEqual(stats["sse"], 0)
        self.assertEqual(stats["named"], 0)
        with self.csv_out.open(encoding="utf-8", newline="") as handle:
            out = list(csv.DictReader(handle))
        self.assertEqual(out[0]["short_name"], "")

    def test_missing_pdf_keeps_the_row_and_counts_the_gap(self):
        self.csv_in.write_bytes(csv_bytes([sse_row()]))
        stats = mod.process(self.csv_in, self.pdfs, self.csv_out)
        self.assertEqual(stats["missing_pdf"], 1)
        self.assertEqual(stats["unavailable"], 0)
        with self.csv_out.open(encoding="utf-8", newline="") as handle:
            out = list(csv.DictReader(handle))
        self.assertEqual(len(out), 1)
        self.assertEqual(out[0]["short_name"], "")

    def test_swapped_pdf_sha_never_attaches_another_fund_name(self):
        sha = "e" * 64
        (self.pdfs / f"{sha}.pdf").write_bytes(b"%PDF-1.4 other bytes\n")
        self.csv_in.write_bytes(csv_bytes([sse_row(sha=sha)]))
        original = mod.page_text
        mod.page_text = lambda pdf, page: "场内简称：民企 ETF\n"
        self.addCleanup(lambda: setattr(mod, "page_text", original))
        stats = mod.process(self.csv_in, self.pdfs, self.csv_out)
        self.assertEqual(stats["unavailable"], 1)
        with self.csv_out.open(encoding="utf-8", newline="") as handle:
            out = list(csv.DictReader(handle))
        self.assertEqual(out[0]["short_name"], "")

    def test_missing_required_column_is_an_error(self):
        header = [name for name in mod.D72_REQUIRED if name != "doc_sha256"]
        self.csv_in.write_bytes(csv_bytes([], header=header))
        with self.assertRaises(ValueError):
            mod.process(self.csv_in, self.pdfs, self.csv_out)

    def test_rerunning_overwrites_short_name_columns(self):
        header = list(mod.D72_REQUIRED) + list(mod.ADDED)
        row = sse_row(sha="f" * 64, short_name="STALE", short_name_page_no="9")
        self.csv_in.write_bytes(csv_bytes([row], header=header))
        stats = mod.process(self.csv_in, self.pdfs, self.csv_out)
        self.assertEqual(stats["missing_pdf"], 1)
        with self.csv_out.open(encoding="utf-8", newline="") as handle:
            out = list(csv.DictReader(handle))
        self.assertEqual(out[0]["short_name"], "")
        self.assertEqual(out[0]["short_name_page_no"], "")
        self.assertEqual(list(out[0].keys()), header)


class V2WrongNamePatterns(unittest.TestCase):
    """The nine v1 names that packed another field into the short name.

    Each fixture is the line shape recorded in XDM-201 §3.3 for that entity.
    """

    def assert_name(self, text, expected):
        found = mod.extract_from_text(text)
        self.assertIsNotNone(found, text)
        self.assertEqual(found[0], expected, text)
        return found

    def test_510120_stops_before_yixiajiancheng(self):
        self.assert_name(
            "指数证券投资基金（基金代码：510120，场内简称：非周 ETF，以下简称：本基金）的基金",
            "非周ETF",
        )

    def test_515500_stops_before_yixiajiancheng(self):
        self.assert_name(
            "式指数证券投资基金（基金代码：515500，场内简称：长三角 LX，以下简称：本基金）的",
            "长三角LX",
        )

    def test_516680_stops_before_quoted_yixiajiancheng(self):
        self.assert_name(
            "基金（代码：516680，场内简称：有色金属，以下简称“本基金”）的基金管理",
            "有色金属",
        )

    def test_510450_stops_before_fund_code(self):
        self.assert_name(
            "2、基金简称：180 高 ETF，场内简称：180 高 ETF，基金代码：510450",
            "180高ETF",
        )

    def test_511280_stops_before_expanded_name_parenthesis(self):
        self.assert_name(
            "基金场内简称：中期信用（扩位证券简称：中期信用 ETF）",
            "中期信用",
        )

    def test_516480_stops_before_expanded_name_label(self):
        self.assert_name(
            "式指数证券投资基金（基金代码：516480，场内简称：新材料基，扩位简称：新材料 ETF 基",
            "新材料基",
        )

    def test_512300_stops_at_the_ideographic_comma(self):
        self.assert_name(
            "交易型开放式指数证券投资基金（代码：512300，场内简称：500 医药、医药 ETF 基金，以",
            "500医药",
        )

    def test_515620_drops_the_trailing_full_width_comma(self):
        self.assert_name(
            "建信中证800交易型开放式指数证券投资基金（代码：515620，场内简称：ZZ800，",
            "ZZ800",
        )

    def test_510430_prefers_the_field_line_over_a_wrapped_prose_value(self):
        # The prose value wraps: the raw text has `50 等` then `权，扩位简称：
        # 50 等权 ETF）`, so a first-match capture yields `50等`. The field
        # line further down is the complete value.
        text = (
            "等权重交易型开放式指数证券投资基金（基金代码：510430，场内简称：50 等\n"
            "权，扩位简称：50 等权 ETF）\n"
            "场内简称：50 等权\n"
            "扩位简称：50 等权 ETF\n"
        )
        self.assert_name(text, "50等权")


class V2MissedNamePatterns(unittest.TestCase):
    """The two real names v1 left empty (XDM-201 §3.3).

    v1 refused both because the prose capture ran past 20 characters; the fix
    is the punctuation cut, not a bigger limit.
    """

    def test_510110_name_inside_prose_is_found(self):
        text = (
            "券投资基金（基金代码：510110，场内简称：周期 ETF，以下简称：本基金）的基金管理人\n"
            "基金简称 ：周期 ETF\n"
        )
        found = mod.extract_from_text(text)
        self.assertEqual(found[0], "周期ETF")
        self.assertIn("场内简称：周期 ETF", found[1])

    def test_511230_name_inside_prose_is_found(self):
        text = (
            "资基金（基金代码：511230，场内简称：周期债，以下简称：本基金）的基金管理人海富通\n"
            "基金简称 ：周期债\n"
        )
        found = mod.extract_from_text(text)
        self.assertEqual(found[0], "周期债")


class V2LabelScope(unittest.TestCase):
    """Only the on-exchange 场内简称 label provides a name."""

    def test_fund_inside_label_is_accepted(self):
        # sh.510610 / sh.510620 / sh.512790 write `基金场内简称：`.
        text = "基金场内简称：能源行业\n基金简称：能源行业ETF\n"
        self.assertEqual(mod.extract_from_text(text)[0], "能源行业")

    def test_expanded_labels_are_never_a_name(self):
        for label in (
            "扩位简称",
            "场内扩位简称",
            "基金扩位简称",
            "基金场内扩位简称",
            "证券简称",
            "扩位证券简称",
            "基金简称",
        ):
            with self.subTest(label=label):
                self.assertIsNone(mod.extract_from_text(f"{label}：沪深300低波ETF\n"))

    def test_expanded_labels_in_the_corpus_leave_the_name_unavailable(self):
        # sh.512270 / sh.512860 / sh.516400 style pages: only 扩位 labels.
        self.assertIsNone(
            mod.extract_from_text(
                "基金场内扩位简称：沪深300低波ETF\n扩位简称：沪深300低波ETF\n"
            )
        )

    def test_full_width_characters_are_preserved(self):
        self.assertEqual(mod.extract_from_text("场内简称：沪深３００ETF\n")[0], "沪深３００ETF")

    def test_name_length_bound_comes_from_evidence(self):
        self.assertEqual(mod.MAX_NAME_CHARS, 12)
        self.assertEqual(mod.extract_from_text("场内简称：" + "中" * 12 + "\n")[0], "中" * 12)
        self.assertIsNone(mod.extract_from_text("场内简称：" + "中" * 13 + "\n"))

    def test_prose_label_word_inside_the_value_is_refused(self):
        # A capture the punctuation cut did not terminate must be refused, not
        # stored as a name.
        self.assertIsNone(
            mod.extract_from_text("场内简称：有色金属以下简称本基金\n")
        )


class V2PageBoundary(unittest.TestCase):
    """Never ask `pdftotext` for a page past the last page (XDM-201 §9)."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        body = b"%PDF-1.4 synthetic\n"
        self.sha = hashlib.sha256(body).hexdigest()
        self.pdf = Path(self.tmp.name) / f"{self.sha}.pdf"
        self.pdf.write_bytes(body)

    def test_page_count_parses_pdfinfo(self):
        class Result:
            returncode = 0
            stdout = b"Title: x\nPages:          2\nEncrypted: no\n"

        with unittest.mock.patch.object(mod.subprocess, "run", return_value=Result()):
            self.assertEqual(mod.page_count(self.pdf), 2)

    def test_page_count_is_none_when_pdfinfo_fails(self):
        class Result:
            returncode = 1
            stdout = b""

        with unittest.mock.patch.object(mod.subprocess, "run", return_value=Result()):
            self.assertIsNone(mod.page_count(self.pdf))

    def test_two_page_pdf_is_not_probed_past_its_last_page(self):
        calls = []

        def fake_page_text(pdf, page):
            calls.append(page)
            return "关于XX证券投资基金终止上市的公告\n"

        with unittest.mock.patch.object(mod, "page_count", return_value=2), \
                unittest.mock.patch.object(mod, "page_text", side_effect=fake_page_text):
            found = mod.extract_pdf(self.pdf, self.sha)
        self.assertEqual(found["short_name"], "")
        self.assertEqual(calls, [1, 2])

    def test_probe_without_pdfinfo_stops_at_the_first_failure_after_page_one(self):
        calls = []

        def fake_page_text(pdf, page):
            calls.append(page)
            if page > 2:
                raise RuntimeError(
                    "pdftotext exited 99: first page (3) can not be after the last page (2)"
                )
            return "关于XX证券投资基金终止上市的公告\n"

        with unittest.mock.patch.object(mod, "page_count", return_value=None), \
                unittest.mock.patch.object(mod, "page_text", side_effect=fake_page_text):
            found = mod.extract_pdf(self.pdf, self.sha)
        self.assertEqual(found["short_name"], "")
        self.assertEqual(calls, [1, 2, 3])

    def test_failure_on_page_one_is_still_an_error(self):
        with unittest.mock.patch.object(mod, "page_count", return_value=None), \
                unittest.mock.patch.object(
                    mod, "page_text", side_effect=RuntimeError("pdftotext exited 1")
                ):
            with self.assertRaises(RuntimeError):
                mod.extract_pdf(self.pdf, self.sha)


if __name__ == "__main__":
    unittest.main()
