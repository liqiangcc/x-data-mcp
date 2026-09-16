# -*- coding: utf-8 -*-
import json
import unittest
from pathlib import Path

import scan_ashare_contract as s


class TestHelpers(unittest.TestCase):
    def test_cfg_rows(self):
        rows = s.cfg_rows(b'1|600000|T1001|||X500102\r\n0|000001|T1001|||X500102\r\n')
        self.assertEqual(rows[0], ['1', '600000', 'T1001', '', '', 'X500102'])
        self.assertEqual(len(rows), 2)

    def test_parse_incon_sections(self):
        sec = s.parse_incon('#TDXRSHY\nX42|计算机\nX4202|软件服务\n######\nX99|其他\n')
        self.assertEqual(sec['TDXRSHY'], {'X42': '计算机', 'X4202': '软件服务'})
        self.assertNotIn('X99', sec['TDXRSHY'])

    def test_level_of(self):
        self.assertEqual(s.level_of('X42'), 1)
        self.assertEqual(s.level_of('X4202'), 2)
        self.assertEqual(s.level_of('X420202'), 3)

    def test_dbf_records_minimal(self):
        # header: 1 record, hlen=32+32+1=65, rlen=1+6=7; fields GPDM C(6)
        import struct
        header = bytearray(32)
        header[0] = 0x03
        header[4:8] = (1).to_bytes(4, 'little')
        header[8:10] = (65).to_bytes(2, 'little')
        header[10:12] = (7).to_bytes(2, 'little')
        desc = b'GPDM' + b'\x00' * 7 + b'C' + b'\x00' * 4 + bytes([6]) + b'\x00' * 15
        rec = b' ' + b'000001'
        fields, recs = s.dbf_records(bytes(header) + desc + b'\x0d' + rec + b'\x1a')
        self.assertEqual(fields[0][0], 'GPDM')
        self.assertEqual(recs[0]['GPDM'], b'000001')


class TestResult(unittest.TestCase):
    def test_result_all_ok(self):
        p = Path(__file__).parent / 'result.json'
        s.check_mode(str(p))

    def test_result_shape(self):
        d = json.load(open(Path(__file__).parent / 'result.json', encoding='utf-8'))
        self.assertIn('ac_verdicts', d)
        self.assertEqual(sorted(d['ac_verdicts']),
                         [f'AC-0{i}' for i in range(1, 8)])
        self.assertIsInstance(d['scope']['S'], int)
        self.assertGreater(d['scope']['S'], 0)


if __name__ == '__main__':
    unittest.main()
