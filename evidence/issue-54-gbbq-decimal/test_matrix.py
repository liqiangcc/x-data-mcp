"""Regression checks for experiment integrity, not official QFQ correctness."""
import copy
import json
import struct
import unittest
from decimal import Decimal
import analyze_matrix as matrix


class MatrixTests(unittest.TestCase):
    def setUp(self):
        self.inputs = json.loads((matrix.HERE / 'inputs.json').read_bytes())

    def test_required_coverage_and_half_cent(self):
        result = matrix.analyze(self.inputs)
        self.assertEqual(result['row_count'], 9)
        reasons = [reason for row in result['rows'] for reason in row['selection_reasons']]
        for reason, count in [('C1_tail', 3), ('C3_tail', 2), ('C4_tail', 2), ('C2_C4_combination', 2)]:
            self.assertEqual(reasons.count(reason), count)
        event = next(r for r in result['rows'] if r['symbol'] == 'sh.600006' and r['date'] == '2025-07-17')
        self.assertEqual(event['previous_close'], '7.25')
        for name in ('frozen_repr', 'exact_binary'):
            self.assertEqual(event['candidates'][name]['reference_cent'], '7.24')
        for name in ('shortest_roundtrip', 'fixed_2dp', 'fixed_3dp', 'fixed_4dp', 'fixed_6dp'):
            self.assertEqual(event['candidates'][name]['reference_cent'], '7.25')
        rights = next(r for r in result['rows'] if r['date'] == '2000-01-10')
        self.assertEqual(rights['candidates']['shortest_roundtrip']['C1_C2_C3_C4'], ['0', '7.5', '0', '2.727'])
        self.assertFalse(rights['candidates']['fixed_2dp']['roundtrip_to_original_bits'][3])

    def test_same_float_does_not_identify_decimal(self):
        self.assertNotEqual(Decimal('0.05'), Decimal('0.050000001'))
        self.assertEqual(struct.pack('<f', float('0.05')), struct.pack('<f', float('0.050000001')))

    def test_tampered_event_or_provenance_rejected(self):
        for field, value in [('selected_events_sha256', 'bad'), ('plan_sha256', 'bad')]:
            bad = copy.deepcopy(self.inputs)
            bad[field] = value
            with self.assertRaises(AssertionError):
                matrix.analyze(bad)
        bad = copy.deepcopy(self.inputs)
        bad['rows'][0]['event']['float32_raw_bits_hex'][0] = '0x00000000'
        with self.assertRaises(AssertionError):
            matrix.analyze(bad)

    def test_previous_bar_must_precede_event(self):
        bad = copy.deepcopy(self.inputs)
        row = bad['rows'][0]
        raw = bytearray.fromhex(row['prior_bar']['raw_hex'])
        struct.pack_into('<I', raw, 0, row['event']['date_raw_int'])
        row['prior_bar']['raw_hex'] = raw.hex()
        with self.assertRaises(AssertionError):
            matrix.analyze(bad)

    def test_missing_close_is_retained(self):
        data = copy.deepcopy(self.inputs)
        data['rows'][0]['prior_bar'] = None
        row = matrix.analyze(data)['rows'][0]
        self.assertEqual(row['calculation_status'], 'PREVIOUS_CLOSE_UNRESOLVED')
        self.assertNotIn('reference_cent', row['candidates']['shortest_roundtrip'])


if __name__ == '__main__':
    unittest.main()
