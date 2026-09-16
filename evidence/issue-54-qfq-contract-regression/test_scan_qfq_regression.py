#!/usr/bin/env python3
"""Tests for scan_qfq_regression.py — contract arithmetic + helpers."""
import struct, sys, unittest
from decimal import Decimal
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import scan_qfq_regression as s


class TestContract(unittest.TestCase):
    def test_norm_c_half_up(self):
        # 0x3d4ccccd float32 diagnostic -> business 0.05
        self.assertEqual(s.norm_c("0.0500000007450580596923828125"),
                         Decimal("0.05"))
        self.assertEqual(s.norm_c("2.849999904632568359375"),
                         Decimal("2.85"))
        self.assertEqual(s.norm_c("4.400000095367431640625"),
                         Decimal("4.40"))
        self.assertEqual(s.norm_c("0"), Decimal("0.00"))

    def test_reference_600006_boundary(self):
        # prev 7.25, C1=0.05 -> 7.245 -> cent 7.25
        c = [Decimal("0.05"), Decimal(0), Decimal(0), Decimal(0)]
        ref = s.ref_unrounded(Decimal("7.25"), c)
        self.assertEqual(ref, Decimal("7.245"))
        self.assertEqual(s.q2(ref), Decimal("7.25"))

    def test_reference_600004_boundary(self):
        # prev 7.96, C1=2.85 -> 7.675 -> cent 7.68
        c = [Decimal("2.85"), Decimal(0), Decimal(0), Decimal(0)]
        ref = s.ref_unrounded(Decimal("7.96"), c)
        self.assertEqual(ref, Decimal("7.675"))
        self.assertEqual(s.q2(ref), Decimal("7.68"))

    def test_reference_allotment_000002(self):
        # 000002 2000-01-10: prev 9.65? -> C2=7.5, C4=2.727
        c = [Decimal(0), Decimal("7.50"), Decimal(0), Decimal("2.727")]
        ref = s.ref_unrounded(Decimal("9.65"), c)
        # (9.65 + 2.727*7.5/10) / 1.2727
        self.assertEqual(s.q2(ref), Decimal("9.19"))

    def test_chain_identity_when_no_events(self):
        A, B = s.chain([{"date_int": 20200101,
                         "c": [Decimal(0)] * 4}], 20210101)
        self.assertEqual((A, B), (Decimal(1), Decimal(0)))

    def test_chain_single_event(self):
        # dividend 0.05/share -> T(x) = x - 0.005
        evs = [{"date_int": 20250117,
                "c": [Decimal("0.05"), Decimal(0), Decimal(0), Decimal(0)]}]
        A, B = s.chain(evs, 20250116)
        self.assertEqual(A, Decimal(1))
        self.assertEqual(B, Decimal("-0.005"))
        # event-day bar and later: identity
        A2, B2 = s.chain(evs, 20250117)
        self.assertEqual((A2, B2), (Decimal(1), Decimal(0)))

    def test_chain_compose_order(self):
        # two dividends 1.0 and 2.0 -> (x-0.1-0.2)
        evs = [{"date_int": 1,
                "c": [Decimal(1), Decimal(0), Decimal(0), Decimal(0)]},
               {"date_int": 2,
                "c": [Decimal(2), Decimal(0), Decimal(0), Decimal(0)]}]
        A, B = s.chain(evs, 0)
        self.assertEqual(A, Decimal(1))
        self.assertEqual(B, Decimal("-0.3"))

    def test_day_records(self):
        # build a fake 32B record: date 20250102, O/H/L/C 725/730/720/728
        rec = struct.pack("<IIIIIfII", 20250102, 725, 730, 720, 728,
                          1.5, 100, 0)
        d = s.day_records(rec)
        self.assertEqual(d[20250102]["C"], Decimal("7.28"))
        self.assertEqual(d[20250102]["vol"], 100)


if __name__ == "__main__":
    unittest.main()
