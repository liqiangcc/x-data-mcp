#!/usr/bin/env python3
"""Validate four fixed cross-market samples with the frozen Decimal QFQ rule."""

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
CENT = Decimal("0.01")
FACTOR_6DP = Decimal("0.000001")
FIELDS = ("open", "high", "low", "close")
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


def read_day(archive: zipfile.ZipFile, item: dict) -> tuple[dict[str, dict], dict]:
    raw = archive.read(item["member"])
    actual_sha = hashlib.sha256(raw).hexdigest()
    if actual_sha != item["day"]["sha256"]:
        raise ValueError(f"{item['member']} SHA mismatch: {actual_sha}")
    if len(raw) % 32:
        raise ValueError(f"{item['member']} length is not divisible by 32")
    rows: dict[str, dict] = {}
    for offset in range(0, len(raw), 32):
        record = raw[offset : offset + 32]
        date_raw, open_raw, high_raw, low_raw, close_raw = struct.unpack_from("<5I", record)
        current_date = iso(date_raw)
        if not valid_date(current_date):
            raise ValueError(f"invalid date {date_raw} in {item['member']}")
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
    meta = {
        "member": item["member"],
        "bytes": len(raw),
        "sha256": actual_sha,
        "record_count": len(rows),
        "first_date": min(rows),
        "last_date": max(rows),
    }
    return rows, meta


def map_events(records: list[dict], day: dict[str, dict]) -> list[dict]:
    first_day = min(day)
    events = []
    for record in records:
        event_date = record["date_iso"]
        mapped = {MAPPING[f"C{i}"]: dec(record["float32_repr_diagnostic"][i - 1]) for i in range(1, 5)}
        event = {
            "record_index": record["record_index"],
            "event_date": event_date,
            "category": 1,
            "mapping": MAPPING,
            "mapped_decimal_inputs": {key: str(value) for key, value in mapped.items()},
            "clear_hex": record["clear_hex"],
            "float32_raw_bits_hex": record["float32_raw_bits_hex"],
            "float32_repr_diagnostic": record["float32_repr_diagnostic"],
        }
        if event_date < first_day:
            event.update({"domain_class": "pre_history", "status": "pre_history", "previous_trading_date": None})
            events.append(event)
            continue
        if event_date == first_day:
            event.update({"domain_class": "non_relevant_on_first_day", "status": "non_relevant_on_first_day", "previous_trading_date": None})
            events.append(event)
            continue
        previous_date = max((current for current in day if current < event_date), default=None)
        event.update({"domain_class": "relevant", "previous_trading_date": previous_date})
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


def output_factor(current_date: str, events: list[dict]) -> Decimal:
    factor = Decimal(1)
    for event in events:
        if event["status"] == "resolved" and event["event_date"] > current_date:
            factor *= dec(event["event_ratio"])
    return factor.quantize(FACTOR_6DP, rounding=ROUND_HALF_UP)


def compare_source(query: dict, day: dict[str, dict], events: list[dict]) -> dict:
    if query.get("query") is None:
        return {"source_status": "failure", "failure": query.get("failure"), "compatible": False}
    response = query["query"]
    positions = {field: index for index, field in enumerate(response["fields"])}
    rows = response["rows"]
    source_dates = [row[positions["date"]] for row in rows]
    matched_dates = sorted(set(source_dates) & set(day))
    missing_dates = sorted(set(source_dates) - set(day))
    comparisons = {}
    exact_count = 0
    mismatch_count = 0
    first_mismatch = None
    mismatch_dates = []
    inference = {}
    for current_date in matched_dates:
        row = rows[source_dates.index(current_date)]
        factor = output_factor(current_date, events)
        raw = {field: dec(day[current_date][field]) for field in FIELDS}
        source = {field: dec(row[positions[field]]) for field in FIELDS}
        row_result = {}
        row_has_mismatch = False
        for field in FIELDS:
            local = raw[field] * factor
            equal = local == source[field]
            row_result[field] = {
                "raw_day_value": str(raw[field]),
                "factor_6dp": str(factor),
                "local_value": str(local),
                "source_qfq_value": str(source[field]),
                "difference": str(local - source[field]),
                "exact_equal": equal,
            }
            if equal:
                exact_count += 1
            else:
                mismatch_count += 1
                row_has_mismatch = True
                if first_mismatch is None:
                    first_mismatch = {"date": current_date, "field": field, "raw": str(raw[field]), "factor": str(factor), "local": str(local), "source": str(source[field]), "difference": str(local - source[field])}
        if row_has_mismatch:
            mismatch_dates.append(current_date)
        comparisons[current_date] = row_result
        quotients = {field: source[field] / raw[field] for field in FIELDS if raw[field] != 0}
        rounded = {field: value.quantize(FACTOR_6DP, rounding=ROUND_HALF_UP) for field, value in quotients.items()}
        inference[current_date] = {
            "all_four_ohlc_nonzero": len(quotients) == 4,
            "rounded_6dp": {field: str(value) for field, value in rounded.items()},
            "consistent_6dp": len(quotients) == 4 and len(set(rounded.values())) == 1,
        }
    consistent_dates = [current for current, item in inference.items() if item["consistent_6dp"]]
    compatible = (
        response["error_code"] == "0"
        and not missing_dates
        and exact_count == len(rows) * len(FIELDS)
        and mismatch_count == 0
        and len(rows) > 0
    )
    return {
        "source_status": "success" if response["error_code"] == "0" else "failure",
        "response_error_code": response["error_code"],
        "response_error_msg": response["error_msg"],
        "source_row_count": len(rows),
        "source_first_date": response["first_date"],
        "source_last_date": response["last_date"],
        "matched_date_count": len(matched_dates),
        "matched_dates_first": matched_dates[0] if matched_dates else None,
        "matched_dates_last": matched_dates[-1] if matched_dates else None,
        "missing_dates": missing_dates,
        "field_count": len(rows) * len(FIELDS),
        "exact_match_count": exact_count,
        "mismatch_count": mismatch_count,
        "mismatch_date_count": len(mismatch_dates),
        "mismatch_dates": mismatch_dates,
        "first_mismatch": first_mismatch,
        "factor_inference": {
            "consistent_6dp_count": len(consistent_dates),
            "first_inconsistent": next((current for current in inference if not inference[current]["consistent_6dp"]), None),
            "rows": inference,
        },
        "compatible": compatible,
        "comparisons": comparisons,
    }


def consecutive_source_clusters(rows: list[list[str]], date_index: int, missing: set[str]) -> list[dict]:
    clusters = []
    current = []
    for index, row in enumerate(rows):
        if row[date_index] in missing:
            current.append({"source_row_index": index, "date": row[date_index]})
        elif current:
            clusters.append(current)
            current = []
    if current:
        clusters.append(current)
    return [{
        "start_date": cluster[0]["date"],
        "end_date": cluster[-1]["date"],
        "row_count": len(cluster),
        "source_row_indices": [item["source_row_index"] for item in cluster],
    } for cluster in clusters]


def diagnose_600006(query: dict, day: dict[str, dict], events: list[dict], source_comparison: dict) -> dict:
    target = [event for event in events if event["event_date"] == "2025-07-17"]
    if len(target) != 1:
        raise ValueError("expected exactly one 600006 2025-07-17 category-1 event")
    event = target[0]
    expected_bits = ["0x3d4ccccd", "0x00000000", "0x00000000", "0x00000000"]
    if event["float32_raw_bits_hex"] != expected_bits:
        raise ValueError(f"unexpected 600006 event bits: {event['float32_raw_bits_hex']}")
    if event["previous_trading_date"] != "2025-07-16":
        raise ValueError(f"unexpected 600006 previous date: {event['previous_trading_date']}")
    if day["2025-07-16"]["close"] != "7.25":
        raise ValueError(f"unexpected 600006 previous close: {day['2025-07-16']['close']}")
    mismatch_dates = source_comparison["mismatch_dates"]
    query_response = query["query"]
    positions = {field: index for index, field in enumerate(query_response["fields"])}
    rows_by_date = {row[positions["date"]]: row for row in query_response["rows"]}
    inferred_one_dates = []
    first_inferred_counterexample = None
    for current_date in mismatch_dates:
        row = rows_by_date[current_date]
        quotients = [dec(row[positions[field]]) / dec(day[current_date][field]) for field in FIELDS]
        rounded = [value.quantize(FACTOR_6DP, rounding=ROUND_HALF_UP) for value in quotients]
        if rounded == [Decimal("1.000000")] * 4:
            inferred_one_dates.append(current_date)
        elif first_inferred_counterexample is None:
            first_inferred_counterexample = {"date": current_date, "rounded_6dp": [str(value) for value in rounded]}
    diagnostic_decimal = dec(event["mapped_decimal_inputs"]["fenhong"])
    previous_close = dec(event["previous_close"])
    counterfactual_fenhong = Decimal("0.05")
    counterfactual_reference = (previous_close * 10 - counterfactual_fenhong) / 10
    counterfactual_cent = counterfactual_reference.quantize(CENT, rounding=ROUND_HALF_UP)
    return {
        "reason": "numeric-semantics/float32-normalization blocker",
        "mismatch_window": {
            "date_start": mismatch_dates[0] if mismatch_dates else None,
            "date_end": mismatch_dates[-1] if mismatch_dates else None,
            "date_count": len(mismatch_dates),
            "ohlc_mismatch_count": source_comparison["mismatch_count"],
        },
        "fixed_event": {
            "event_date": event["event_date"],
            "category": event["category"],
            "float32_raw_bits_hex": event["float32_raw_bits_hex"],
            "diagnostic_float32_values": event["float32_repr_diagnostic"],
            "mapped_fenhong_decimal": str(diagnostic_decimal),
            "mapped_c2_c3_c4_decimal": [event["mapped_decimal_inputs"][key] for key in ("peigujia", "songzhuangu", "peigu")],
            "previous_trading_date": event["previous_trading_date"],
            "previous_raw_close": str(previous_close),
            "frozen_reference": event["reference"],
            "frozen_reference_cent": event["reference_cent"],
            "frozen_event_ratio": event["event_ratio"],
            "frozen_output_factor_6dp": output_factor("2025-07-16", events).__str__(),
        },
        "source_factor_diagnostic": {
            "method": "source_qfq/raw_day quotient, each OHLC quotient ROUND_HALF_UP to 6dp",
            "mismatch_date_count": len(mismatch_dates),
            "all_four_ohlc_quotients_1_000000_count": len(inferred_one_dates),
            "all_four_ohlc_quotients_1_000000": len(inferred_one_dates) == len(mismatch_dates),
            "first_date": inferred_one_dates[0] if inferred_one_dates else None,
            "last_date": inferred_one_dates[-1] if inferred_one_dates else None,
            "first_counterexample": first_inferred_counterexample,
        },
        "counterfactual": {
            "status": "GBBQ_FLOAT32_BUSINESS_DECIMAL_NORMALIZATION_NOT_VERIFIED",
            "result": "COUNTERFACTUAL_EXPLAINS_SOURCE_FACTOR",
            "normalization": "C1 diagnostic float32 0.05000000074505806 treated as business Decimal 0.05",
            "reference_formula_inputs": {"previous_close": str(previous_close), "fenhong": "0.05", "peigujia": "0", "songzhuangu": "0", "peigu": "0"},
            "reference": str(counterfactual_reference),
            "reference_cent": str(counterfactual_cent),
            "event_ratio": str(counterfactual_cent / previous_close),
            "explains_source_6dp_factor": counterfactual_cent / previous_close == Decimal(1),
            "contract_status": "not official TDX or BaoStock contract; no PASS promotion",
        },
        "boundary": "Direct Decimal conversion of the Python/IEEE float32 diagnostic tail is not stable enough at half-cent boundaries; TDX GBBQ numeric decimal/display/rounding semantics require separate evidence.",
    }


def diagnose_000004(query: dict, day: dict[str, dict], source_comparison: dict) -> dict:
    response = query["query"]
    positions = {field: index for index, field in enumerate(response["fields"])}
    rows = response["rows"]
    missing = set(source_comparison["missing_dates"])
    missing_rows = [row for row in rows if row[positions["date"]] in missing]
    checks = []
    for row in missing_rows:
        price_values = [row[positions[field]] for field in ("open", "high", "low", "close", "preclose")]
        checks.append({
            "date": row[positions["date"]],
            "prices_equal": len(set(price_values)) == 1,
            "price_values": price_values,
            "volume_empty": row[positions["volume"]] == "",
            "amount_empty": row[positions["amount"]] == "",
        })
    clusters = consecutive_source_clusters(rows, positions["date"], missing)
    first_counterexample = next((item for item in checks if not (item["prices_equal"] and item["volume_empty"] and item["amount_empty"])), None)
    return {
        "reason": "source-row coverage/suspension-placeholder difference",
        "missing_source_row_count": len(missing_rows),
        "missing_day_date_count": len(missing),
        "placeholder_rule": "open=high=low=close=preclose as raw strings, volume='' and amount=''",
        "placeholder_rows_matching": sum(item["prices_equal"] and item["volume_empty"] and item["amount_empty"] for item in checks),
        "placeholder_rows_all_match": len(checks) == 37 and all(item["prices_equal"] and item["volume_empty"] and item["amount_empty"] for item in checks),
        "first_counterexample": first_counterexample,
        "missing_clusters_in_source_row_order": clusters,
        "matched_trading_bar_result": {
            "status": "MATCHED_TRADING_BAR_QFQ_EXACT",
            "matched_dates": source_comparison["matched_date_count"],
            "ohlc_exact_fields": source_comparison["exact_match_count"],
            "ohlc_field_total_on_matched_dates": source_comparison["matched_date_count"] * len(FIELDS),
            "mismatch_count": source_comparison["mismatch_count"],
        },
        "coverage_result": "BAOSTOCK_SUSPENSION_PLACEHOLDER_NOT_PRESENT_IN_TDX_DAY",
        "gate_boundary": "The security remains incompatible under the original all-source-dates-present gate; this is not classified as an OHLC QFQ numeric mismatch.",
    }


def main() -> None:
    selection_path, events_path, probe_path, day_zip_path, output_dir = map(Path, sys.argv[1:6])
    selection = json.loads(selection_path.read_text())
    selected_events = json.loads(events_path.read_text())["selected_events"]
    probe = json.loads(probe_path.read_text())
    probe_by_security = {item["request"]["code"]: item for item in probe["queries"]}
    output_dir.mkdir(parents=True, exist_ok=True)
    per_stock = []
    with zipfile.ZipFile(day_zip_path) as archive:
        for item in selection["selected"]:
            security = f"{item['market']}.{item['code']}"
            day, day_meta = read_day(archive, item)
            events = map_events(selected_events[security], day)
            relevant = [event for event in events if event["domain_class"] == "relevant"]
            unresolved = [event for event in relevant if event["status"] != "resolved"]
            pre_history = [event for event in events if event["domain_class"] == "pre_history"]
            latest_event = max(event["event_date"] for event in events)
            source = compare_source(probe_by_security[security], day, events)
            anchor_factor = output_factor("2026-09-11", events)
            diagnostics = {}
            if security == "sh.600006":
                diagnostics = diagnose_600006(probe_by_security[security], day, events, source)
            elif security == "sz.000004":
                diagnostics = diagnose_000004(probe_by_security[security], day, source)
            summary = {
                "security": security,
                "code": item["code"],
                "market": item["market"],
                "day": day_meta,
                "events": {
                    "category": 1,
                    "total_count": len(events),
                    "first_date": min(event["event_date"] for event in events),
                    "last_date": latest_event,
                    "pre_history_count": len(pre_history),
                    "relevant_count": len(relevant),
                    "relevant_resolved_count": len(relevant) - len(unresolved),
                    "relevant_unresolved_count": len(unresolved),
                    "latest_event_date": latest_event,
                },
                "anchor": {
                    "anchor_date": "2026-09-11",
                    "factor_6dp": str(anchor_factor),
                    "latest_event_before_anchor": latest_event < "2026-09-11",
                    "anchor_semantics_pass": latest_event < "2026-09-11" and anchor_factor == Decimal(1),
                },
                "source_comparison": source,
                "diagnostics": diagnostics,
                "compatible": bool(unresolved == [] and source.get("compatible")),
            }
            (output_dir / f"{item['market']}-{item['code']}.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2, sort_keys=True) + "\n")
            per_stock.append(summary)
    compatible_count = sum(item["compatible"] for item in per_stock)
    aggregate = {
        "schema": "x-data-mcp.issue-3.cross-stock.qfq-validation.v1",
        "scope": {
            "window": {"start": "2025-01-01", "end": "2026-09-10"},
            "selection_sha256": hashlib.sha256(selection_path.read_bytes()).hexdigest(),
            "algorithm": "frozen PR #51/#52 Decimal rule; no formula, precision, event filter, or tolerance changes",
        },
        "incident_excluded": {
            "type": "PRE_PROBE_SELECTION_BUG",
            "excluded_from_validation_conclusions": True,
            "path": "pre-probe-selection-bug.json",
        },
        "selected_securities": [item["security"] for item in per_stock],
        "reason_taxonomy": {
            "sh.600004": "exact pass",
            "sh.600006": "numeric-semantics/float32-normalization blocker",
            "sz.000002": "exact pass",
            "sz.000004": "source-row coverage/suspension-placeholder difference",
        },
        "per_stock": [{"security": item["security"], "compatible": item["compatible"], "source_row_count": item["source_comparison"].get("source_row_count"), "exact_match_count": item["source_comparison"].get("exact_match_count"), "field_count": item["source_comparison"].get("field_count"), "mismatch_count": item["source_comparison"].get("mismatch_count"), "first_mismatch": item["source_comparison"].get("first_mismatch"), "relevant_unresolved_count": item["events"]["relevant_unresolved_count"], "consistent_6dp_count": item["source_comparison"].get("factor_inference", {}).get("consistent_6dp_count")} for item in per_stock],
        "compatible_stock_count": compatible_count,
        "conclusions": {
            "cross_market": "FOUR_STOCK_CROSS_MARKET_OUTPUT_COMPATIBILITY_PASS" if compatible_count == len(per_stock) == 4 else "CROSS_MARKET_VALIDATION_HAS_MISMATCH",
            "tdx_official_qfq_contract": "NOT_PROVEN",
            "full_market_production_sufficiency": "NOT_VERIFIED",
        },
    }
    (output_dir / "aggregate-summary.json").write_text(json.dumps(aggregate, ensure_ascii=False, indent=2, sort_keys=True) + "\n")
    print(json.dumps(aggregate["per_stock"] + [{"conclusions": aggregate["conclusions"]}], ensure_ascii=False, sort_keys=True))


if __name__ == "__main__":
    main()
