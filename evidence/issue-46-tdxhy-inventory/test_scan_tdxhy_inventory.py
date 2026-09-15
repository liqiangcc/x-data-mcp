"""Regression checks for scanner integrity, not TDX semantic correctness."""
import struct
import tempfile
import unittest
from pathlib import Path

import scan_tdxhy_inventory as scan


class ScanTests(unittest.TestCase):
    def test_plan_hash_pinned(self):
        self.assertEqual(scan.sha((scan.HERE / 'plan.json').read_bytes()), scan.PLAN_SHA256)

    def test_parse_lines_eol_forms(self):
        data = b'a|1\r\nb|2\r\r\nc|3\rd|4\ne|5'
        lines = scan.parse_lines(data)
        self.assertEqual([l['eol'] for l in lines], ['CRLF', 'CRCRLF', 'CR', 'LF', None])
        self.assertEqual([l['content'] for l in lines], [b'a|1', b'b|2', b'c|3', b'd|4', b'e|5'])

    def test_field_inventory_patterns(self):
        lines = scan.parse_lines(b'0|000001|T1001|||X500102\r\n2|920985|T0706|||X300403\r\n')
        nonblank = [l for l in lines if l['content']]
        stats = scan.field_inventory(nonblank)
        self.assertEqual(stats['0']['pattern_conformance']['single_digit'], 2)
        self.assertEqual(stats['1']['pattern_conformance']['six_digit'], 2)
        self.assertEqual(stats['2']['pattern_conformance']['T_code'], 2)
        self.assertEqual(stats['3']['empty_count'], 2)
        self.assertEqual(stats['5']['pattern_conformance']['X_code'], 2)
        self.assertEqual(stats['0']['distinct_count'], 2)

    def test_token_field_attribution(self):
        # '402010' occurs at field 1 (code position) and inside ZX field of another line
        lines = scan.parse_lines(b'0|402010|ZX500102\r\n1|600000|ZX402010\r\n')
        nonblank = [l for l in lines if l['content']]
        tokens = scan.token_scan(nonblank)
        self.assertEqual(sorted({h['field_index'] for h in tokens['402010']}), [1, 2])
        self.assertEqual(sorted({h['field_index'] for h in tokens['600000']}), [1])
        # ZZ 8-digit runs must not yield a six-digit token
        lines2 = scan.parse_lines(b'0|000001|ZZ40101010\r\n')
        self.assertNotIn('401010', scan.token_scan([l for l in lines2 if l['content']]))

    def test_dbf_report_synthetic(self):
        fields = [(b'GPDM', b'C', 6), (b'SSDATE', b'C', 8)]
        hlen = 32 + 32 * len(fields) + 1
        rlen = 1 + sum(f[2] for f in fields)
        recs = [b' 600000' + b'20260101', b' 000001' + b'19901219']
        header = bytearray(hlen)
        header[0] = 3
        struct.pack_into('<I', header, 4, len(recs))
        struct.pack_into('<H', header, 8, hlen)
        struct.pack_into('<H', header, 10, rlen)
        off = 32
        for name, typ, length in fields:
            header[off:off + len(name)] = name
            header[off + 11] = typ[0]
            header[off + 16] = length
            off += 32
        header[hlen - 1] = 0x0D
        data = bytes(header) + b''.join(recs) + b'\x1a'
        report = scan.dbf_report(data)
        self.assertEqual(report['record_count'], 2)
        self.assertEqual(report['gpdm']['six_digit_count'], 2)
        self.assertEqual(report['gpdm']['code_set'], ['000001', '600000'])
        self.assertEqual(report['trailing_bytes_after_records'], 1)

    def test_run_fails_closed_on_wrong_archive(self):
        with tempfile.TemporaryDirectory() as tmp:
            fake = Path(tmp) / 'base.zip'
            fake.write_bytes(b'PK\x05\x06' + b'\0' * 18)
            with self.assertRaises(AssertionError):
                scan.run(fake)


if __name__ == '__main__':
    unittest.main()
