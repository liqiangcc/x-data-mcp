#!/usr/bin/env python3
"""Self-tests for scan_authority.py — synthetic fixtures only."""
import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import scan_authority as sa


class TestInconParse(unittest.TestCase):
    def test_sections(self):
        text = "#TDXRSHY\nX4202|软件服务\nX420202|行业应用软件\n######\n#TDXNHY\nT1205|软件服务\n######\n"
        sect = sa.parse_incon_sections(text)
        self.assertEqual(sect["TDXRSHY"], [["X4202", "软件服务"],
                                          ["X420202", "行业应用软件"]])
        self.assertEqual(sect["TDXNHY"], [["T1205", "软件服务"]])

    def test_empty_and_header_only(self):
        sect = sa.parse_incon_sections("#A\n######\n#\n######\n")
        self.assertEqual(sect["A"], [])


class TestCoverage(unittest.TestCase):
    def test_basic(self):
        c = sa.coverage({"X1", "X2"}, {"X1", "X3"})
        self.assertEqual(c["covered"], 1)
        self.assertEqual(c["uncovered_field_values"], ["X3"])

    def test_field_tokens(self):
        cfg = b"0|600000|X5001||\r\n1|600004||X99|\r\n"
        # idx=2 tokens
        self.assertEqual(sa.field_tokens(cfg, 2), {"X5001", ""} - {""} or
                         sa.field_tokens(cfg, 2) == {"X5001"})
        self.assertEqual(sa.field_tokens(cfg, 3), {"X99"})


class TestDecode(unittest.TestCase):
    def test_gbk(self):
        raw = "软件服务".encode("gbk")
        self.assertIn("软件服务", sa.decode_best(raw))

    def test_utf8(self):
        raw = "除权".encode("utf-8")
        self.assertEqual(sa.decode_best(raw), "除权")


class TestDocAnchors(unittest.TestCase):
    def test_anchor_map_nonempty(self):
        # every anchor map key must be a pinned doc file
        plan = sa.load_plan()
        for fn in sa.DOC_ANCHORS:
            self.assertIn(fn, plan["inputs"]["tq_docs"])
            for a in sa.DOC_ANCHORS[fn]:
                self.assertTrue(a)


if __name__ == "__main__":
    unittest.main()
