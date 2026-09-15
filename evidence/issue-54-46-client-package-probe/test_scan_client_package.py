#!/usr/bin/env python3
"""Tests for scan_client_package.py helpers."""
import sys, unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import scan_client_package as s


class TestHelpers(unittest.TestCase):
    def test_sha256(self):
        self.assertEqual(
            s.sha256(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")

    def test_is_text(self):
        self.assertTrue(s.is_text(b"hello world\n" * 10))
        self.assertFalse(s.is_text(bytes(range(32)) * 60))
        self.assertTrue(s.is_text(b""))

    def test_decode_gbk(self):
        self.assertEqual(s.decode("除权".encode("gbk")), "除权")

    def test_kw_hits(self):
        hits = s.kw_hits("除权除息 GBBQ gbbq C1 C1 C1")
        self.assertEqual(hits["C1"], 3)
        self.assertEqual(hits["gbbq"], 1)
        self.assertEqual(hits["GBBQ"], 1)
        self.assertNotIn("C2", hits)

    def test_kw_excerpts_cap(self):
        text = "除权" * 20
        out = s.kw_excerpts(text, ["除权"], ctx=5, cap=3)
        self.assertEqual(len(out), 3)

    def test_incon_sections(self):
        b = "#TDXNHY\nT01|农林牧渔\nT02|x\n#TDXRSHY\nX4202|软件服务\n".encode("gbk")
        secs = s.incon_sections(b)
        self.assertEqual(secs["#TDXNHY"], ["T01", "T02"])
        self.assertEqual(secs["#TDXRSHY"], ["X4202"])

    def test_tdxhy_code_sets(self):
        import io, zipfile
        buf = io.BytesIO()
        with zipfile.ZipFile(buf, "w") as z:
            z.writestr("tdxhy.cfg", "1|600000|T880503|||X4201\n0|000001|T880101|||X4202\n")
        f2, f5 = s.tdxhy_code_sets(buf.getvalue())
        self.assertEqual(f2, {"T880503", "T880101"})
        self.assertEqual(f5, {"X4201", "X4202"})


if __name__ == "__main__":
    unittest.main()
