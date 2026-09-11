#!/usr/bin/env python3
"""Offline 000001 verification of the frozen PR #51 6dp output rule."""

from __future__ import annotations

import hashlib
import json
import struct
import sys
import zipfile
from datetime import date
from decimal import Decimal, ROUND_HALF_UP, getcontext
from pathlib import Path

getcontext().prec = 100
CODE = "000001"
DAY_MEMBER = "sz\\lday\\sz000001.day"
EXPECTED_DAY_SHA256 = "8db365d39beaa0e2178717adcdc1e411f63295f87c647b8bf6aa56d9762dfc7b"
DATES = ("2017-05-24", "2017-05-25", "2017-05-26")
CENT = Decimal("0.01")
FACTOR_6DP = Decimal("0.000001")
MAPPING = {"C1": "fenhong", "C2": "peigujia", "C3": "songzhuangu", "C4": "peigu"}


def dec(value: object) -> Decimal:
    return Decimal(str(value))


def iso(raw: int) -> str:
    return f"{raw // 10000:04d}-{raw // 100 % 100:02d}-{raw % 100:02d}"


def valid_date(value: str) -> bool:
    try:
        date.fromisoformat(value)
        return True
    except ValueError:
        return False


def read_day(zip_path: Path) -> tuple[dict[str, dict], dict]:
    with zipfile.ZipFile(zip_path) as archive:
        raw = archive.read(DAY_MEMBER)
    actual_sha256 = hashlib.sha256(raw).hexdigest()
    if actual_sha256 != EXPECTED_DAY_SHA256:
        raise ValueError(f"unexpected {DAY_MEMBER} SHA256: {actual_sha256}")
    if len(raw) % 32:
        raise ValueError(".day byte length is not divisible by 32")
    rows = {}
    for offset in range(0, len(raw), 32):
        record = raw[offset : offset + 32]
        date_raw, open_raw, high_raw, low_raw, close_raw = struct.unpack_from("<5I", record)
        current_date = iso(date_raw)
        if not valid_date(current_date):
            raise ValueError(f"invalid .day date {date_raw}")
        rows[current_date] = {
            "record_index": offset // 32,
            "raw_hex": record.hex(),
            "open_raw_int": open_raw,
            "high_raw_int": high_raw,
            "low_raw_int": low_raw,
            "close_raw_int": close_raw,
            "open": str(dec(open_raw) / 100),
            "high": str(dec(high_raw) / 100),
            "low": str(dec(low_raw) / 100),
            "close": str(dec(close_raw) / 100),
        }
    return rows, {"member": DAY_MEMBER, "bytes": len(raw), "sha256": actual_sha256, "record_count": len(rows), "record_width": 32}


def map_events(target: dict, day: dict[str, dict]) -> list[dict]:
    events = []
    first_day = min(day)
    for record in target["target_records"][CODE]["records"]:
        if record["category"] != 1:
            continue
        event_date = record["date_iso"]
        mapped = {MAPPING[f"C{i}"]: dec(record["float32_repr_diagnostic"][i - 1]) for i in range(1, 5)}
        event = {
            "record_index": record["record_index"],
            "event_date": event_date,
            "category": 1,
            "clear_hex": record["clear_hex"],
            "float32_raw_bits_hex": record["float32_raw_bits_hex"],
            "float32_repr_diagnostic": record["float32_repr_diagnostic"],
            "mapping": MAPPING,
            "mapped_decimal_inputs": {key: str(value) for key, value in mapped.items()},
        }
        if event_date < first_day:
            event.update({
                "domain_class": "pre_history",
                "status": "pre_history",
                "previous_trading_date": None,
                "unresolved_reason": "event precedes first available .day date and cannot affect any available bar",
            })
            events.append(event)
            continue
        if event_date == first_day:
            event.update({
                "domain_class": "non_relevant_on_first_day",
                "status": "non_relevant_on_first_day",
                "previous_trading_date": None,
                "unresolved_reason": "strict event_date > bar_date excludes an event on the first available bar",
            })
            events.append(event)
            continue
        prior_dates = [current_date for current_date in day if current_date < event_date]
        previous_date = max(prior_dates) if prior_dates else None
        event["domain_class"] = "relevant"
        event["previous_trading_date"] = previous_date
        if previous_date is None:
            event.update({"status": "unresolved", "unresolved_reason": "no earlier .day trading record"})
            events.append(event)
            continue
        previous_close = dec(day[previous_date]["close"])
        denominator = Decimal(10) + mapped["peigu"] + mapped["songzhuangu"]
        if denominator == 0:
            event.update({"status": "unresolved", "unresolved_reason": "formula denominator is zero"})
            events.append(event)
            continue
        reference = (previous_close * 10 - mapped["fenhong"] + mapped["peigu"] * mapped["peigujia"]) / denominator
        reference_cent = reference.quantize(CENT, rounding=ROUND_HALF_UP)
        event.update({
            "status": "resolved",
            "previous_close": str(previous_close),
            "reference": str(reference),
            "reference_cent": str(reference_cent),
            "event_ratio": str(reference_cent / previous_close),
        })
        events.append(event)
    return events


def cumulative_factor(current_date: str, events: list[dict]) -> Decimal:
    factor = Decimal(1)
    for event in events:
        if event["status"] == "resolved" and event["event_date"] > current_date:
            factor *= dec(event["event_ratio"])
    return factor


def output_factor(current_date: str, events: list[dict]) -> Decimal:
    return cumulative_factor(current_date, events).quantize(FACTOR_6DP, rounding=ROUND_HALF_UP)


def comparison(actual: Decimal, expected: Decimal) -> dict:
    return {"actual": str(actual), "expected": str(expected), "difference": str(actual - expected), "exact_equal": actual == expected}


def fixed_history(daily_probe: dict, day: dict[str, dict], events: list[dict]) -> dict:
    query = next(
        item for item in daily_probe["initial_session"]["queries"]
        if item["label"] == "fixed_history" and item["request"]["code"] == "sz.000001"
    )
    response = query["response"]
    positions = {field: index for index, field in enumerate(response["fields"])}
    source_rows = {row[positions["date"]]: row for row in response["rows"]}
    expected_dates = sorted(source_rows)
    matched_dates = sorted(set(expected_dates) & set(day))
    missing_dates = sorted(set(expected_dates) - set(day))
    comparisons = {}
    exact_count = 0
    mismatch_count = 0
    first_mismatch = None
    inference = {}
    fields = ("open", "high", "low", "close")
    for current_date in expected_dates:
        if current_date not in day:
            continue
        row = source_rows[current_date]
        factor = output_factor(current_date, events)
        raw = {field: dec(day[current_date][field]) for field in fields}
        source = {field: dec(row[positions[field]]) for field in fields}
        row_comparisons = {}
        for field in fields:
            item = comparison(raw[field] * factor, source[field])
            item.update({"raw_day_value": str(raw[field]), "factor_6dp": str(factor), "source_qfq_value": str(source[field])})
            row_comparisons[field] = item
            if item["exact_equal"]:
                exact_count += 1
            else:
                mismatch_count += 1
                if first_mismatch is None:
                    first_mismatch = {"date": current_date, "field": field, "raw": str(raw[field]), "factor": str(factor), "local": str(raw[field] * factor), "source": str(source[field]), "diff": item["difference"]}
        comparisons[current_date] = row_comparisons
        quotients = {field: source[field] / raw[field] for field in fields if raw[field] != 0}
        rounded = {field: value.quantize(FACTOR_6DP, rounding=ROUND_HALF_UP) for field, value in quotients.items()}
        consistent = len(quotients) == 4 and len(set(rounded.values())) == 1
        inference[current_date] = {
            "quotients": {field: str(value) for field, value in quotients.items()},
            "rounded_6dp": {field: str(value) for field, value in rounded.items()},
            "all_four_ohlc_nonzero": len(quotients) == 4,
            "consistent_6dp": consistent,
            "reason": "all four OHLC quotients agree at 6dp" if consistent else "not asserted due zero or disagreement",
        }
    consistent_dates = [current_date for current_date, item in inference.items() if item["consistent_6dp"]]
    return {
        "request": query["request"],
        "expected_source_row_count": 411,
        "actual_source_row_count": len(source_rows),
        "matched_date_count": len(matched_dates),
        "matched_dates_first": matched_dates[0] if matched_dates else None,
        "matched_dates_last": matched_dates[-1] if matched_dates else None,
        "missing_dates": missing_dates,
        "field_count": len(expected_dates) * 4,
        "exact_match_count": exact_count,
        "mismatch_count": mismatch_count,
        "first_mismatch": first_mismatch,
        "comparisons": comparisons,
        "factor_inference": {
            "consistent_6dp_count": len(consistent_dates),
            "first_inconsistent": next((current_date for current_date in inference if not inference[current_date]["consistent_6dp"]), None),
            "rows": inference,
        },
    }


def main() -> None:
    target_path, zip_path, daily_probe_path, output_path = map(Path, sys.argv[1:5])
    target = json.loads(target_path.read_text())
    daily_probe = json.loads(daily_probe_path.read_text())
    day, day_meta = read_day(zip_path)
    events = map_events(target, day)
    pre_history_events = [event for event in events if event["domain_class"] == "pre_history"]
    non_relevant_events = [event for event in events if event["domain_class"] == "non_relevant_on_first_day"]
    relevant_events = [event for event in events if event["domain_class"] == "relevant"]
    relevant_unresolved_events = [event for event in relevant_events if event["status"] != "resolved"]
    total_unresolved_events = [event for event in events if event["status"] == "unresolved"]
    for event in events:
        if event["status"] == "resolved":
            event["previous_date_factor_6dp"] = str(output_factor(event["previous_trading_date"], events))
            event["event_date_factor_6dp"] = str(output_factor(event["event_date"], events))
    window = fixed_history(daily_probe, day, events)
    latest_event = max(event["event_date"] for event in events)
    anchor_factor = output_factor("2026-09-11", events)
    all_exact = (
        not relevant_unresolved_events
        and window["actual_source_row_count"] == 411
        and window["matched_date_count"] == 411
        and not window["missing_dates"]
        and window["exact_match_count"] == window["field_count"]
        and window["mismatch_count"] == 0
    )
    selected_factors = {current_date: str(output_factor(current_date, events)) for current_date in sorted(set(DATES + ("2026-09-11",)))}
    output = {
        "schema": "x-data-mcp.issue-3.tdx-000001-qfq-compat.v1",
        "scope": {
            "code": CODE,
            "rule": "reference ROUND_HALF_UP 0.01; event_ratio=reference_cent/previous raw close; factor=product of event_date>d ratios; factor ROUND_HALF_UP 0.000001 before OHLC",
            "mapping": MAPPING,
            "arithmetic": "Decimal only; raw .day OHLC divided by 100; no final OHLC rounding",
            "diagnostic_boundary": "evidence-only output compatibility, not TDX/BaoStock official contract",
        },
        "inputs": {
            "target_summary": "evidence/issue-3-tdx-gbbq-community/target-summary.json",
            "day_zip": "official hsjday.zip retained in /tmp",
            "daily_probe": "evidence/issue-2-baostock-daily/probe-result.json",
            "day_member": day_meta,
        },
        "daily_source": {"first_date": min(day), "last_date": max(day), "record_count": len(day), **day_meta},
        "events": {
            "category": 1,
            "count": len(events),
            "first_date": min(event["event_date"] for event in events),
            "last_date": latest_event,
            "pre_history_event_count": len(pre_history_events),
            "pre_history_events": pre_history_events,
            "non_relevant_event_count": len(non_relevant_events),
            "non_relevant_events": non_relevant_events,
            "relevant_event_count": len(relevant_events),
            "relevant_resolved_count": len(relevant_events) - len(relevant_unresolved_events),
            "relevant_unresolved_count": len(relevant_unresolved_events),
            "relevant_unresolved_events": relevant_unresolved_events,
            "resolved_count": len(events) - len(total_unresolved_events),
            "unresolved_count": len(total_unresolved_events),
            "total_unresolved_count": len(total_unresolved_events),
            "records": events,
        },
        "available_daily_domain": {
            "first_day": min(day),
            "last_day": max(day),
            "pre_history_rule": "event_date < first_day is retained without previous_close/ratio and cannot affect an available bar",
            "relevant_rule": "event_date > first_day can affect at least the first available bar under strict event_date > bar_date",
            "first_day_rule": "event_date == first_day does not affect the first bar under strict event_date > bar_date",
            "pre_history_event_count": len(pre_history_events),
            "relevant_event_count": len(relevant_events),
            "relevant_unresolved_count": len(relevant_unresolved_events),
        },
        "selected_factors": selected_factors,
        "fixed_history": window,
        "anchor": {"anchor_date": "2026-09-11", "latest_event_date": latest_event, "factor_6dp": str(anchor_factor), "latest_event_before_anchor": latest_event < "2026-09-11", "anchor_semantics_pass": latest_event < "2026-09-11" and anchor_factor == Decimal(1)},
        "conclusions": {
            "output_compatibility": "BAOSTOCK_000001_OUTPUT_COMPATIBLE_WITH_FIXED_TDX_LOCAL_QFQ_RULE" if all_exact else "BAOSTOCK_000001_OUTPUT_MISMATCH_WITH_FIXED_TDX_LOCAL_QFQ_RULE",
            "tdx_official_qfq_contract": "NOT_PROVEN",
            "production_sufficiency": "NOT_VERIFIED",
        },
    }
    output_path.write_text(json.dumps(output, ensure_ascii=False, indent=2, sort_keys=True) + "\n")
    print(json.dumps({"event_count": len(events), "first_event": output["events"]["first_date"], "last_event": latest_event, "pre_history": len(pre_history_events), "relevant": len(relevant_events), "relevant_unresolved": len(relevant_unresolved_events), "total_unresolved": len(total_unresolved_events), "window_rows": window["actual_source_row_count"], "matched_dates": window["matched_date_count"], "exact": window["exact_match_count"], "fields": window["field_count"], "mismatch": window["mismatch_count"], "first_mismatch": window["first_mismatch"], "consistent_6dp": window["factor_inference"]["consistent_6dp_count"], "conclusions": output["conclusions"]}, ensure_ascii=False, sort_keys=True))


if __name__ == "__main__":
    main()
