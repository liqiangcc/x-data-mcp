#!/usr/bin/env python3
"""Unit tests for eval_read_tools.py check functions."""
from __future__ import annotations

import unittest
from decimal import Decimal

# Import functions from eval_read_tools
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

from eval_read_tools import (
    check_tradability,
    check_limit_price,
    check_market_cap,
    check_unknown_status,
)


class TestCheckTradability(unittest.TestCase):
    def test_tradable_with_bar_no_suspension(self) -> None:
        answer = "2020-01-07 有成交记录，未停牌。\n结论：能买"
        oracle_rows = {
            "tradability": [{"date": "2020-01-07", "close": 1200.0}],
            "suspension": [],
        }
        check = {"oracles": ["tradability", "suspension"]}
        result = check_tradability(answer, oracle_rows, check)
        self.assertTrue(result["ok"])
        self.assertEqual(result["detail"]["expected"], "能买")
        self.assertEqual(result["detail"]["got"], "能买")

    def test_not_tradable_no_bar(self) -> None:
        answer = "无交易数据。\n结论：不能买"
        oracle_rows = {
            "tradability": [],
            "suspension": [],
        }
        check = {"oracles": ["tradability", "suspension"]}
        result = check_tradability(answer, oracle_rows, check)
        self.assertTrue(result["ok"])
        self.assertEqual(result["detail"]["expected"], "不能买")

    def test_not_tradable_suspended(self) -> None:
        answer = "停牌。\n结论：不能买"
        oracle_rows = {
            "tradability": [{"date": "2020-01-07", "close": 1200.0}],
            "suspension": [{"start_day": "2020-01-06", "end_day": "2020-01-10"}],
        }
        check = {"oracles": ["tradability", "suspension"]}
        result = check_tradability(answer, oracle_rows, check)
        self.assertTrue(result["ok"])
        self.assertEqual(result["detail"]["expected"], "不能买")

    def test_wrong_conclusion(self) -> None:
        answer = "有交易数据，未停牌。\n结论：不能买"
        oracle_rows = {
            "tradability": [{"date": "2020-01-07", "close": 1200.0}],
            "suspension": [],
        }
        check = {"oracles": ["tradability", "suspension"]}
        result = check_tradability(answer, oracle_rows, check)
        self.assertFalse(result["ok"])


class TestCheckLimitPrice(unittest.TestCase):
    def test_gem_20_percent_band(self) -> None:
        # sz.300750 on 2020-08-25, prev close 50.50, 20% band
        # limit_up = round_half_up(50.50 * 1.2) = 60.60
        answer = "前一交易日收盘价 50.50 元，涨幅 20%。\n涨停价：60.60 元"
        oracle_rows = {
            "prev_bar": [{"date": "2020-08-24", "close": 50.50}],
            "curr_bar": [{"date": "2020-08-25"}],
        }
        check = {"oracles": ["prev_bar", "curr_bar"], "band_bp": 2000, "min_tick_cents": 1}
        result = check_limit_price(answer, oracle_rows, check)
        self.assertTrue(result["ok"])
        self.assertAlmostEqual(result["detail"]["expected"], 60.60, places=2)

    def test_main_board_10_percent(self) -> None:
        # 10% band, prev close 10.00
        # limit_up = round_half_up(10.00 * 1.1) = 11.00
        answer = "涨停价：11.00 元"
        oracle_rows = {
            "prev_bar": [{"date": "2020-01-06", "close": 10.00}],
            "curr_bar": [{"date": "2020-01-07"}],
        }
        check = {"oracles": ["prev_bar", "curr_bar"], "band_bp": 1000, "min_tick_cents": 1}
        result = check_limit_price(answer, oracle_rows, check)
        self.assertTrue(result["ok"])
        self.assertAlmostEqual(result["detail"]["expected"], 11.00, places=2)

    def test_rounding_half_up(self) -> None:
        # prev close 12.34, 10% band
        # limit_up = round_half_up(12.34 * 1.1) = round_half_up(13.574) = 13.57
        answer = "涨停价：13.57 元"
        oracle_rows = {
            "prev_bar": [{"date": "2020-01-06", "close": 12.34}],
            "curr_bar": [{"date": "2020-01-07"}],
        }
        check = {"oracles": ["prev_bar", "curr_bar"], "band_bp": 1000, "min_tick_cents": 1}
        result = check_limit_price(answer, oracle_rows, check)
        self.assertTrue(result["ok"])
        self.assertAlmostEqual(result["detail"]["expected"], 13.57, places=2)

    def test_no_prev_close(self) -> None:
        answer = "无法计算"
        oracle_rows = {
            "prev_bar": [],
            "curr_bar": [{"date": "2020-01-07"}],
        }
        check = {"oracles": ["prev_bar", "curr_bar"], "band_bp": 1000, "min_tick_cents": 1}
        result = check_limit_price(answer, oracle_rows, check)
        self.assertFalse(result["ok"])
        self.assertEqual(result["detail"]["error"], "no_prev_close")


class TestCheckMarketCap(unittest.TestCase):
    def test_float_a_market_cap(self) -> None:
        # close 10.00, float_a 1000万股 = 10000000
        # market_cap = 10 * 10000000 = 100000000 yuan = 1亿
        answer = "流通市值：1 亿元"
        oracle_rows = {
            "bar": [{"date": "2020-01-07", "close": 10.00}],
            "capital": [{"effective_day": "2019-12-01", "float_after": 10000000}],
        }
        check = {"oracles": ["bar", "capital"], "scope": "float_a"}
        result = check_market_cap(answer, oracle_rows, check)
        self.assertTrue(result["ok"])
        self.assertAlmostEqual(result["detail"]["expected_yuan"], 100000000, places=0)
        self.assertAlmostEqual(result["detail"]["expected_yi"], 1.0, places=2)

    def test_market_cap_in_yuan(self) -> None:
        # Answer in yuan instead of yi
        answer = "流通市值：100000000 元"
        oracle_rows = {
            "bar": [{"date": "2020-01-07", "close": 10.00}],
            "capital": [{"effective_day": "2019-12-01", "float_after": 10000000}],
        }
        check = {"oracles": ["bar", "capital"], "scope": "float_a"}
        result = check_market_cap(answer, oracle_rows, check)
        self.assertTrue(result["ok"])

    def test_no_shares_data(self) -> None:
        answer = "无股本数据"
        oracle_rows = {
            "bar": [{"date": "2020-01-07", "close": 10.00}],
            "capital": [],
        }
        check = {"oracles": ["bar", "capital"], "scope": "float_a"}
        result = check_market_cap(answer, oracle_rows, check)
        self.assertFalse(result["ok"])
        self.assertEqual(result["detail"]["error"], "no_shares_data")


class TestCheckUnknownStatus(unittest.TestCase):
    def test_admits_uncertainty_when_no_status(self) -> None:
        answer = "数据库中无 ST 状态记录。\n结论：无法确定"
        oracle_rows = []
        check = {"oracle": "risk_status"}
        result = check_unknown_status(answer, oracle_rows, check)
        self.assertTrue(result["ok"])
        self.assertEqual(result["detail"]["expected"], "无法确定")
        self.assertEqual(result["detail"]["got"], "无法确定")

    def test_certain_when_has_status(self) -> None:
        answer = "状态为 normal。\n结论：已确定"
        oracle_rows = [{"normalized_status": "normal"}]
        check = {"oracle": "risk_status"}
        result = check_unknown_status(answer, oracle_rows, check)
        self.assertTrue(result["ok"])
        self.assertEqual(result["detail"]["expected"], "已确定")

    def test_wrong_when_guesses_with_no_data(self) -> None:
        answer = "应该是正常状态。\n结论：已确定"
        oracle_rows = []
        check = {"oracle": "risk_status"}
        result = check_unknown_status(answer, oracle_rows, check)
        self.assertFalse(result["ok"])


if __name__ == "__main__":
    unittest.main()
