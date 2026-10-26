#!/usr/bin/env python3
"""CI check: `expected.json` is reproducible from its own inputs.

Mirrors `evidence/issue-54-gbbq-decimal/test_matrix.py`: the fixture that the
Rust test asserts against must not be an unverifiable transcription.
"""

import json
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import reference  # noqa: E402  (path set above)


class QfqExactReferenceTest(unittest.TestCase):
    def test_schema(self):
        document = json.loads(reference.EXPECTED.read_text())
        self.assertEqual(document["schema"], reference.SCHEMA)
        self.assertEqual(document["algorithm"], "qfq-exact-v1")

    def test_expected_values_are_reproducible(self):
        document = json.loads(reference.EXPECTED.read_text())
        self.assertEqual(reference.check(document), [])

    def test_corpus_covers_the_frozen_event_domain(self):
        document = json.loads(reference.EXPECTED.read_text())
        totals = document["corpus"]["totals"]
        self.assertEqual(len(document["corpus"]["securities"]), 3)
        self.assertEqual(totals["events"], 86)
        self.assertEqual(totals["price_fields"], totals["bars"] * 4)

    def test_every_case_states_its_boundary(self):
        document = json.loads(reference.EXPECTED.read_text())
        for case in document["cases"]:
            self.assertTrue(case["note"], case["name"])
            self.assertIn(case["scale"], (2, 6), case["name"])
            self.assertEqual(len(case["expected"]), len(case["bars"]), case["name"])


if __name__ == "__main__":
    unittest.main()
