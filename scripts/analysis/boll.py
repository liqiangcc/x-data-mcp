#!/usr/bin/env python3
"""Whole-market 2026 "first breakout -> pullback -> reclaim BOLL20 mid" backtest.

This is the optimised replacement for the three ad-hoc scripts captured in the
#373 comment (``boll_backtest.py`` / ``boll_bench.py`` / ``boll_detail.py``).
The signal definition and every reported statistic are unchanged; only the data
access changed:

* the database path is a command-line argument and is opened ``mode=ro``;
* no full history and no full table scan: ``series_bars`` has no ``trade_day``
  index (``migrations/tasks/021_latest_only.sql``), so the bars are read per
  series with a range seek on the ``(series_key, trade_day)`` primary key, and
  only the bars a signal can be computed from are read;
* the 20-day SMA and the "pullback then reclaim" state machine are pushed down
  into SQL window functions (SQLite >= 3.25, checked at run time), so only two
  day numbers per series reach Python; the forward returns are index arithmetic
  over the ~62 bars that follow a signal, which is one small slice per signal;
* the three variants share one load: ``--variant`` selects them, all of them run
  from a single pass over the database in one process (no ``exec`` re-load).

Usage::

    python3 scripts/analysis/boll.py --db <path-to-sqlite> [--variant backtest] ...

Every variant defaults to on.  The script only reads; it never writes to the
database, and it is not part of the service, the MCP surface or the schema.
"""
from __future__ import annotations

import argparse
import collections
import datetime
import sqlite3
import sys
import threading
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from pathlib import Path
from typing import Callable

# trade_day is a day number since 1970-01-01 (src/compact_market.rs:798-801).
_EPOCH = datetime.date(1970, 1, 1)


def epoch_day(year: int, month: int, day: int) -> int:
    """Day number used by ``series_bars.trade_day``."""
    return (datetime.date(year, month, day) - _EPOCH).days


def epoch_iso(day: int) -> str:
    """ISO date for a ``trade_day`` value."""
    return (_EPOCH + datetime.timedelta(days=day)).isoformat()


# 2025 calendar year: the reference script filters the yearly series with
# strftime('%Y', ...) = 2025, which is exactly this day range.
YEAR_2025_START = epoch_day(2025, 1, 1)
YEAR_2025_END = epoch_day(2025, 12, 31)
# First trading day of the 2026 breakout window, and the right edge of it.
BREAKOUT_START_DAY = epoch_day(2026, 1, 5)
LAST_DAY = epoch_day(2026, 10, 23)
# Holding periods, in bars, for the strategy and for the buy-at-breakout
# comparison.
HORIZONS = (5, 10, 20, 60)
# The equal-weight index is only ever read on or after the first possible
# signal day (a signal is strictly after a breakout, and breakouts start here),
# so the index can start here instead of 2025-07-31.  Every ratio
# idx[later]/idx[earlier] the bench reports is unaffected by the base day.
INDEX_START_DAY = BREAKOUT_START_DAY
# Bars kept after the signal bar: the entry bar plus the longest holding period.
FORWARD_BARS = 1 + max(HORIZONS)

SMA_PERIOD = 20  # BOLL20 mid line
MIN_BARS = 25  # the reference script drops series shorter than this
# Series are independent, and a cold run is bound by the latency of the random
# page reads that reach one series each (~4 per series on this schema, because
# series_bars has no trade_day index).  Overlapping them is what turns a cold
# run from ~11s into ~4.6s; see the measurement in the pull request.  The value
# is a small constant on purpose: this runs on the host that also serves
# production.
DEFAULT_WORKERS = 4

# One row per entity that has a 2025 yearly bar.  Yearly bars carry the year's
# last trading day (migrations/tasks/021_latest_only.sql), so the year filter is
# a plain trade_day range.
_YEARLY_HIGH_SQL = """
SELECT s.entity AS entity, max(b.high_cents) AS high_cents
FROM market_series s
JOIN series_bars b ON b.series_key = s.series_key
WHERE s.period = 2 AND s.adjustment = 1 AND b.trade_day BETWEEN ?1 AND ?2
GROUP BY s.entity
"""

_DAILY_SERIES_SQL = """
SELECT series_key, entity, record_count, last_day
FROM market_series
WHERE period = 1 AND adjustment = 1
ORDER BY entity
"""

# First day of the 2026 window whose high beats the 2025 high.  ORDER BY ...
# LIMIT 1 stops at the first hit instead of aggregating the whole window.
_FIRST_BREAKOUT_SQL = """
SELECT trade_day FROM series_bars
WHERE series_key = ?1 AND trade_day >= ?2 AND high_cents > ?3
ORDER BY trade_day
LIMIT 1
"""

# Pullback and reclaim for one series, in one statement.
#
# Parameters: ?1 series_key, ?2 right edge (LAST_DAY), ?3 breakout day,
# ?4 SMA period.  The warm-up start is the SMA period-th bar counting back from
# the breakout day, so the mid line at the first bar after the breakout has all
# the bars it needs and every mid value equals the one the full-history
# reference script computes.  `d` is MATERIALIZED so the window pass runs once
# for both lookups.
#
# The reclaim needs no "previous close was not above the mid" test: by
# definition no bar between the pullback and the first bar back above the mid
# closed above its mid (bars without a mid are skipped by both versions), so
# the first bar above the mid after the pullback is the signal.
_SIGNAL_SQL = """
WITH d AS MATERIALIZED (
  SELECT trade_day AS day, close_cents AS cl,
         CASE WHEN count(*) OVER w = ?4 THEN avg(close_cents) OVER w END AS mid
  FROM series_bars
  WHERE series_key = ?1
    AND trade_day BETWEEN coalesce((
          SELECT trade_day FROM series_bars
          WHERE series_key = ?1 AND trade_day <= ?3
          ORDER BY trade_day DESC LIMIT 1 OFFSET ?4 - 1), 0)
        AND ?2
  WINDOW w AS (ORDER BY trade_day ROWS BETWEEN ?4 - 1 PRECEDING AND CURRENT ROW)
),
pull AS (SELECT min(day) AS day FROM d WHERE day > ?3 AND cl < mid)
SELECT (SELECT day FROM pull) AS pull_day,
       (SELECT min(d.day) FROM d
        WHERE d.day > (SELECT day FROM pull) AND d.cl > d.mid) AS signal_day
"""

# The signal bar and the bars a holding period can reach from the entry bar.
_FORWARD_BARS_SQL = """
SELECT trade_day, open_cents, high_cents, low_cents, close_cents
FROM series_bars
WHERE series_key = ?1 AND trade_day >= ?2 AND trade_day <= ?3
ORDER BY trade_day
LIMIT ?4
"""

# Right edge of the series: the reference script measures "hold to end" and the
# buy-at-breakout comparison against it.
_TAIL_SQL = """
SELECT
  (SELECT trade_day FROM series_bars WHERE series_key = ?1 AND trade_day <= ?3
   ORDER BY trade_day DESC LIMIT 1) AS last_day,
  (SELECT close_cents FROM series_bars WHERE series_key = ?1 AND trade_day <= ?3
   ORDER BY trade_day DESC LIMIT 1) AS last_close,
  (SELECT open_cents FROM series_bars WHERE series_key = ?1
   AND trade_day > ?2 AND trade_day <= ?3 ORDER BY trade_day LIMIT 1) AS bo_entry
"""

# Bars of one series used to build the equal-weight index.
_INDEX_BARS_SQL = """
SELECT trade_day, close_cents FROM series_bars
WHERE series_key = ?1 AND trade_day BETWEEN ?2 AND ?3
ORDER BY trade_day
"""


@dataclass
class Dataset:
    """Result of the single pass over the database shared by every variant."""

    trades: list[dict]
    entities: int
    entities_with_2025_high: int
    entities_with_breakout: int
    conn: sqlite3.Connection


def connect(path: str) -> sqlite3.Connection:
    """Open `path` read-only; the connection is also ``query_only``.

    Each connection is used by one thread at a time (one per worker, plus the
    caller's), so the same-thread guard is off only to let the pool close the
    worker connections from the calling thread.
    """
    uri = Path(path).expanduser().resolve().as_uri() + "?mode=ro"
    conn = sqlite3.connect(uri, uri=True, check_same_thread=False)
    conn.row_factory = sqlite3.Row
    conn.execute("PRAGMA query_only = 1")
    return conn


def _require_window_functions() -> None:
    major, minor, _ = (int(part) for part in sqlite3.sqlite_version.split("."))
    if (major, minor) < (3, 25):
        raise SystemExit(
            f"sqlite {sqlite3.sqlite_version} has no window functions; need >= 3.25"
        )


def _map_series(
    path: str,
    rows: list[sqlite3.Row],
    work: Callable[[sqlite3.Connection, sqlite3.Row], object],
    workers: int,
) -> list:
    """Run `work` over the series, one read-only connection per worker thread.

    `rows` order is preserved, so the reported numbers do not depend on the
    worker count.
    """
    if workers <= 1 or len(rows) < 2:
        conn = connect(path)
        try:
            return [work(conn, row) for row in rows]
        finally:
            conn.close()
    local = threading.local()
    opened: list[sqlite3.Connection] = []
    opened_lock = threading.Lock()

    def task(row: sqlite3.Row) -> object:
        conn = getattr(local, "conn", None)
        if conn is None:
            conn = local.conn = connect(path)
            with opened_lock:
                opened.append(conn)
        return work(conn, row)

    try:
        with ThreadPoolExecutor(max_workers=workers) as pool:
            return list(pool.map(task, rows))
    finally:
        for conn in opened:
            conn.close()


def _bars_up_to_last(conn: sqlite3.Connection, row: sqlite3.Row) -> int:
    """Number of bars at or before LAST_DAY — the reference script's ``len(bs)``."""
    if row["last_day"] <= LAST_DAY:
        return row["record_count"]
    after = conn.execute(
        "SELECT count(*) FROM series_bars WHERE series_key = ?1 AND trade_day > ?2",
        (row["series_key"], LAST_DAY),
    ).fetchone()[0]
    return row["record_count"] - after


def _trade(conn: sqlite3.Connection, key: int, breakout: int, signal: int) -> dict | None:
    """Forward returns and excursions for one signal, in bar order from the entry bar."""
    bars = conn.execute(
        _FORWARD_BARS_SQL, (key, signal, LAST_DAY, FORWARD_BARS + 1)
    ).fetchall()
    if len(bars) < 2:
        return None  # no bar after the signal bar: the reference script drops it too
    entry = bars[1]["open_cents"]
    if entry <= 0:
        return None
    tail = conn.execute(_TAIL_SQL, (key, breakout, LAST_DAY)).fetchone()
    last_close = tail["last_close"]
    trade = {
        "bo_day": breakout,
        "sig_day": bars[0]["trade_day"],
        "entry_day": bars[1]["trade_day"],
        "entry": entry,
        "ex_end": tail["last_day"],
        "rend": last_close / entry - 1,
        "mae20": min(bar["close_cents"] for bar in bars[1:22]) / entry - 1,
        "mfe": max(bar["high_cents"] for bar in bars[1:22]) / entry - 1,
        "mae": min(bar["low_cents"] for bar in bars[1:22]) / entry - 1,
    }
    for hold in HORIZONS:
        exit_bar = bars[1 + hold] if 1 + hold < len(bars) else None
        if exit_bar is not None:
            trade[f"r{hold}"] = exit_bar["close_cents"] / entry - 1
            trade[f"ex{hold}"] = exit_bar["trade_day"]
    bo_entry = tail["bo_entry"]
    if bo_entry and bo_entry > 0:
        trade["bo_entry"] = bo_entry
        for hold in HORIZONS:
            if f"r{hold}" in trade:
                trade[f"b{hold}"] = bars[1 + hold]["close_cents"] / bo_entry - 1
        trade["bend"] = last_close / bo_entry - 1
    return trade


def load(path: str, workers: int = DEFAULT_WORKERS) -> Dataset:
    """One pass over the database: every variant reads from the result."""
    conn = connect(path)
    _require_window_functions()
    highs = {
        row["entity"]: row["high_cents"]
        for row in conn.execute(_YEARLY_HIGH_SQL, (YEAR_2025_START, YEAR_2025_END))
    }
    rows = conn.execute(_DAILY_SERIES_SQL).fetchall()

    def work(series_conn: sqlite3.Connection, row: sqlite3.Row):
        return _series(series_conn, row, highs)

    results = _map_series(path, rows, work, workers)
    trades = [trade for _, _, trade in results if trade is not None]
    return Dataset(
        trades=trades,
        entities=len(rows),
        entities_with_2025_high=sum(1 for with_high, _, _ in results if with_high),
        entities_with_breakout=sum(1 for _, broke_out, _ in results if broke_out),
        conn=conn,
    )


def _series(
    conn: sqlite3.Connection, row: sqlite3.Row, highs: dict[str, int]
) -> tuple[bool, bool, dict | None]:
    """What one series contributes: (has 2025 high, has breakout, trade)."""
    high = highs.get(row["entity"])
    if high is None:
        return False, False, None
    if _bars_up_to_last(conn, row) < MIN_BARS:
        return True, False, None
    key = row["series_key"]
    found = conn.execute(_FIRST_BREAKOUT_SQL, (key, BREAKOUT_START_DAY, high)).fetchone()
    if found is None:
        return True, False, None
    breakout = found[0]
    signal = conn.execute(
        _SIGNAL_SQL, (key, LAST_DAY, breakout, SMA_PERIOD)
    ).fetchone()["signal_day"]
    if signal is None:
        return True, True, None
    trade = _trade(conn, key, breakout, signal)
    if trade is not None:
        trade["entity"] = row["entity"]
    return True, True, trade


def _stats(values: list[float]) -> dict | None:
    """Quantile/summary block, unchanged from the reference script."""
    if not values:
        return None
    ordered = sorted(values)
    count = len(ordered)

    def quantile(p: float) -> float:
        return ordered[min(count - 1, max(0, int(p * count)))]

    return {
        "n": count,
        "win": sum(1 for value in ordered if value > 0) / count,
        "mean": sum(ordered) / count,
        "median": quantile(0.5),
        "p10": quantile(0.1),
        "p90": quantile(0.9),
        "min": ordered[0],
        "max": ordered[-1],
    }


def _mean_sd(values: list[float]) -> tuple[int, float, float]:
    count = len(values)
    mean = sum(values) / count
    sd = (sum((value - mean) ** 2 for value in values) / (count - 1)) ** 0.5
    return count, mean, sd


def _t_stat(values: list[float]) -> tuple[int, float, float, float] | None:
    if len(values) < 2:
        return None
    count, mean, sd = _mean_sd(values)
    return count, mean, sd, mean / (sd / count**0.5)


def _round(stats: dict | None) -> dict | None:
    if stats is None:
        return None
    return {
        key: round(value, 4) if isinstance(value, float) else value
        for key, value in stats.items()
    }


def _column(trades: list[dict], key: str) -> list[float]:
    return [trade[key] for trade in trades if key in trade]


def report_backtest(dataset: Dataset) -> None:
    """Main backtest: strategy and buy-at-breakout statistics per holding period."""
    trades = dataset.trades
    print(f"entities with a daily series: {dataset.entities}")
    print(f"entities with a 2025 yearly high: {dataset.entities_with_2025_high}")
    print(f"stocks with first breakout: {dataset.entities_with_breakout}")
    print(f"trades (pullback+reclaim signal): {len(trades)}")
    for hold in HORIZONS:
        print(f"--- hold {hold}d ---")
        print(" strategy", _round(_stats(_column(trades, f"r{hold}"))))
        if any(f"b{hold}" in trade for trade in trades):
            print(" breakout", _round(_stats(_column(trades, f"b{hold}"))))
    print("--- hold to end ---")
    print(" strategy", _round(_stats(_column(trades, "rend"))))
    print(" breakout", _round(_stats(_column(trades, "bend"))))
    print("--- MAE20 ---", _round(_stats(_column(trades, "mae20"))))
    print("--- t-stats ---")
    for key in ("r5", "r10", "r20", "r60", "rend"):
        print(key, _t_stat(_column(trades, key)))
    print("--- mean/std ---")
    for key in ("r5", "r10", "r20", "r60", "b5", "b10", "b20", "b60"):
        values = _column(trades, key)
        if not values:
            continue
        count, mean, sd = _mean_sd(values)
        print(key, (count, round(mean, 4), round(sd, 4), round(mean / sd, 4)))


def _equal_weight_index(conn: sqlite3.Connection) -> dict[int, float]:
    """Equal-weight index of all daily series, built from one streaming pass.

    For every series the return between consecutive bars is accumulated per
    ``(previous day, day)`` pair; the index only uses the pairs whose previous
    day is the previous trading day, exactly like the reference script (an
    entity that did not trade on the previous day is left out of that day's
    cross-section).  Accumulating on those pairs keeps memory at a few hundred
    counters instead of a close price per entity and day.
    """
    accumulated: dict[tuple[int, int], list[float]] = {}
    for row in conn.execute(_DAILY_SERIES_SQL).fetchall():
        previous_day = None
        previous_close = 0
        for bar in conn.execute(
            _INDEX_BARS_SQL, (row["series_key"], INDEX_START_DAY, LAST_DAY)
        ):
            if previous_day is not None:
                bucket = accumulated.setdefault((previous_day, bar[0]), [0.0, 0])
                bucket[0] += bar[1] / previous_close - 1
                bucket[1] += 1
            previous_day = bar[0]
            previous_close = bar[1]
    days = sorted({day for _, day in accumulated} | {prev for prev, _ in accumulated})
    index: dict[int, float] = {}
    if not days:
        return index
    index[days[0]] = 1.0
    for previous, day in zip(days, days[1:]):
        bucket = accumulated.get((previous, day))
        step = bucket[0] / bucket[1] if bucket else 0.0
        index[day] = index[previous] * (1 + step)
    return index


def report_bench(dataset: Dataset) -> None:
    """Equal-weight market index and excess returns over it."""
    index = _equal_weight_index(dataset.conn)

    def market_return(start: int, end: int) -> float | None:
        if start not in index or end not in index:
            return None
        return index[end] / index[start] - 1

    span = f"{epoch_iso(INDEX_START_DAY)}..{epoch_iso(LAST_DAY)[5:]}"
    headline = market_return(INDEX_START_DAY, LAST_DAY)
    print(f"index {span}", None if headline is None else round(headline, 4))

    def excess(hold: int | None) -> list[float]:
        values = []
        for trade in dataset.trades:
            if hold is None:
                start, end, result = trade["sig_day"], trade["ex_end"], trade["rend"]
            elif f"r{hold}" in trade and f"ex{hold}" in trade:
                start, end, result = trade["sig_day"], trade[f"ex{hold}"], trade[f"r{hold}"]
            else:
                continue
            market = market_return(start, end)
            if market is not None:
                values.append(result - market)
        return values

    for hold in HORIZONS + (None,):
        values = excess(hold)
        if not values:
            continue
        ordered = sorted(values)
        count = len(ordered)
        label = "end" if hold is None else f"{hold}d"
        print(
            f"{label} excess n={count} mean={sum(values) / count:.4f} "
            f"median={ordered[count // 2]:.4f} "
            f"win={sum(1 for value in ordered if value > 0) / count:.3f}"
        )
    for hold in HORIZONS:
        values = excess(hold)
        if values:
            print("excess", hold, _t_stat(values))
    values = excess(None)
    if values:
        print("excess end", _t_stat(values))


def report_detail(dataset: Dataset) -> None:
    """Entries by month, 20-day excursion statistics."""
    trades = dataset.trades
    months = collections.Counter(epoch_iso(trade["entry_day"])[:7] for trade in trades)
    print("entries by month", sorted(months.items()))
    for month in sorted(months):
        in_month = [
            trade for trade in trades if epoch_iso(trade["entry_day"])[:7] == month
        ]
        r20 = _column(in_month, "r20")
        r5 = _column(in_month, "r5")
        if not r20:
            continue
        print(
            month,
            "n",
            len(r20),
            "5d mean %.4f" % (sum(r5) / len(r5)),
            "20d mean %.4f" % (sum(r20) / len(r20)),
            "20d win %.3f" % (sum(1 for value in r20 if value > 0) / len(r20)),
        )
    mfe = _column(trades, "mfe")
    mae = _column(trades, "mae")
    if not mfe:
        return
    print(
        "20d MFE mean %.4f median %.4f  >=+10%% %.3f"
        % (
            sum(mfe) / len(mfe),
            sorted(mfe)[len(mfe) // 2],
            sum(1 for value in mfe if value >= 0.10) / len(mfe),
        )
    )
    print(
        "20d MAE mean %.4f median %.4f  <=-10%% %.3f"
        % (
            sum(mae) / len(mae),
            sorted(mae)[len(mae) // 2],
            sum(1 for value in mae if value <= -0.10) / len(mae),
        )
    )


VARIANTS = {
    "backtest": report_backtest,
    "bench": report_bench,
    "detail": report_detail,
}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--db", required=True, help="SQLite database file (opened read-only)"
    )
    parser.add_argument(
        "--variant",
        action="append",
        choices=sorted(VARIANTS),
        help="variant to print; repeatable, default is all of them",
    )
    parser.add_argument(
        "--workers",
        type=int,
        default=DEFAULT_WORKERS,
        help=f"threads overlapping the per-series reads (default {DEFAULT_WORKERS})",
    )
    args = parser.parse_args(argv)
    variants = args.variant or list(VARIANTS)
    dataset = load(args.db, workers=args.workers)
    try:
        for name in variants:
            print(f"=== {name} ===")
            VARIANTS[name](dataset)
    finally:
        dataset.conn.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
