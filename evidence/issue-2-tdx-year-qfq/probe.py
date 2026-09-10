#!/usr/bin/env python3
"""One-shot, bounded TDX year none/qfq evidence probe for Issue #2."""

import hashlib
import importlib.metadata
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

from eltdx import TdxClient


SYMBOLS = {"000001": "sz000001", "600000": "sh600000"}
ADJUSTMENTS = {"none": None, "qfq": "qfq"}


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def ohlc(bar) -> dict[str, str]:
    return {
        name: repr(getattr(bar, name))
        for name in ("open", "high", "low", "close")
    }


def fetch_once(client: TdxClient, symbol: str, adjust_name: str, adjust_value):
    started = utc_now()
    try:
        series = client.bars.get(
            SYMBOLS[symbol],
            period="year",
            adjust=adjust_value,
            count=50,
            include_raw=True,
        )
        received = utc_now()
        bars = list(series.bars)
        raw = bytes(series.raw_payload)
        return {
            "symbol": symbol,
            "wire_code": SYMBOLS[symbol],
            "period_argument": "year",
            "adjust_argument": adjust_value,
            "adjust_label": adjust_name,
            "count_argument": 50,
            "requested_at_utc": started,
            "received_at_utc": received,
            "status": "success",
            "period_name": series.period_name,
            "period_raw": series.period_raw,
            "period_param_raw": series.period_param_raw,
            "adjust_mode": series.adjust_mode,
            "adjust_mode_raw": series.adjust_mode_raw,
            "bar_count": len(bars),
            "first_date": bars[0].time.date().isoformat() if bars else None,
            "last_date": bars[-1].time.date().isoformat() if bars else None,
            "first_ohlc": ohlc(bars[0]) if bars else None,
            "last_ohlc": ohlc(bars[-1]) if bars else None,
            "raw_response_sha256": hashlib.sha256(raw).hexdigest(),
            "_bars_for_compare": [
                {"date": bar.time.date().isoformat(), "ohlc": ohlc(bar)}
                for bar in bars
            ],
        }
    except Exception as error:  # Record one bounded attempt without retrying.
        return {
            "symbol": symbol,
            "wire_code": SYMBOLS[symbol],
            "period_argument": "year",
            "adjust_argument": adjust_value,
            "adjust_label": adjust_name,
            "count_argument": 50,
            "requested_at_utc": started,
            "received_at_utc": utc_now(),
            "status": "error",
            "error_type": type(error).__name__,
            "error": str(error),
            "raw_response_sha256": None,
            "_bars_for_compare": [],
        }


def main(output: Path) -> None:
    client_version = importlib.metadata.version("eltdx")
    results = []
    # One client, four explicit calls, no application retry or pagination.
    with TdxClient(timeout=8.0, probe_timeout=1.2) as client:
        for symbol in SYMBOLS:
            for adjust_name, adjust_value in ADJUSTMENTS.items():
                results.append(fetch_once(client, symbol, adjust_name, adjust_value))

    comparisons = []
    for symbol in SYMBOLS:
        by_adjust = {row["adjust_label"]: row for row in results if row["symbol"] == symbol}
        none_row = by_adjust.get("none", {})
        qfq_row = by_adjust.get("qfq", {})
        comparisons.append({
            "symbol": symbol,
            "comparison": "none vs qfq OHLC by date",
            "none_raw_response_sha256": none_row.get("raw_response_sha256"),
            "qfq_raw_response_sha256": qfq_row.get("raw_response_sha256"),
            "ohlc_changed_count": None,
            "ohlc_examples": [],
        })
        if none_row.get("status") != "success" or qfq_row.get("status") != "success":
            continue
        none_bars = {bar["date"]: bar["ohlc"] for bar in none_row["_bars_for_compare"]}
        qfq_bars = {bar["date"]: bar["ohlc"] for bar in qfq_row["_bars_for_compare"]}
        common_dates = sorted(none_bars.keys() & qfq_bars.keys())
        changed_dates = [date for date in common_dates if none_bars[date] != qfq_bars[date]]
        comparisons[-1]["ohlc_changed_count"] = len(changed_dates)
        comparisons[-1]["common_date_count"] = len(common_dates)
        comparisons[-1]["ohlc_examples"] = [
            {
                "date": date,
                "none": none_bars[date],
                "qfq": qfq_bars[date],
            }
            for date in changed_dates[:2]
        ]

    public_results = [
        {key: value for key, value in row.items() if not key.startswith("_")}
        for row in results
    ]
    payload = {
        "schema": "x-data-mcp.issue-2.tdx-year-qfq-probe.v1",
        "probe_started_at_utc": results[0]["requested_at_utc"],
        "probe_finished_at_utc": results[-1]["received_at_utc"],
        "source": "tdx-7709",
        "source_version": None,
        "source_version_note": "TDX server version is not exposed by KlineSeries/eltdx.",
        "client": "eltdx",
        "client_version": client_version,
        "client_release_commit": "086502a480600595a6fd407be38bc979eefd7c87",
        "client_release_url": "https://github.com/electkismet/eltdx/releases/tag/v3.1.7",
        "timeout_seconds": 8.0,
        "probe_timeout_seconds": 1.2,
        "retry_policy": "no application retry; one call per symbol/adjustment; no pagination",
        "results": public_results,
        "comparisons": comparisons,
        "interpretation": {
            "parameter_mapping": "The v3.1.7 docs map year to 11/1 and qfq to adjustment value 1.",
            "native_year_vs_local_aggregation": "The docs state 0x052d directly returns none/qfq and distinguish local adjustment_factors; this is source documentation, not an independent wire capture.",
            "qfq_semantics": "UNKNOWN until an authoritative adjustment-base/corporate-action reconciliation is available.",
            "coverage": "UNKNOWN beyond the bounded sample/page because count=50 is not a completeness proof.",
            "unit_currency": "UNKNOWN; this probe does not establish price currency/unit or amount currency/unit.",
            "date_as_of": "UNKNOWN; bar dates and receive time are recorded, but receive time is not a source cutoff proof.",
        },
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(payload, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: probe.py OUTPUT.json")
    main(Path(sys.argv[1]))
