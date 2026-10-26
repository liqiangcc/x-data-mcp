#!/usr/bin/env python3
"""Tests for scripts/analysis/boll.py against a synthetic SQLite fixture.

The fixture mirrors the two tables the analysis reads
(`migrations/tasks/021_latest_only.sql`: `market_series` and the WITHOUT ROWID
`series_bars` with primary key `(series_key, trade_day)`), so the SQL under test
runs with the same access paths as on a real database.

The expected values come from `reference_trades` / `reference_index` /
`reference_excursions` below: a deliberately naive port of the reference scripts
captured in the #373 comment (full history in memory, Python SMA, index
arithmetic).  Comparing the optimised SQL against that port is what proves the
pushdown did not change the signal definition or the return arithmetic.
"""
from __future__ import annotations

import datetime
import random
import sqlite3
import tempfile
import unittest
from pathlib import Path

from analysis import boll

# Same shape as the deployed schema, restricted to the columns the analysis
# reads (migrations/tasks/021_latest_only.sql).
SCHEMA = """
CREATE TABLE market_series(
  series_key INTEGER PRIMARY KEY,
  entity TEXT NOT NULL,
  period INTEGER NOT NULL,
  adjustment INTEGER NOT NULL,
  record_count INTEGER NOT NULL,
  last_day INTEGER NOT NULL
) STRICT;
CREATE TABLE series_bars(
  series_key INTEGER NOT NULL,
  trade_day INTEGER NOT NULL,
  open_cents INTEGER NOT NULL,
  high_cents INTEGER NOT NULL,
  low_cents INTEGER NOT NULL,
  close_cents INTEGER NOT NULL,
  PRIMARY KEY(series_key, trade_day)
) STRICT, WITHOUT ROWID;
"""

# Schema 22 (issue-383 §3.3) renamed the price columns to ``*_units``: the
# analysis resolves the names per connection, so both shapes must read the same.
SCHEMA_22 = SCHEMA.replace("_cents", "_units")


def write_fixture(path: Path, daily: dict, highs: dict, schema: str = SCHEMA) -> None:
    """Create the fixture database.

    `daily` maps entity -> [(trade_day, open, high, low, close), ...] and
    `highs` maps entity -> 2025 high; every entity in `highs` also gets the
    yearly bar the analysis reads, which carries the year's last trading day.
    """
    conn = sqlite3.connect(path)
    try:
        conn.executescript(schema)
        key = 1
        for entity, bars in daily.items():
            conn.execute(
                "INSERT INTO market_series VALUES(?,?,1,1,?,?)",
                (key, entity, len(bars), bars[-1][0]),
            )
            conn.executemany(
                "INSERT INTO series_bars VALUES(?,?,?,?,?,?)",
                [(key,) + tuple(bar) for bar in bars],
            )
            key += 1
        for entity, high in highs.items():
            conn.execute(
                "INSERT INTO market_series VALUES(?,?,2,1,1,?)",
                (key, entity, boll.YEAR_2025_END),
            )
            conn.execute(
                "INSERT INTO series_bars VALUES(?,?,?,?,?,?)",
                (key, boll.YEAR_2025_END, high, high, high, high),
            )
            key += 1
        conn.commit()
    finally:
        conn.close()


def sma(values: list[int], period: int) -> list[float | None]:
    """Rolling mean, as in the reference script."""
    out: list[float | None] = [None] * len(values)
    total = 0
    for index, value in enumerate(values):
        total += value
        if index >= period:
            total -= values[index - period]
        if index >= period - 1:
            out[index] = total / period
    return out


def reference_trades(daily: dict, highs: dict) -> dict:
    """Naive port of the reference script's signal loop, keyed by entity."""
    trades = {}
    for entity, series in daily.items():
        if entity not in highs:
            continue
        bars = [bar for bar in series if bar[0] <= boll.LAST_DAY]
        if len(bars) < boll.MIN_BARS:
            continue
        closes = [bar[4] for bar in bars]
        mids = sma(closes, boll.SMA_PERIOD)
        breakout = None
        for index, bar in enumerate(bars):
            if bar[0] >= boll.BREAKOUT_START_DAY and bar[2] > highs[entity]:
                breakout = index
                break
        if breakout is None:
            continue
        pulled = False
        signal = None
        for index in range(breakout + 1, len(bars)):
            if mids[index] is None:
                continue
            if not pulled:
                if closes[index] < mids[index]:
                    pulled = True
            elif closes[index] > mids[index] and closes[index - 1] <= mids[index - 1]:
                signal = index
                break
        if signal is None or signal + 1 >= len(bars):
            continue
        entry_index = signal + 1
        entry = bars[entry_index][1]
        if entry <= 0:
            continue
        trade = {
            "bo_day": bars[breakout][0],
            "sig_day": bars[signal][0],
            "entry_day": bars[entry_index][0],
            "entry": entry,
            "ex_end": bars[-1][0],
            "rend": bars[-1][4] / entry - 1,
            "mae20": min(bar[4] for bar in bars[entry_index : entry_index + 21])
            / entry
            - 1,
            "mfe": max(bar[2] for bar in bars[entry_index : entry_index + 21]) / entry
            - 1,
            "mae": min(bar[3] for bar in bars[entry_index : entry_index + 21]) / entry
            - 1,
        }
        for hold in boll.HORIZONS:
            if entry_index + hold < len(bars):
                trade[f"r{hold}"] = bars[entry_index + hold][4] / entry - 1
                trade[f"ex{hold}"] = bars[entry_index + hold][0]
        if breakout + 1 < len(bars) and bars[breakout + 1][1] > 0:
            bo_entry = bars[breakout + 1][1]
            trade["bo_entry"] = bo_entry
            for hold in boll.HORIZONS:
                if entry_index + hold < len(bars):
                    trade[f"b{hold}"] = bars[entry_index + hold][4] / bo_entry - 1
            trade["bend"] = bars[-1][4] / bo_entry - 1
        trades[entity] = trade
    return trades


def reference_index(daily: dict, start_day: int) -> dict[int, float]:
    """Naive port of the reference bench index over [start_day, LAST_DAY]."""
    days = sorted(
        {
            bar[0]
            for series in daily.values()
            for bar in series
            if start_day <= bar[0] <= boll.LAST_DAY
        }
    )
    closes = {day: {} for day in days}
    for entity, series in daily.items():
        for bar in series:
            if start_day <= bar[0] <= boll.LAST_DAY:
                closes[bar[0]][entity] = bar[4]
    index = {days[0]: 1.0}
    for position in range(1, len(days)):
        previous, day = days[position - 1], days[position]
        returns = [
            closes[day][entity] / closes[previous][entity] - 1
            for entity in closes[day]
            if entity in closes[previous] and closes[previous][entity] > 0
        ]
        index[day] = index[previous] * (1 + sum(returns) / len(returns))
    return index


def random_series(rng: random.Random, start_day: int, count: int) -> list[tuple]:
    """A deterministic random walk of daily bars, trading days are consecutive."""
    bars = []
    price = 1000
    for offset in range(count):
        price = max(1, price + rng.choice([-3, -2, -1, 0, 1, 2, 3]) * rng.randint(5, 60))
        open_cents = max(1, price + rng.randint(-40, 40))
        high = max(price, open_cents) + rng.randint(0, 50)
        low = max(1, min(price, open_cents) - rng.randint(0, 50))
        bars.append((start_day + offset, open_cents, high, low, price))
    return bars


class FixtureCase(unittest.TestCase):
    """Builds a fixture database per test and removes it afterwards."""

    def setUp(self) -> None:
        self._dir = tempfile.TemporaryDirectory()
        self.path = Path(self._dir.name) / "fixture.sqlite"
        self.addCleanup(self._dir.cleanup)

    def analyse(self, daily: dict, highs: dict) -> boll.Dataset:
        write_fixture(self.path, daily, highs)
        return boll.load(str(self.path), workers=1)

    def assert_trades_match(self, daily: dict, highs: dict) -> dict:
        dataset = self.analyse(daily, highs)
        expected = reference_trades(daily, highs)
        actual = {trade["entity"]: trade for trade in dataset.trades}
        self.assertEqual(sorted(actual), sorted(expected))
        for entity, reference in expected.items():
            for field, value in reference.items():
                self.assertEqual(actual[entity][field], value, f"{entity}.{field}")
        return actual


class SignalDetectionTest(FixtureCase):
    def test_breakout_pullback_and_reclaim(self) -> None:
        # A flat 1000 run keeps the mid line near 1000; the breakout is the first
        # 2026 bar above the 2025 high, then one close far below the mid line,
        # then a close back above it.
        bars = [(20362 + i, 1000, 1000, 1000, 1000) for i in range(96)]
        bars += [
            (20458, 1010, 1100, 1005, 1090),  # breakout: high 1100 > 1000
            (20459, 1010, 1095, 590, 600),  # close below the mid line
            (20460, 600, 1210, 595, 1200),  # reclaim
            (20461, 1015, 1030, 1010, 1025),  # entry bar, open is the entry price
            (20462, 1025, 1040, 1020, 1035),
        ]
        daily = {"sz.000001": bars}
        highs = {"sz.000001": 1000}
        trade = self.assert_trades_match(daily, highs)["sz.000001"]
        self.assertEqual(trade["bo_day"], 20458)
        self.assertEqual(trade["sig_day"], 20460)
        self.assertEqual(trade["entry_day"], 20461)
        self.assertEqual(trade["entry"], 1015)

    def test_no_breakout_produces_no_trade(self) -> None:
        bars = [(20362 + i, 1000, 1000, 1000, 1000) for i in range(90)]
        bars += [(20458 + i, 900, 950, 890, 940) for i in range(10)]
        dataset = self.analyse({"sz.000002": bars}, {"sz.000002": 1000})
        self.assertEqual(dataset.entities, 1)
        self.assertEqual(dataset.entities_with_2025_high, 1)
        self.assertEqual(dataset.entities_with_breakout, 0)
        self.assertEqual(dataset.trades, [])

    def test_breakout_without_pullback_produces_no_trade(self) -> None:
        bars = [(20362 + i, 1000, 1000, 1000, 1000) for i in range(90)]
        bars += [(20458 + i, 1100 + i * 10, 1150 + i * 10, 1090 + i * 10, 1140 + i * 10)
                 for i in range(10)]
        dataset = self.analyse({"sz.000003": bars}, {"sz.000003": 1000})
        self.assertEqual(dataset.entities_with_breakout, 1)
        self.assertEqual(dataset.trades, [])

    def test_reclaim_after_several_bars_below_the_mid(self) -> None:
        # Three bars below the mid, then the reclaim: the reclaim bar is the
        # signal, not the first bar that is below the mid.
        bars = [(20362 + i, 1000, 1000, 1000, 1000) for i in range(96)]
        bars += [
            (20458, 1010, 1100, 1005, 1090),  # breakout
            (20459, 1010, 1095, 590, 600),  # close below the mid
            (20460, 600, 620, 595, 610),  # below
            (20461, 610, 630, 605, 620),  # below
            (20462, 620, 1210, 615, 1200),  # reclaim
            (20463, 1200, 1220, 1190, 1210),  # entry bar
        ]
        trade = self.assert_trades_match(
            {"sz.000004": bars}, {"sz.000004": 1000}
        )["sz.000004"]
        self.assertEqual(trade["sig_day"], 20462)
        self.assertEqual(trade["entry_day"], 20463)

    def test_signal_on_the_last_bar_has_no_entry(self) -> None:
        bars = [(20362 + i, 1000, 1000, 1000, 1000) for i in range(96)]
        bars += [
            (20458, 1010, 1100, 1005, 1090),
            (20459, 1010, 1095, 590, 600),
            (20460, 600, 1210, 595, 1200),  # reclaim on the last bar of the series
        ]
        dataset = self.analyse({"sz.000005": bars}, {"sz.000005": 1000})
        self.assertEqual(dataset.entities_with_breakout, 1)
        self.assertEqual(dataset.trades, [])

    def test_series_shorter_than_the_reference_minimum_is_dropped(self) -> None:
        # 24 bars is below the reference script's `len(bs) < 25` guard.
        bars = [(20440 + i, 1000, 1010, 990, 1000) for i in range(24)]
        dataset = self.analyse({"sz.000006": bars}, {"sz.000006": 900})
        self.assertEqual(dataset.entities_with_2025_high, 1)
        self.assertEqual(dataset.entities_with_breakout, 0)
        self.assertEqual(dataset.trades, [])

    def test_entity_without_a_2025_yearly_bar_is_skipped(self) -> None:
        bars = [(20362 + i, 1000, 1100, 990, 1000) for i in range(90)]
        dataset = self.analyse({"sz.000007": bars}, {})
        self.assertEqual(dataset.entities, 1)
        self.assertEqual(dataset.entities_with_2025_high, 0)
        self.assertEqual(dataset.trades, [])

    def test_bars_after_last_day_are_ignored(self) -> None:
        bars = [(20362 + i, 1000, 1000, 1000, 1000) for i in range(96)]
        bars += [
            (20458, 1010, 1100, 1005, 1090),
            (20459, 1010, 1095, 590, 600),
            (20460, 600, 1210, 595, 1200),  # reclaim
            (20461, 1000, 1010, 990, 1005),  # entry bar
            (20462, 1005, 1010, 1000, 1008),
            (boll.LAST_DAY + 1, 1008, 5000, 1000, 4000),  # outside the window
        ]
        trade = self.assert_trades_match(
            {"sz.000008": bars}, {"sz.000008": 1000}
        )["sz.000008"]
        self.assertEqual(trade["ex_end"], 20462)  # the last bar inside the window
        self.assertEqual(trade["rend"], 1008 / 1000 - 1)


class ReturnTest(FixtureCase):
    def test_holding_period_returns_and_excursions(self) -> None:
        bars = [(20362 + i, 1000, 1000, 1000, 1000) for i in range(96)]
        bars += [
            (20458, 1010, 1100, 1005, 1090),  # breakout, next open 1010
            (20459, 1010, 1095, 590, 600),  # pullback
            (20460, 600, 1210, 595, 1200),  # reclaim (signal)
            (20461, 1000, 1100, 980, 1080),  # entry bar, open 1000
        ]
        # 61 further bars with distinct closes so every horizon is covered.
        bars += [(20462 + i, 1080 + i, 1080 + i, 1080 + i, 1080 + i) for i in range(61)]
        trade = self.assert_trades_match(
            {"sz.000101": bars}, {"sz.000101": 1000}
        )["sz.000101"]
        self.assertEqual(trade["entry"], 1000)
        self.assertEqual(trade["r5"], (1080 + 4) / 1000 - 1)
        self.assertEqual(trade["r60"], (1080 + 59) / 1000 - 1)
        self.assertIn("ex5", trade)
        self.assertIn("ex60", trade)
        # Buy at the breakout uses the same exit day but the breakout open.
        self.assertEqual(trade["bo_entry"], 1010)
        self.assertEqual(trade["b5"], (1080 + 4) / 1010 - 1)
        self.assertEqual(trade["bend"], bars[-1][4] / 1010 - 1)
        # MAE20 uses closes from the entry bar onwards, MFE/MAE use high/low.
        self.assertEqual(trade["mae20"], min([1080] + [1080 + i for i in range(20)]) / 1000 - 1)

    def test_short_history_leaves_late_horizons_out(self) -> None:
        bars = [(20362 + i, 1000, 1000, 1000, 1000) for i in range(96)]
        bars += [
            (20458, 1010, 1100, 1005, 1090),
            (20459, 1010, 1095, 590, 600),
            (20460, 600, 1210, 595, 1200),
            (20461, 1000, 1010, 995, 1005),  # entry bar, open 1000
            (20462, 1005, 1010, 1000, 1008),
            (20463, 1008, 1012, 1002, 1010),
            (20464, 1010, 1015, 1005, 1012),
            (20465, 1012, 1018, 1008, 1015),
            (20466, 1015, 1020, 1010, 1018),
            (20467, 1018, 1022, 1012, 1020),  # 6 bars after entry: r5 only
        ]
        trade = self.assert_trades_match(
            {"sz.000102": bars}, {"sz.000102": 1000}
        )["sz.000102"]
        self.assertIn("r5", trade)
        self.assertNotIn("r10", trade)
        self.assertNotIn("b10", trade)
        self.assertEqual(trade["rend"], 1020 / 1000 - 1)


class BenchTest(FixtureCase):
    def test_equal_weight_index_skips_entities_absent_on_the_previous_day(self) -> None:
        # Two entities trade every day; the third is missing on 20460, so it is
        # left out of the 20460 cross-section but present on 20461.
        days = [boll.INDEX_START_DAY + i for i in range(5)]
        daily = {
            "sz.000201": [(day, 100, 100, 100, 100 + i) for i, day in enumerate(days)],
            "sz.000202": [(day, 200, 200, 200, 200 + 2 * i) for i, day in enumerate(days)],
            "sz.000203": [
                (day, 300, 300, 300, 300 + 3 * i)
                for i, day in enumerate(days)
                if i != 2
            ],
        }
        dataset = self.analyse(daily, {})
        index = boll._equal_weight_index(dataset.conn)
        expected = reference_index(daily, boll.INDEX_START_DAY)
        self.assertEqual(sorted(index), sorted(expected))
        for day in expected:
            self.assertAlmostEqual(index[day] / index[days[0]], expected[day], places=12)

    def test_index_start_day_does_not_change_the_ratios(self) -> None:
        rng = random.Random(7)
        daily = {
            f"sz.{300 + n:06d}": random_series(rng, 20300, 120) for n in range(4)
        }
        dataset = self.analyse(daily, {})
        index = boll._equal_weight_index(dataset.conn)
        early = reference_index(daily, 20300)
        for later in sorted(index):
            for base in sorted(index):
                if base > later:
                    continue
                self.assertAlmostEqual(
                    index[later] / index[base], early[later] / early[base], places=12
                )


class DetailTest(FixtureCase):
    def test_entries_by_month_and_excursions(self) -> None:
        rng = random.Random(11)
        daily = {}
        highs = {}
        for n in range(6):
            entity = f"sh.{400 + n:06d}"
            daily[entity] = random_series(rng, 20362, 120)
            highs[entity] = min(bar[2] for bar in daily[entity][:60]) + 200
        dataset = self.analyse(daily, highs)
        expected = reference_trades(daily, highs)
        self.assertEqual(len(dataset.trades), len(expected))
        months = {}
        for trade in expected.values():
            month = (
                datetime.date(1970, 1, 1) + datetime.timedelta(days=trade["entry_day"])
            ).strftime("%Y-%m")
            months.setdefault(month, []).append(trade)
        for month, trades in months.items():
            in_month = [
                trade
                for trade in dataset.trades
                if (
                    datetime.date(1970, 1, 1)
                    + datetime.timedelta(days=trade["entry_day"])
                ).strftime("%Y-%m")
                == month
            ]
            self.assertEqual(len(in_month), len(trades))
            for trade in in_month:
                self.assertAlmostEqual(
                    trade["mfe"], expected[trade["entity"]]["mfe"], places=12
                )
                self.assertAlmostEqual(
                    trade["mae"], expected[trade["entity"]]["mae"], places=12
                )


class RandomisedEquivalenceTest(FixtureCase):
    def test_random_fixtures_match_the_reference(self) -> None:
        for seed in range(5):
            with self.subTest(seed=seed):
                rng = random.Random(seed)
                daily = {}
                highs = {}
                for n in range(12):
                    entity = f"sz.{500 + n:06d}"
                    daily[entity] = random_series(rng, 20362, 140)
                    if rng.random() < 0.15:
                        continue  # some entities have no 2025 yearly bar
                    highs[entity] = min(
                        bar[2] for bar in daily[entity][:80]
                    ) + rng.randint(0, 120)
                self.path = Path(self._dir.name) / f"seed{seed}.sqlite"
                self.assert_trades_match(daily, highs)


class SchemaShapeTest(FixtureCase):
    """The analysis reads both the deployed schema-21 and the schema-22 names."""

    def test_schema22_units_columns_match_the_reference(self) -> None:
        rng = random.Random(7)
        daily = {"sz.000903": random_series(rng, 20362, 140)}
        highs = {"sz.000903": min(bar[2] for bar in daily["sz.000903"][:80])}
        write_fixture(self.path, daily, highs, schema=SCHEMA_22)
        dataset = boll.load(str(self.path), workers=1)
        expected = reference_trades(daily, highs)
        self.assertEqual(
            sorted(trade["entity"] for trade in dataset.trades), sorted(expected)
        )
        actual = {trade["entity"]: trade for trade in dataset.trades}
        for entity, reference in expected.items():
            for field, value in reference.items():
                self.assertEqual(actual[entity][field], value, f"{entity}.{field}")

    def test_price_suffix_follows_the_table_shape(self) -> None:
        write_fixture(self.path, {"sz.000904": [(20362, 1, 1, 1, 1)]}, {})
        conn = boll.connect(str(self.path))
        self.addCleanup(conn.close)
        self.assertEqual(boll._price_suffix(conn), "cents")


class ReadOnlyTest(FixtureCase):
    def test_database_is_opened_read_only(self) -> None:
        write_fixture(self.path, {"sz.000901": [(20362, 1, 1, 1, 1)]}, {})
        conn = boll.connect(str(self.path))
        self.addCleanup(conn.close)
        with self.assertRaises(sqlite3.OperationalError):
            conn.execute("DELETE FROM series_bars")

    def test_main_requires_a_database_path(self) -> None:
        import contextlib
        import io

        with contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit):
                boll.main([])

    def test_main_prints_every_variant_by_default(self) -> None:
        rng = random.Random(3)
        daily = {"sz.000902": random_series(rng, 20362, 140)}
        highs = {"sz.000902": min(bar[2] for bar in daily["sz.000902"][:80])}
        write_fixture(self.path, daily, highs)
        import contextlib
        import io

        buffer = io.StringIO()
        with contextlib.redirect_stdout(buffer):
            boll.main(["--db", str(self.path), "--workers", "1"])
        output = buffer.getvalue()
        for name in ("backtest", "bench", "detail"):
            self.assertIn(f"=== {name} ===", output)
        self.assertIn("trades (pullback+reclaim signal):", output)


if __name__ == "__main__":
    unittest.main()
