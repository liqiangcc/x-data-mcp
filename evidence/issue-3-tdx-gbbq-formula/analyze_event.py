#!/usr/bin/env python3
"""Offline Decimal analysis of the 600000/2017-05-25 category-1 event.

Inputs are the already committed PR #49 target summary, the already
downloaded official hsjday.zip, and the already committed BaoStock evidence.
No network or third-party package is used.
"""

from __future__ import annotations

import hashlib
import json
import struct
import sys
import zipfile
from decimal import Decimal, ROUND_HALF_UP, getcontext
from pathlib import Path

getcontext().prec = 80
EVENT_DATE = "2017-05-25"
DATES = ["2017-05-24", "2017-05-25", "2017-05-26"]
DAY_PATH = "sh\\lday\\sh600000.day"


def d(value: object) -> Decimal:
    return Decimal(str(value))


def d2(value: Decimal) -> str:
    return str(value.quantize(Decimal("0.01"), rounding=ROUND_HALF_UP))


def day_rows(zip_path: Path) -> tuple[dict, ...]:
    with zipfile.ZipFile(zip_path) as archive:
        raw = archive.read(DAY_PATH)
    rows = []
    for offset in range(0, len(raw), 32):
        record = raw[offset : offset + 32]
        words = struct.unpack_from("<5I", record, 0)
        date_raw = words[0]
        iso = f"{date_raw // 10000:04d}-{date_raw // 100 % 100:02d}-{date_raw % 100:02d}"
        if iso in DATES:
            rows.append(
                {
                    "record_index": offset // 32,
                    "raw_hex": record.hex(),
                    "date_raw_int": date_raw,
                    "date": iso,
                    "open_raw_int": words[1],
                    "high_raw_int": words[2],
                    "low_raw_int": words[3],
                    "close_raw_int": words[4],
                    "open": str(d(words[1]) / 100),
                    "high": str(d(words[2]) / 100),
                    "low": str(d(words[3]) / 100),
                    "close": str(d(words[4]) / 100),
                    "amount_raw_hex": record[20:24].hex(),
                    "volume_raw_u32": struct.unpack_from("<I", record, 24)[0],
                }
            )
    if [row["date"] for row in rows] != DATES:
        raise ValueError("target daily dates were not found in order")
    return tuple(rows)


def rows_by_date(response: dict) -> dict[str, list[str]]:
    fields = {name: index for index, name in enumerate(response["fields"])}
    return {
        row[fields["date"]]: row
        for row in response["rows"]
        if row[fields["date"]] in DATES
    }


def main() -> None:
    target_path, day_zip, probe_path, derived_path, output_path = map(Path, sys.argv[1:6])
    target = json.loads(target_path.read_text())
    probe = json.loads(probe_path.read_text())
    derived = json.loads(derived_path.read_text())
    rows = day_rows(day_zip)
    day = {row["date"]: row for row in rows}

    event = target["target_records"]["600000"]["exact_2017_05_25_matches"]
    if len(event) != 1:
        raise ValueError("expected exactly one 600000/2017-05-25 event")
    event = event[0]
    bits = event["float32_raw_bits_hex"]
    diagnostic = event["float32_repr_diagnostic"]
    mapping = {
        "C1": "fenhong",
        "C2": "peigujia",
        "C3": "songzhuangu",
        "C4": "peigu",
    }
    event_values = {mapping[f"C{i}"]: d(diagnostic[i - 1]) for i in range(1, 5)}
    previous_close = d(day["2017-05-24"]["close"])
    reference_preclose = (
        previous_close * 10
        - event_values["fenhong"]
        + event_values["peigu"] * event_values["peigujia"]
    ) / (10 + event_values["peigu"] + event_values["songzhuangu"])
    event_ratio = reference_preclose / previous_close

    no_adjust = rows_by_date(probe["none"]["response"])
    qfq = rows_by_date(probe["qfq"]["response"])
    pdf = {
        "2017-05-24": {"open": "11.681648", "close": "11.750007", "preclose": "11.719625"},
        "2017-05-25": {"open": "11.75", "close": "12.93", "preclose": "11.75"},
        "2017-05-26": {"open": "12.81", "close": "12.84", "preclose": "12.93"},
    }
    bounded = {}
    for current_date in DATES:
        factor = event_ratio if current_date < EVENT_DATE else Decimal(1)
        raw = day[current_date]
        preclose_input = d(no_adjust[current_date][6])
        bounded[current_date] = {
            "adjustment_factor": str(factor),
            "open": str(d(raw["open"]) * factor),
            "high": str(d(raw["high"]) * factor),
            "low": str(d(raw["low"]) * factor),
            "close": str(d(raw["close"]) * factor),
            "preclose": str(preclose_input * factor),
            "display_2dp": {
                field: d2(d(value))
                for field, value in {
                    "open": str(d(raw["open"]) * factor),
                    "high": str(d(raw["high"]) * factor),
                    "low": str(d(raw["low"]) * factor),
                    "close": str(d(raw["close"]) * factor),
                    "preclose": str(preclose_input * factor),
                }.items()
            },
        }

    comparison = {}
    for current_date in DATES:
        comparison[current_date] = {}
        for field in ("open", "close", "preclose"):
            actual = d(bounded[current_date][field])
            expected = d(pdf[current_date][field])
            comparison[current_date][field] = {
                "bounded_decimal": str(actual),
                "pdf_decimal": str(expected),
                "exact_decimal_equal": actual == expected,
                "bounded_display_2dp": d2(actual),
                "pdf_display_2dp": d2(expected),
                "display_2dp_equal": d2(actual) == d2(expected),
            }

    current_ratios = {
        item["date"]: item["expected_ratio"]
        for item in derived["ratios"]
        if item["field"] == "open"
    }
    output = {
        "schema": "x-data-mcp.issue-3.tdx-gbbq-formula.v1",
        "scope": {
            "code": "600000",
            "event_date": EVENT_DATE,
            "daily_dates": DATES,
            "anchor": "2017-05-26 temporary local anchor only",
        },
        "event": {
            "record_index": event["record_index"],
            "category": event["category"],
            "clear_hex": event["clear_hex"],
            "float32_raw_bits_hex": bits,
            "float32_repr_diagnostic": event["float32_repr_diagnostic"],
            "category_1_mapping": mapping,
            "mapped_decimal_inputs": {key: str(value) for key, value in event_values.items()},
        },
        "daily_source": {
            "zip_entry": DAY_PATH,
            "member_sha256": "bc6815ceafa9cdcd24c68df4d8ec43accfc3e724f5d34d2c7d827a5b12ccac8f",
            "record_width": 32,
            "rows": list(rows),
        },
        "formula": {
            "source_formula": "preclose=(prev_close*10-fenhong+peigu*peigujia)/(10+peigu+songzhuangu)",
            "reference_preclose": str(reference_preclose),
            "previous_close": str(previous_close),
            "single_event_adjustment_ratio": str(event_ratio),
            "bounded_local_adjustment": bounded,
        },
        "existing_baostock_evidence": {
            "no_adjust_rows": no_adjust,
            "qfq_rows": qfq,
            "pdf_values": pdf,
            "current_qfq_no_adjust_ratios": current_ratios,
            "old_pdf_factor_2016_06_23": "0.759535",
            "old_factor_difference_from_local_event_ratio": str(event_ratio - d("0.759535")),
        },
        "comparisons": comparison,
        "layered_conclusions": {
            "field_mapping": "FIELD_MAPPING_COMMUNITY_CONSISTENT",
            "event_formula": "EVENT_FORMULA_NUMERICALLY_COMPATIBLE",
            "event_formula_boundary": "Only compatible at displayed two-decimal price precision; exact local decimals and old PDF factor are not equal.",
            "current_anchor": "CURRENT_ANCHORED_QFQ_NOT_PROVEN",
            "qfq_formula_baseline_sufficiency": "NOT_VERIFIED",
            "category_semantics": "NOT_VERIFIED",
        },
    }
    output_path.write_text(json.dumps(output, ensure_ascii=False, indent=2, sort_keys=True) + "\n")
    print(json.dumps({
        "reference_preclose": str(reference_preclose),
        "single_event_ratio": str(event_ratio),
        "current_ratios": current_ratios,
        "all_pdf_comparisons_display_2dp": all(
            item["display_2dp_equal"]
            for values in comparison.values()
            for item in values.values()
        ),
        "exact_pdf_comparisons": [
            (current_date, field)
            for current_date, values in comparison.items()
            for field, item in values.items()
            if item["exact_decimal_equal"]
        ],
    }, ensure_ascii=False, sort_keys=True))


if __name__ == "__main__":
    main()
