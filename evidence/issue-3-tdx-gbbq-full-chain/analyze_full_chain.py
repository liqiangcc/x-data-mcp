#!/usr/bin/env python3
"""Offline full category-1 event-chain reconstruction for 600000.

Inputs are merged evidence already in this checkout and the official ZIP
retained in /tmp. No network, package, or binary-float price arithmetic is
used. The float32 event diagnostics are converted to Decimal strings before
the formula is evaluated.
"""

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
CODE = "600000"
DAY_MEMBER = "sh\\lday\\sh600000.day"
GBBQ_MEMBER = "gbbq"
DATES_2017 = ["2017-05-24", "2017-05-25", "2017-05-26"]
MAPPING = {"C1": "fenhong", "C2": "peigujia", "C3": "songzhuangu", "C4": "peigu"}
EXPECTED_GBBQ_MEMBER_SHA256 = "24f46cecc9b085ee6c0ef126fab33b405b8057747793eb5d25d44e624769ff60"
EXPECTED_DAY_MEMBER_SHA256 = "bc6815ceafa9cdcd24c68df4d8ec43accfc3e724f5d34d2c7d827a5b12ccac8f"
CENT = Decimal("0.01")


def dec(value: object) -> Decimal:
    return Decimal(str(value))


def day_iso(value: int) -> str:
    return f"{value // 10000:04d}-{value // 100 % 100:02d}-{value % 100:02d}"


def valid_iso(value: str) -> bool:
    try:
        date.fromisoformat(value)
    except ValueError:
        return False
    return True


def compare_decimal(actual: Decimal, expected: Decimal) -> dict:
    difference = actual - expected
    return {
        "actual": str(actual),
        "expected": str(expected),
        "difference": str(difference),
        "exact_equal": actual == expected,
        "abs_difference_le_1e-6_diagnostic": abs(difference) <= Decimal("0.000001"),
    }


def compare_price(actual: Decimal, expected: Decimal) -> dict:
    result = compare_decimal(actual, expected)
    result["display_2dp_equal"] = actual.quantize(CENT, rounding=ROUND_HALF_UP) == expected.quantize(CENT, rounding=ROUND_HALF_UP)
    return result


def read_zip_member(zip_path: Path, member: str, expected_sha256: str) -> tuple[bytes, dict]:
    with zipfile.ZipFile(zip_path) as archive:
        raw = archive.read(member)
    actual_sha256 = hashlib.sha256(raw).hexdigest()
    if actual_sha256 != expected_sha256:
        raise ValueError(f"{member} SHA256 mismatch: {actual_sha256}")
    return raw, {"member": member, "bytes": len(raw), "sha256": actual_sha256}


def read_day_rows(zip_path: Path) -> tuple[dict[str, dict], dict]:
    raw, meta = read_zip_member(zip_path, DAY_MEMBER, EXPECTED_DAY_MEMBER_SHA256)
    if len(raw) % 32:
        raise ValueError(".day member is not divisible by 32")
    rows = {}
    for offset in range(0, len(raw), 32):
        record = raw[offset : offset + 32]
        date_raw, open_raw, high_raw, low_raw, close_raw = struct.unpack_from("<5I", record)
        iso = day_iso(date_raw)
        if not valid_iso(iso):
            raise ValueError(f"invalid .day date at record {offset // 32}: {date_raw}")
        rows[iso] = {
            "record_index": offset // 32,
            "raw_hex": record.hex(),
            "date_raw_int": date_raw,
            "open_raw_int": open_raw,
            "high_raw_int": high_raw,
            "low_raw_int": low_raw,
            "close_raw_int": close_raw,
            "open": str(dec(open_raw) / 100),
            "high": str(dec(high_raw) / 100),
            "low": str(dec(low_raw) / 100),
            "close": str(dec(close_raw) / 100),
        }
    meta.update({"record_width": 32, "record_count": len(raw) // 32})
    return rows, meta


def map_events(target: dict, day_rows: dict[str, dict]) -> list[dict]:
    records = target["target_records"][CODE]["records"]
    category_one = [record for record in records if record["category"] == 1]
    events = []
    for record in category_one:
        event_date = record["date_iso"]
        previous_dates = [d for d in day_rows if d < event_date]
        previous_date = max(previous_dates) if previous_dates else None
        mapped = {
            MAPPING[f"C{i}"]: dec(record["float32_repr_diagnostic"][i - 1])
            for i in range(1, 5)
        }
        item = {
            "record_index": record["record_index"],
            "date": event_date,
            "category": record["category"],
            "clear_hex": record["clear_hex"],
            "float32_raw_bits_hex": record["float32_raw_bits_hex"],
            "float32_repr_diagnostic": record["float32_repr_diagnostic"],
            "mapping": MAPPING,
            "mapped_decimal_inputs": {key: str(value) for key, value in mapped.items()},
            "previous_trading_date": previous_date,
        }
        if previous_date is None:
            item["status"] = "unresolved"
            item["unresolved_reason"] = "no earlier .day trading record"
            events.append(item)
            continue
        previous_close = dec(day_rows[previous_date]["close"])
        denominator = Decimal(10) + mapped["peigu"] + mapped["songzhuangu"]
        if denominator == 0:
            item["status"] = "unresolved"
            item["unresolved_reason"] = "formula denominator is zero"
            events.append(item)
            continue
        reference = (
            previous_close * 10
            - mapped["fenhong"]
            + mapped["peigu"] * mapped["peigujia"]
        ) / denominator
        rounded_reference = reference.quantize(CENT, rounding=ROUND_HALF_UP)
        item.update(
            {
                "status": "resolved",
                "previous_close": str(previous_close),
                "theoretical_reference_preclose": str(reference),
                "unrounded_ratio": str(reference / previous_close),
                "cent_rounded_reference_preclose": str(rounded_reference),
                "cent_rounded_ratio": str(rounded_reference / previous_close),
            }
        )
        events.append(item)
    return events


def factor_for(day: str, events: list[dict], key: str) -> Decimal:
    factor = Decimal(1)
    for event in events:
        if event["status"] == "resolved" and event["date"] > day:
            factor *= dec(event[key])
    return factor


def baostock_rows(probe: dict, mode: str) -> dict[str, list[str]]:
    response = probe[mode]["response"]
    positions = {field: index for index, field in enumerate(response["fields"])}
    return {row[positions["date"]]: row for row in response["rows"]}


def row_values(row: list[str], response_fields: list[str], selected_fields: tuple[str, ...]) -> dict[str, Decimal]:
    positions = {field: index for index, field in enumerate(response_fields)}
    return {field: dec(row[positions[field]]) for field in selected_fields if field in positions}


def crosscheck_2017(day_rows: dict[str, dict], events: list[dict], probe: dict) -> dict:
    none_response = probe["none"]["response"]
    qfq_response = probe["qfq"]["response"]
    none = baostock_rows(probe, "none")
    qfq = baostock_rows(probe, "qfq")
    numeric_fields = ("open", "close", "preclose")
    result = {}
    for current_date in DATES_2017:
        no_values = row_values(none[current_date], none_response["fields"], numeric_fields)
        qfq_values = row_values(qfq[current_date], qfq_response["fields"], numeric_fields)
        qfq_price_values = row_values(
            qfq[current_date], qfq_response["fields"], ("open", "high", "low", "close")
        )
        baostock_ratios = {
            field: qfq_values[field] / no_values[field]
            for field in ("open", "close", "preclose")
        }
        unrounded_factor = factor_for(current_date, events, "unrounded_ratio")
        rounded_factor = factor_for(current_date, events, "cent_rounded_ratio")
        tdx_values = {field: dec(day_rows[current_date][field]) for field in ("open", "high", "low", "close")}
        result[current_date] = {
            "baostock_no_adjust": none[current_date],
            "baostock_qfq": qfq[current_date],
            "baostock_ratios": {field: str(value) for field, value in baostock_ratios.items()},
            "unrounded_factor": str(unrounded_factor),
            "cent_rounded_factor": str(rounded_factor),
            "factor_vs_baostock_ratio": {
                "unrounded": {
                    field: compare_decimal(unrounded_factor, value)
                    for field, value in baostock_ratios.items()
                },
                "cent_rounded": {
                    field: compare_decimal(rounded_factor, value)
                    for field, value in baostock_ratios.items()
                },
            },
            "scaled_price_comparison": {
                "unrounded": {
                    field: compare_price(tdx_values[field] * unrounded_factor, qfq_price_values[field])
                    for field in ("open", "high", "low", "close")
                },
                "cent_rounded": {
                    field: compare_price(tdx_values[field] * rounded_factor, qfq_price_values[field])
                    for field in ("open", "high", "low", "close")
                },
            },
        }
    before = result["2017-05-24"]["baostock_ratios"]
    event_day = result["2017-05-25"]["baostock_ratios"]
    unrounded_event = next(event for event in events if event["date"] == "2017-05-25")
    result["quotient_2017-05-24_over_05-25"] = {
        "unrounded_local_event_ratio": unrounded_event["unrounded_ratio"],
        "cent_rounded_local_event_ratio": unrounded_event["cent_rounded_ratio"],
        "baostock_ratio_quotients": {
            field: compare_decimal(dec(before[field]) / dec(event_day[field]), Decimal(unrounded_event["unrounded_ratio"]))
            for field in before
        },
        "cent_rounded_comparisons": {
            field: compare_decimal(dec(before[field]) / dec(event_day[field]), Decimal(unrounded_event["cent_rounded_ratio"]))
            for field in before
        },
    }
    return result


def make_gate(name: str, passed: bool, required: bool, detail: object) -> dict:
    return {
        "name": name,
        "pass": bool(passed),
        "required_for_full_match": required,
        "detail": detail,
    }


def build_chain_gates(
    variant: str,
    events: list[dict],
    unresolved_count: int,
    cross_2017: dict,
    cross_recent: dict,
    current_anchor_date: str,
) -> dict:
    ratio_key = "unrounded" if variant == "unrounded" else "cent_rounded"
    price_key = "unrounded" if variant == "unrounded" else "cent_rounded"
    gates = []
    gates.append(make_gate(
        "unresolved_count_zero",
        unresolved_count == 0,
        True,
        {"unresolved_count": unresolved_count},
    ))
    latest_event = cross_recent["latest_resolved_event_date"]
    anchor_factor = factor_for(current_anchor_date, events, f"{ratio_key}_ratio")
    post_latest_factor = factor_for("2026-09-10", events, f"{ratio_key}_ratio")
    anchor_pass = latest_event < current_anchor_date and anchor_factor == Decimal(1) and post_latest_factor == Decimal(1)
    gates.append(make_gate(
        "anchor_latest_event_semantics",
        anchor_pass,
        True,
        {
            "current_anchor_date": current_anchor_date,
            "latest_event_date": latest_event,
            "factor_at_anchor": str(anchor_factor),
            "factor_at_2026-09-10": str(post_latest_factor),
            "rule": "latest event and later dates exclude the latest event ratio; factor is 1 when no later event exists",
        },
    ))

    ratio_exact = all(
        item["exact_equal"]
        for current_date in DATES_2017
        for item in cross_2017[current_date]["factor_vs_baostock_ratio"][ratio_key].values()
    )
    ratio_tolerance = all(
        item["abs_difference_le_1e-6_diagnostic"]
        for current_date in DATES_2017
        for item in cross_2017[current_date]["factor_vs_baostock_ratio"][ratio_key].values()
    )
    gates.append(make_gate(
        "2017_factor_ratio_exact",
        ratio_exact,
        True,
        {current_date: cross_2017[current_date]["factor_vs_baostock_ratio"][ratio_key] for current_date in DATES_2017},
    ))
    gates.append(make_gate(
        "2017_factor_ratio_tolerance_diagnostic",
        ratio_tolerance,
        False,
        {"absolute_tolerance": "0.000001", "reason": "diagnostic only; never substitutes for exact equality"},
    ))
    price_comparisons_2017 = {
        current_date: cross_2017[current_date]["scaled_price_comparison"][price_key]
        for current_date in DATES_2017
    }
    price_exact = all(item["exact_equal"] for values in price_comparisons_2017.values() for item in values.values())
    price_display = all(item["display_2dp_equal"] for values in price_comparisons_2017.values() for item in values.values())
    gates.append(make_gate("2017_price_exact", price_exact, True, price_comparisons_2017))
    gates.append(make_gate(
        "2017_price_display_2dp_diagnostic",
        price_display,
        False,
        {"comparisons": price_comparisons_2017, "reason": "display precision is reported separately from exact equality"},
    ))

    recent_comparisons = {
        current_date: cross_recent["rows"][current_date][f"{price_key}_price_comparison"]
        for current_date in cross_recent["selected_dates"]
    }
    recent_exact = all(item["exact_equal"] for values in recent_comparisons.values() for item in values.values())
    recent_display = all(item["display_2dp_equal"] for values in recent_comparisons.values() for item in values.values())
    gates.append(make_gate("recent_price_exact", recent_exact, True, recent_comparisons))
    gates.append(make_gate(
        "recent_price_display_2dp_diagnostic",
        recent_display,
        False,
        {"comparisons": recent_comparisons, "reason": "display precision is reported separately from exact equality"},
    ))

    quotient_key = "baostock_ratio_quotients" if variant == "unrounded" else "cent_rounded_comparisons"
    quotient = cross_2017["quotient_2017-05-24_over_05-25"][quotient_key]
    quotient_exact = all(item["exact_equal"] for item in quotient.values())
    quotient_tolerance = all(item["abs_difference_le_1e-6_diagnostic"] for item in quotient.values())
    gates.append(make_gate("2017_factor_quotient_exact", quotient_exact, True, quotient))
    gates.append(make_gate(
        "2017_factor_quotient_tolerance_diagnostic",
        quotient_tolerance,
        False,
        {"absolute_tolerance": "0.000001", "comparisons": quotient},
    ))
    required = [gate for gate in gates if gate["required_for_full_match"]]
    return {
        "variant": variant,
        "fixed_diagnostic_tolerance": "0.000001",
        "fixed_diagnostic_tolerance_reason": "requested diagnostic threshold only; exact equality remains the required full-match criterion",
        "gates": gates,
        "required_gate_names": [gate["name"] for gate in required],
        "full_chain_match": all(gate["pass"] for gate in required),
    }


def crosscheck_2025_2026(day_rows: dict[str, dict], events: list[dict], daily_probe: dict) -> dict:
    session = next(
        query for query in daily_probe["initial_session"]["queries"]
        if query["label"] == "fixed_history" and query["request"]["code"] == "sh.600000"
    )
    response = session["response"]
    positions = {field: index for index, field in enumerate(response["fields"])}
    rows = {row[positions["date"]]: row for row in response["rows"]}
    latest_event = max(event["date"] for event in events if event["status"] == "resolved")
    selected = ["2026-07-15", "2026-07-16", "2026-07-17", "2026-09-10"]
    out = {
        "source_request": session["request"],
        "latest_resolved_event_date": latest_event,
        "selected_dates": selected,
        "rows": {},
    }
    for current_date in selected:
        row = rows[current_date]
        qfq_values = {field: dec(row[positions[field]]) for field in ("open", "high", "low", "close")}
        factor_u = factor_for(current_date, events, "unrounded_ratio")
        factor_c = factor_for(current_date, events, "cent_rounded_ratio")
        raw = {field: dec(day_rows[current_date][field]) for field in ("open", "high", "low", "close")}
        out["rows"][current_date] = {
            "source_qfq": row,
            "unrounded_factor": str(factor_u),
            "cent_rounded_factor": str(factor_c),
            "is_after_latest_event": current_date > latest_event,
            "unrounded_price_comparison": {field: compare_price(raw[field] * factor_u, qfq_values[field]) for field in raw},
            "cent_rounded_price_comparison": {field: compare_price(raw[field] * factor_c, qfq_values[field]) for field in raw},
        }
    return out


def main() -> None:
    target_path, day_zip, doc_probe_path, daily_probe_path, output_path = map(Path, sys.argv[1:6])
    target = json.loads(target_path.read_text())
    doc_probe = json.loads(doc_probe_path.read_text())
    daily_probe = json.loads(daily_probe_path.read_text())
    day, day_meta = read_day_rows(day_zip)
    events = map_events(target, day)
    unresolved = [event for event in events if event["status"] != "resolved"]
    resolved = [event for event in events if event["status"] == "resolved"]
    for event in resolved:
        event["cumulative_factor_on_event_date_unrounded"] = str(
            factor_for(event["date"], events, "unrounded_ratio")
        )
        event["cumulative_factor_on_event_date_cent_rounded"] = str(
            factor_for(event["date"], events, "cent_rounded_ratio")
        )
    event_dates = [event["date"] for event in events]
    latest_event = max(event_dates)
    cross_2017 = crosscheck_2017(day, events, doc_probe)
    cross_recent = crosscheck_2025_2026(day, events, daily_probe)
    selected_dates = set(DATES_2017 + cross_recent["selected_dates"] + ["2026-09-11"])
    factors = {
        current_date: {
            "unrounded": str(factor_for(current_date, events, "unrounded_ratio")),
            "cent_rounded_diagnostic": str(factor_for(current_date, events, "cent_rounded_ratio")),
        }
        for current_date in sorted(selected_dates)
    }
    gates = {
        "unrounded": build_chain_gates(
            "unrounded", events, len(unresolved), cross_2017, cross_recent, "2026-09-11"
        ),
        "cent_rounded": build_chain_gates(
            "cent_rounded", events, len(unresolved), cross_2017, cross_recent, "2026-09-11"
        ),
    }
    unrounded_match = gates["unrounded"]["full_chain_match"]
    cent_match = gates["cent_rounded"]["full_chain_match"]
    unrounded_anchor_gate = next(
        gate for gate in gates["unrounded"]["gates"] if gate["name"] == "anchor_latest_event_semantics"
    )
    current_proven = unrounded_match and unrounded_anchor_gate["pass"] and len(unresolved) == 0
    output = {
        "schema": "x-data-mcp.issue-3.tdx-gbbq-full-chain.v1",
        "scope": {
            "code": CODE,
            "current_anchor_date": "2026-09-11",
            "factor_rule": "factor(date)=product of event ratios for category-1 event dates strictly later than date; event date excludes its own ratio",
            "price_arithmetic": "raw .day integer / 100 and Decimal; no binary-float price arithmetic",
            "cent_rounded_variant": "diagnostic only: ROUND_HALF_UP reference preclose to 0.01 before dividing by previous close; not an official contract",
        },
        "inputs": {
            "gbbq_target_summary": str(target_path),
            "day_zip": str(day_zip),
            "day_member": day_meta,
            "doc_probe": str(doc_probe_path),
            "daily_probe": str(daily_probe_path),
            "mapping": MAPPING,
        },
        "daily_source": {
            "first_date": min(day),
            "last_date": max(day),
            "record_count": len(day),
            "member": DAY_MEMBER,
            "member_sha256": EXPECTED_DAY_MEMBER_SHA256,
        },
        "events": {
            "category": 1,
            "count": len(events),
            "resolved_count": len(resolved),
            "unresolved_count": len(unresolved),
            "first_date": min(event_dates),
            "latest_date": latest_event,
            "records": events,
        },
        "cumulative_factors": factors,
        "cross_checks": {
            "2017_baostock_current_qfq": cross_2017,
            "2025_2026_baostock_current_qfq": cross_recent,
        },
        "match_gates": gates,
        "conclusions": {
            "unrounded_full_chain_current_qfq": "UNROUNDED_FULL_CHAIN_CURRENT_QFQ_MATCH" if unrounded_match else "UNROUNDED_FULL_CHAIN_CURRENT_QFQ_MISMATCH",
            "cent_rounded_diagnostic_full_chain": "CENT_ROUNDED_DIAGNOSTIC_FULL_CHAIN_MATCH" if cent_match else "CENT_ROUNDED_DIAGNOSTIC_FULL_CHAIN_MISMATCH",
            "current_anchored_qfq_reconstruction": "CURRENT_ANCHORED_QFQ_RECONSTRUCTION_PROVEN" if current_proven else "CURRENT_ANCHORED_QFQ_RECONSTRUCTION_NOT_PROVEN",
            "rounding_rule": "NOT_OFFICIAL_TDX_CONTRACT",
        },
    }
    output_path.write_text(json.dumps(output, ensure_ascii=False, indent=2, sort_keys=True) + "\n")
    print(json.dumps({
        "category_1_count": len(events),
        "latest_event_date": latest_event,
        "unresolved_count": len(unresolved),
        "unrounded_2017_05_24_factor": factors["2017-05-24"]["unrounded"],
        "cent_rounded_2017_05_24_factor": factors["2017-05-24"]["cent_rounded_diagnostic"],
        "conclusions": output["conclusions"],
        "unrounded_full_chain_match": unrounded_match,
        "cent_rounded_full_chain_match": cent_match,
    }, ensure_ascii=False, sort_keys=True))


if __name__ == "__main__":
    main()
