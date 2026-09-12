#!/usr/bin/env python3
"""Select cross-market samples before any BaoStock result is inspected."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import sys
import zipfile
from datetime import date
from pathlib import Path


WINDOW_START = "2025-01-01"
WINDOW_END = "2026-09-10"
GBBQ_MEMBER = "gbbq"
EVENT_CATEGORY = 1
A_SHARE_PREFIXES = {
    "sh": ("600", "601", "603", "605", "688", "689"),
    "sz": ("000", "001", "002", "003", "300", "301"),
}


def load_decoder(path: Path):
    spec = importlib.util.spec_from_file_location("merged_gbbq_decoder", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load decoder: {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def iso(raw: int) -> str:
    return f"{raw // 10000:04d}-{raw // 100 % 100:02d}-{raw % 100:02d}"


def valid_iso(value: str) -> bool:
    try:
        date.fromisoformat(value)
        return True
    except ValueError:
        return False


def day_meta(raw: bytes) -> dict:
    if len(raw) % 32:
        raise ValueError(".day length is not divisible by 32")
    dates = []
    for offset in range(0, len(raw), 32):
        date_raw = int.from_bytes(raw[offset : offset + 4], "little")
        current = iso(date_raw)
        if not valid_iso(current):
            raise ValueError(f"invalid .day date {date_raw}")
        dates.append(current)
    window_dates = [value for value in dates if WINDOW_START <= value <= WINDOW_END]
    return {
        "bytes": len(raw),
        "sha256": hashlib.sha256(raw).hexdigest(),
        "record_count": len(dates),
        "first_date": min(dates) if dates else None,
        "last_date": max(dates) if dates else None,
        "window_row_count": len(window_dates),
        "window_first_date": min(window_dates) if window_dates else None,
        "window_last_date": max(window_dates) if window_dates else None,
    }


def is_nonzero(value: object) -> bool:
    return float(value) != 0.0


def main() -> None:
    gbbq_zip_path, source_path, day_zip_path, selection_path, events_path = map(Path, sys.argv[1:6])
    decoder_path = Path(__file__).resolve().parents[1] / "issue-3-tdx-gbbq-community" / "decode_gbbq.py"
    decoder = load_decoder(decoder_path)
    source = source_path.read_bytes()
    key = decoder.key_from_source(source)
    with zipfile.ZipFile(gbbq_zip_path) as archive:
        gbbq = archive.read(GBBQ_MEMBER)
    count = int.from_bytes(gbbq[:4], "little")
    if len(gbbq) != 4 + count * decoder.RECORD_SIZE:
        raise ValueError("GBBQ structural length mismatch")

    events_by_code: dict[str, list[dict]] = {}
    category1_total = 0
    for index in range(count):
        encrypted = gbbq[4 + index * decoder.RECORD_SIZE : 4 + (index + 1) * decoder.RECORD_SIZE]
        record = decoder.parse_clear_record(decoder.decode_encrypted_record(encrypted, key), index)
        if record["category"] == EVENT_CATEGORY and record["code"].isdigit() and len(record["code"]) == 6:
            category1_total += 1
            events_by_code.setdefault(record["code"], []).append(record)

    candidates: dict[str, dict] = {}
    with zipfile.ZipFile(day_zip_path) as archive:
        for name in archive.namelist():
            normalized = name.replace("/", "\\")
            parts = normalized.split("\\")
            if len(parts) != 3 or parts[1].lower() != "lday" or not parts[2].lower().endswith(".day"):
                continue
            market = parts[0].lower()
            code = parts[2][2:-4] if len(parts[2]) == 12 else ""
            if market not in {"sh", "sz"} or len(code) != 6 or not code.isdigit() or code in {"000001", "600000"} or not code.startswith(A_SHARE_PREFIXES[market]) or code not in events_by_code:
                continue
            meta = day_meta(archive.read(name))
            if meta["window_row_count"] == 0:
                continue
            candidates.setdefault(market, {})[code] = {
                "market": market,
                "code": code,
                "member": name,
                "event_count_category_1": len(events_by_code[code]),
                "events": events_by_code[code],
                "day": meta,
            }

    def pick(market: str, field: str, selected: set[str]) -> tuple[str | None, str]:
        choices = []
        for code, candidate in sorted(candidates.get(market, {}).items()):
            if code in selected:
                continue
            if any(is_nonzero(event["float32_repr_diagnostic"][{"fenhong": 0, "peigujia": 1, "songzhuangu": 2, "peigu": 3}[field]]) for event in candidate["events"]):
                choices.append(code)
        if choices:
            return choices[0], f"smallest {market} candidate with category=1 history and {field} != 0"
        return None, f"no unselected {market} candidate with category=1 history and {field} != 0"

    selected_codes: set[str] = set()
    selected: list[dict] = []
    rationale: list[dict] = []
    for market in ("sh", "sz"):
        code, reason = pick(market, "fenhong", selected_codes)
        if code is None:
            code, reason = pick(market, "peigu", selected_codes)
        if code is None:
            code = min(candidates[market])
            reason = f"fallback smallest {market} category=1 candidate"
        selected_codes.add(code)
        selected.append(candidates[market][code])
        rationale.append({"market": market, "stage": "fenhong_or_fallback", "code": code, "reason": reason})

    for market in ("sh", "sz"):
        code, reason = pick(market, "songzhuangu", selected_codes)
        if code is None:
            code, reason = pick(market, "peigu", selected_codes)
        if code is None:
            remaining = sorted(set(candidates.get(market, {})) - selected_codes)
            if not remaining:
                raise ValueError(f"could not select second {market} security")
            code = remaining[0]
            reason = f"fallback smallest unselected {market} category=1 candidate"
        selected_codes.add(code)
        selected.append(candidates[market][code])
        rationale.append({"market": market, "stage": "songzhuangu_or_fallback", "code": code, "reason": reason})

    selected.sort(key=lambda item: (item["market"], item["code"]))
    selection = {
        "schema": "x-data-mcp.issue-3.cross-stock.selection.v1",
        "selection_before_baostock": True,
        "window": {"start": WINDOW_START, "end": WINDOW_END},
        "candidate_definition": "six-digit numeric security member under official hsjday sh\\lday or sz\\lday, with category=1 GBBQ history, an allowed A-share code prefix, and at least one .day bar in the fixed window; this is a bounded A-share code-range proxy, not a complete listing-status contract",
        "filters": [
            "retained official GBBQ category=1 history",
            "official hsjday sh/sz lday member exists",
            "SH prefixes 600/601/603/605/688/689; SZ prefixes 000/001/002/003/300/301",
            "at least one .day bar in 2025-01-01..2026-09-10",
            "exclude 600000 and 000001",
        ],
        "full_current_listing_status": "NOT_VERIFIED",
        "category1_decoded_total": category1_total,
        "candidate_counts": {market: len(candidates.get(market, {})) for market in ("sh", "sz")},
        "rationale": rationale,
        "selected": [
            {
                "market": item["market"],
                "code": item["code"],
                "member": item["member"],
                "event_count_category_1": item["event_count_category_1"],
                "day": item["day"],
            }
            for item in selected
        ],
        "selection_rule": "per market: smallest candidate with fenhong != 0; then smallest unselected candidate with songzhuangu != 0; otherwise peigu != 0; otherwise smallest unselected candidate",
        "official_artifact_boundary": "selection uses only retained local official artifacts; the A-share prefixes are a bounded code-range proxy, not proof of current listing status; event/category semantics remain community-decoder evidence and NOT_VERIFIED",
    }
    event_output = {
        "schema": "x-data-mcp.issue-3.cross-stock.selected-events.v1",
        "source": "selection.json selected codes; only category=1 records are retained",
        "mapping": {"C1": "fenhong", "C2": "peigujia", "C3": "songzhuangu", "C4": "peigu"},
        "selected_events": {
            f"{item['market']}.{item['code']}": item["events"] for item in selected
        },
    }
    selection_path.write_text(json.dumps(selection, ensure_ascii=False, indent=2, sort_keys=True) + "\n")
    events_path.write_text(json.dumps(event_output, ensure_ascii=False, indent=2, sort_keys=True) + "\n")
    print(json.dumps({"candidate_counts": selection["candidate_counts"], "selected": [f"{x['market']}.{x['code']}" for x in selected], "category1_decoded_total": category1_total}, sort_keys=True))


if __name__ == "__main__":
    main()
