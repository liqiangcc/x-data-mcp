"""Unit tests for the offline SSE ETF short-name extractor (PR-4).

All fixtures are synthetic text or synthetic files in a tempdir: no PDF bytes
and no network are needed. The extraction rule must never substitute a fund's
full name from an announcement title, and an unavailable name must stay empty.
"""

import csv
import hashlib
import importlib.util
import tempfile
import unittest
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


if __name__ == "__main__":
    unittest.main()
