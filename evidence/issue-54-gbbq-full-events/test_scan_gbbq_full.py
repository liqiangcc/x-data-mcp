"""Regression checks for scanner integrity, not official QFQ correctness."""
import struct
import tempfile
import unittest
from decimal import Decimal as D
from pathlib import Path

import scan_gbbq_full as scan


def make_clear(market, code, date_raw, category, floats):
    clear = bytearray(29)
    clear[0] = market
    clear[1:7] = code.encode()
    struct.pack_into('<I', clear, 8, date_raw)
    clear[12] = category
    for i, value in enumerate(floats):
        struct.pack_into('<f', clear, 13 + 4 * i, value)
    return bytes(clear)


def make_record(market, code, date_raw, category, floats, index=0):
    decoder = scan.load_decoder_module()
    return decoder.parse_clear_record(make_clear(market, code, date_raw, category, floats), index)


def make_day_row(date_raw, close_int):
    record = struct.pack('<IIIII', date_raw, close_int, close_int, close_int, close_int) + b'\0' * 12
    return {
        'index': 0,
        'date_raw_int': date_raw,
        'date_iso': f'{date_raw // 10000:04d}-{date_raw // 100 % 100:02d}-{date_raw % 100:02d}',
        'raw_hex': record.hex(),
        'open': D(close_int) / 100,
        'close': D(close_int) / 100,
    }


class ScanTests(unittest.TestCase):
    def test_plan_hash_pinned(self):
        self.assertEqual(scan.sha((scan.HERE / 'plan.json').read_bytes()), scan.PLAN_SHA256)

    def test_half_cent_candidates_match_bounded_matrix(self):
        event = make_record(1, '600006', 20250717, 1, [0.05, 0.0, 0.0, 0.0], index=127405)
        self.assertEqual(event['float32_raw_bits_hex'][0], '0x3d4ccccd')
        day = [make_day_row(20250716, 725), make_day_row(20250717, 728)]
        (row,) = scan.event_matrix([event], day)
        self.assertEqual(row['previous_close'], '7.25')
        for name in ('frozen_repr', 'exact_binary'):
            self.assertEqual(row['candidates'][name]['reference_cent'], '7.24')
            self.assertFalse(row['candidates'][name]['reference_cent_equals_previous_close'])
        for name in ('shortest_roundtrip', 'fixed_2dp', 'fixed_3dp', 'fixed_4dp', 'fixed_6dp'):
            self.assertEqual(row['candidates'][name]['reference_cent'], '7.25')
            self.assertTrue(row['candidates'][name]['reference_cent_equals_previous_close'])
        summary = scan.matrix_summary([row])
        self.assertEqual(summary['candidate_disagreement_count'], 1)
        self.assertEqual(summary['per_candidate']['exact_binary']['reference_cent_differs_count'], 1)
        self.assertTrue(row['event_date_in_day'])

    def test_unresolved_previous_close_retained(self):
        event = make_record(0, '000002', 19910601, 1, [0.0, 4.4, 0.0, 5.0], index=81)
        day = [make_day_row(19910603, 469)]
        (row,) = scan.event_matrix([event], day)
        self.assertEqual(row['calculation_status'], 'PREVIOUS_CLOSE_UNRESOLVED')
        self.assertNotIn('reference_cent', row['candidates']['shortest_roundtrip'])

    def test_histograms_on_synthetic_records(self):
        records = [
            make_record(0, '000002', 19930405, 1, [0.6, 0.0, 5.0, 0.0], index=86),
            make_record(0, '000002', 19960806, 1, [1.4, 0.0, 1.0, 0.0], index=92),
            make_record(1, '600006', 20250717, 1, [0.05, 0.0, 0.0, 0.0], index=127405),
            make_record(1, '600004', 20051220, 2, [0.0, 0.0, 1.9, 0.0], index=127306),
        ]
        cat1 = [r for r in records if r['category'] == 1]
        stats = scan.field_stats(cat1)
        self.assertEqual(stats[0]['field'], 'C1')
        self.assertEqual(stats[0]['nonzero_count'], 3)
        self.assertEqual(stats[0]['zero_count'], 0)
        self.assertEqual(stats[0]['distinct_raw_bits_count'], 3)
        self.assertEqual(stats[0]['tail_bits_count'], 3)
        self.assertEqual(stats[2]['distinct_raw_bits_count'], 3)
        self.assertEqual(stats[2]['tail_bits_count'], 0)
        c1_values = {item['bits']: item for item in stats[0]['distinct_values']}
        self.assertNotIn('0x00000000', c1_values)
        self.assertEqual(c1_values['0x3d4ccccd']['shortest_roundtrip_decimal'], '0.05')
        patterns = {p['mask']: p['count'] for p in scan.combination_patterns(cat1)}
        self.assertEqual(patterns[0b0101], 2)
        self.assertEqual(patterns[0b0001], 1)
        coverage = scan.symbol_coverage(cat1)
        self.assertEqual(coverage['0.000002']['count'], 2)
        self.assertEqual(coverage['0.000002']['first_date'], '1993-04-05')
        self.assertEqual(coverage['1.600006']['count'], 1)
        self.assertNotIn('1.600004', coverage)

    def test_run_fails_closed_on_unpinned_inputs(self):
        with tempfile.TemporaryDirectory() as tmp:
            fake = Path(tmp)
            gbbq = fake / 'gbbq'
            gbbq.write_bytes(b'\x9f\x23\x03\x00' + b'\0' * 29)
            source = fake / 'gbbq_reader.py'
            source.write_text('hexdump_keys = "00"\n')
            day_zip = fake / 'hsjday.zip'
            day_zip.write_bytes(b'PK\x05\x06' + b'\0' * 18)
            with self.assertRaises(AssertionError):
                scan.run(gbbq, source, day_zip)


if __name__ == '__main__':
    unittest.main()
