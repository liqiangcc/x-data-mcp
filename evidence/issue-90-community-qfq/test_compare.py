import json
import math
from pathlib import Path
import tempfile
import unittest
import compare


class ComparisonTests(unittest.TestCase):
    def test_existing_nine_rows_and_frozen_output(self):
        result = compare.analyze()
        self.assertEqual(result['row_count'], 9)
        self.assertEqual(compare.serialized(), (compare.HERE / 'result.json').read_bytes())
        old = json.loads((compare.ROOT / 'evidence/issue-54-gbbq-decimal/inputs.json').read_bytes())
        self.assertEqual([(r['symbol'], r['date']) for r in result['rows']],
                         [(r['symbol'], r['event']['date_iso']) for r in old['rows']])
        self.assertEqual(result['official_semantics'], 'NOT_VERIFIED')
        self.assertEqual(result['upstream_runtime_tests'], 'NOT_RUN_SCALAR_ADAPTATION_ONLY')
        self.assertFalse(result['production_algorithm_changed'])

    def test_go_round_ties_and_adjacent_binary64(self):
        self.assertEqual(compare.round_away(0.5), 1)
        self.assertEqual(compare.round_away(-0.5), -1)
        self.assertEqual(compare.round_away(math.nextafter(0.5, 0)), 0)
        self.assertEqual(compare.round_away(math.nextafter(-0.5, 0)), 0)

    def test_models_are_distinct_even_without_float32_tail(self):
        fields = [1.0, 0.0, 0.0, 0.0]
        prices = [20.0, 20.0, 20.0, 10.0]
        _, affine = compare.injoyai(fields, prices)
        reference, proportional = compare.quantaxis(fields, prices)
        self.assertEqual(reference, 9.9)
        self.assertEqual(affine[0], 19.9)
        self.assertEqual(proportional[0], 19.8)

    def test_half_cent_and_rights_information_loss(self):
        rows = compare.analyze()['rows']
        half = next(r for r in rows if r['date'] == '2025-07-17')
        self.assertEqual(half['injoyai_affine_ohlc'][3], '7.25')
        self.assertEqual(half['frozen_repr']['reference_cent'], '7.24')
        rights = next(r for r in rows if r['date'] == '2000-01-10')
        self.assertFalse(rights['normalized_bits_preserved'][3])
        self.assertEqual(float(rights['injoyai_fields_2dp'][3]), 2.73)

    def test_retained_digest_tampering_rejected(self):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / 'source'
            p.write_bytes(b'original')
            digest = compare.sha(p.read_bytes())
            p.write_bytes(b'changed')
            with self.assertRaises(ValueError):
                compare.verified_bytes(p, digest)


if __name__ == '__main__':
    unittest.main()
