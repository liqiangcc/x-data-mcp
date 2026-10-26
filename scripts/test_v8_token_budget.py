"""Unit tests for scripts/v8_token_budget.py (#387 §8.4)."""
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("v8_token_budget", SCRIPTS / "v8_token_budget.py")
budget = importlib.util.module_from_spec(spec)
spec.loader.exec_module(budget)


class TokenBudget(unittest.TestCase):
    def test_committed_fixtures_pass_their_budgets(self):
        failures, measured = budget.check()
        self.assertEqual(failures, [])
        self.assertIn("T1-panel-242", measured)
        # The design's T1 budget: one call, at most 5.0K model-visible tokens.
        self.assertLessEqual(measured["T1-panel-242"], 5000)

    def test_every_fixture_has_a_budget(self):
        baseline = json.loads(budget.BASELINE.read_text(encoding="utf-8"))
        listed = {task["fixture"] for task in baseline["tasks"]}
        self.assertEqual(budget.fixture_files() - listed, set())

    def test_tokenizer_counts_non_ascii_at_one_point_five(self):
        self.assertAlmostEqual(budget.token_count("abcd", 4, 1.5), 1.0)
        self.assertAlmostEqual(budget.token_count("中", 4, 1.5), 1 / 1.5)
        self.assertAlmostEqual(budget.token_count("中abc", 4, 1.5), 1 / 1.5 + 0.75)

    def test_canonical_is_compact_and_sorted(self):
        self.assertEqual(budget.canonical({"b": 1, "a": [2]}), '{"a":[2],"b":1}')
        self.assertEqual(budget.canonical({"中": "文"}), '{"中":"文"}')

    def test_over_budget_is_reported(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "fixtures/v8").mkdir(parents=True)
            (root / "fixtures/v8/one.json").write_text(
                json.dumps({"expected": {"a": "x" * 4000}}), encoding="utf-8"
            )
            baseline = root / "baseline.json"
            baseline.write_text(
                json.dumps(
                    {
                        "tokenizer": {
                            "ascii_chars_per_token": 4,
                            "non_ascii_chars_per_token": 1.5,
                        },
                        "tasks": [
                            {
                                "id": "tiny",
                                "fixture": "fixtures/v8/one.json",
                                "budget_tokens": 10,
                            }
                        ],
                    }
                ),
                encoding="utf-8",
            )
            failures, _ = budget.check(baseline, root)
            self.assertTrue(any("> budget" in failure for failure in failures), failures)

    def test_missing_fixture_and_unbudgeted_fixture_are_reported(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "fixtures/v8").mkdir(parents=True)
            (root / "fixtures/v8/extra.json").write_text(
                json.dumps({"expected": {}}), encoding="utf-8"
            )
            baseline = root / "baseline.json"
            baseline.write_text(
                json.dumps(
                    {
                        "tokenizer": {
                            "ascii_chars_per_token": 4,
                            "non_ascii_chars_per_token": 1.5,
                        },
                        "tasks": [
                            {
                                "id": "gone",
                                "fixture": "fixtures/v8/gone.json",
                                "budget_tokens": 10,
                            }
                        ],
                    }
                ),
                encoding="utf-8",
            )
            failures, _ = budget.check(baseline, root)
            self.assertTrue(any("missing fixture" in failure for failure in failures), failures)
            self.assertTrue(any("no token budget" in failure for failure in failures), failures)


if __name__ == "__main__":
    unittest.main()
