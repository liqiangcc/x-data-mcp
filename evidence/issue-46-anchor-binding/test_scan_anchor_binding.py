#!/usr/bin/env python3
"""Self-tests for scan_anchor_binding.py — synthetic fixtures only."""
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import scan_anchor_binding as sa


class TestPdfParse(unittest.TestCase):
    def setUp(self):
        self.lines = [
            "T01 能源",
            "T0101 煤炭 880301",
            "T010101 煤炭开采 880302 主营为煤炭开采、加工等",
            "T0102 电力 880305",
            "T010201 水力发电 880306 主营为水力发电等",
            "T0705",
            "工业机械",
            "880440",
            "T02 材料 T0201 钢铁 880318",
        ]

    def test_rows(self):
        p = sa.parse_pdf_table(self.lines, 0, len(self.lines), "T")
        self.assertEqual(p["T01"]["name"], "能源")
        self.assertEqual(p["T01"]["level"], 1)
        self.assertEqual(p["T0101"]["idx"], ["880301"])
        self.assertEqual(p["T010101"]["name"], "煤炭开采")
        self.assertEqual(p["T010101"]["level"], 3)
        # multi-code line
        self.assertEqual(p["T02"]["name"], "材料")
        self.assertEqual(p["T0201"]["name"], "钢铁")
        # next-line name fallback
        self.assertEqual(p["T0705"]["name"], "工业机械")
        self.assertEqual(p["T0705"]["idx"], ["880440"])


class TestNameStatus(unittest.TestCase):
    def test_match(self):
        self.assertEqual(sa.name_status("软件服务", "软件服务"), "MATCH")

    def test_variant_substring(self):
        self.assertEqual(sa.name_status("有色", "有色金属"),
                         "NAME_WORDING_VARIANT")
        self.assertEqual(sa.name_status("IT", "IT设备"),
                         "NAME_WORDING_VARIANT")

    def test_variant_ratio(self):
        self.assertEqual(sa.name_status("红黄药酒", "红黄酒"),
                         "NAME_WORDING_VARIANT")

    def test_conflict(self):
        self.assertEqual(sa.name_status("银行", "煤炭"),
                         "NAME_CONFLICT")

    def test_none(self):
        self.assertIsNone(sa.name_status(None, "x"))
        self.assertIsNone(sa.name_status("x", None))


class TestInconParse(unittest.TestCase):
    def test_sections(self):
        t = "#TDXNHY\nT01|能源\n######\n#TDXRSHY\nX4202|软件服务\n######\n"
        s = sa.parse_incon(t)
        self.assertEqual(s["TDXNHY"]["T01"], "能源")
        self.assertEqual(s["TDXRSHY"]["X4202"], "软件服务")


class TestFieldTokens(unittest.TestCase):
    def test(self):
        cfg = b"0|600000|X5001||\r\n1|600004||X99|\r\n"
        self.assertEqual(sa.field_tokens(cfg, 2), {"X5001"})
        self.assertEqual(sa.field_tokens(cfg, 3), {"X99"})


if __name__ == "__main__":
    unittest.main()
