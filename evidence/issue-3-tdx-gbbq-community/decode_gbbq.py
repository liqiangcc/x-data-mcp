#!/usr/bin/env python3
"""Stdlib-only offline reproduction of the fixed pytdx GBBQ decoder.

Usage:
    decode_gbbq.py /path/to/gbbq /path/to/fixed/gbbq_reader.py output.json

The fixed third-party source is supplied separately and is checked by both
Git blob SHA-1 and raw SHA-256. The key table is extracted at runtime from
that local source; it is deliberately not vendored here. No pandas or
network access is used.
"""

from __future__ import annotations

import hashlib
import json
import re
import struct
import sys
from datetime import date
from pathlib import Path


SOURCE_COMMIT = "0d6f80618e1bae0f8e48552e597af492f3640f5d"
SOURCE_PATH = "pytdx/reader/gbbq_reader.py"
SOURCE_GIT_BLOB_SHA1 = "0eda4935027ffc79b5c425eff0a70a6d492b21d1"
SOURCE_RAW_SHA256 = "e28fae76760b1247a960cab285aa357c2d263b3212e7cabfb3f1ebfcc127d6ba"
TARGET_CODES = {"000001", "600000"}
RECORD_SIZE = 29


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def git_blob_sha1(data: bytes) -> str:
    header = f"blob {len(data)}\0".encode("ascii")
    return hashlib.sha1(header + data).hexdigest()


def u32(data: bytes, offset: int) -> int:
    return struct.unpack_from("<I", data, offset)[0]


def c_uint32(value: int) -> int:
    return value & 0xFFFFFFFF


def key_from_source(source: bytes) -> bytes:
    text = source.decode("utf-8")
    match = re.search(r'hexdump_keys\s*=\s*"([^"]+)"', text)
    if match is None:
        raise ValueError("fixed source hexdump_keys assignment not found")
    key = bytes.fromhex(match.group(1))
    if len(key) < 0xC48 + 256 * 4:
        raise ValueError("fixed source key table is shorter than decoder indexes")
    return key


def decode_encrypted_record(encrypted: bytes, key: bytes) -> bytes:
    if len(encrypted) != RECORD_SIZE:
        raise ValueError("encrypted record is not 29 bytes")
    clear = bytearray()
    data_offset = 0
    for _ in range(3):
        eax = u32(key, 0x44)
        ebx = u32(encrypted, data_offset)
        num = c_uint32(eax ^ ebx)
        numold = u32(encrypted, data_offset + 4)
        for j in reversed(range(4, 0x40 + 4, 4)):
            ebx = (num & 0xFF0000) >> 16
            eax = u32(key, ebx * 4 + 0x448)
            ebx = num >> 24
            eax = c_uint32(eax + u32(key, ebx * 4 + 0x48))
            ebx = (num & 0xFF00) >> 8
            eax = c_uint32(eax ^ u32(key, ebx * 4 + 0x848))
            ebx = num & 0xFF
            eax = c_uint32(eax + u32(key, ebx * 4 + 0xC48))
            eax = c_uint32(eax ^ u32(key, j))
            ebx = num
            num = c_uint32(numold ^ eax)
            numold = ebx
        numold = c_uint32(numold ^ u32(key, 0))
        clear.extend(struct.pack("<II", numold, num))
        data_offset += 8
    clear.extend(encrypted[data_offset : data_offset + 5])
    if len(clear) != RECORD_SIZE:
        raise ValueError("decoder did not produce 29 clear bytes")
    return bytes(clear)


def parse_clear_record(clear: bytes, index: int) -> dict:
    market = clear[0]
    code_bytes = clear[1:8]
    date_raw = u32(clear, 8)
    category = clear[12]
    float_bits = [u32(clear, offset) for offset in (13, 17, 21, 25)]
    code = code_bytes.rstrip(b"\0").decode("utf-8")
    year, month, day = date_raw // 10000, date_raw // 100 % 100, date_raw % 100
    date_iso = None
    calendar_valid = False
    try:
        date(year, month, day)
        date_iso = f"{year:04d}-{month:02d}-{day:02d}"
        calendar_valid = True
    except ValueError:
        pass
    date_plausible = calendar_valid and 1900 <= year <= 2100
    return {
        "record_index": index,
        "clear_hex": clear.hex(),
        "market": market,
        "code": code,
        "date_raw_int": date_raw,
        "date_iso": date_iso,
        "date_calendar_valid": calendar_valid,
        "date_plausible_1900_2100": date_plausible,
        "category": category,
        "float32_raw_bits_hex": [f"0x{value:08x}" for value in float_bits],
        "float32_repr_diagnostic": [
            struct.unpack("<f", struct.pack("<I", value))[0] for value in float_bits
        ],
    }


def main() -> None:
    raw_path, source_path, output_path = map(Path, sys.argv[1:4])
    raw = raw_path.read_bytes()
    source = source_path.read_bytes()
    actual_source_sha256 = sha256(source)
    actual_source_blob_sha1 = git_blob_sha1(source)
    if actual_source_sha256 != SOURCE_RAW_SHA256 or actual_source_blob_sha1 != SOURCE_GIT_BLOB_SHA1:
        raise SystemExit("fixed pytdx source hash mismatch")
    key = key_from_source(source)

    if len(raw) < 4:
        raise SystemExit("gbbq file is shorter than count header")
    count = u32(raw, 0)
    expected_bytes = 4 + count * RECORD_SIZE
    if len(raw) != expected_bytes:
        raise SystemExit(f"length mismatch: {len(raw)} != {expected_bytes}")

    decoded_count = 0
    invalid_code_count = 0
    calendar_invalid_date_count = 0
    date_sentinel_or_out_of_range_count = 0
    all_dates = []
    categories = set()
    targets = {code: [] for code in sorted(TARGET_CODES)}

    for index in range(count):
        encrypted = raw[4 + index * RECORD_SIZE : 4 + (index + 1) * RECORD_SIZE]
        clear = decode_encrypted_record(encrypted, key)
        record = parse_clear_record(clear, index)
        decoded_count += 1
        categories.add(record["category"])
        if len(record["code"]) != 6 or not record["code"].isdigit():
            invalid_code_count += 1
        if not record["date_calendar_valid"]:
            calendar_invalid_date_count += 1
        elif not record["date_plausible_1900_2100"]:
            date_sentinel_or_out_of_range_count += 1
        else:
            all_dates.append(record["date_iso"])
        if record["code"] in targets:
            targets[record["code"]].append(record)

    target_summary = {}
    for code, records in targets.items():
        dates = [record["date_iso"] for record in records if record["date_plausible_1900_2100"]]
        target_summary[code] = {
            "event_count": len(records),
            "date_min": min(dates) if dates else None,
            "date_max": max(dates) if dates else None,
            "category_values": sorted({record["category"] for record in records}),
            "exact_2017_05_25_matches": [
                record for record in records if record["date_iso"] == "2017-05-25"
            ],
            "records": records,
        }

    result = {
        "schema": "x-data-mcp.issue-3.tdx-gbbq-community.v1",
        "source_parser": {
            "repo": "wxyzZ/pytdx",
            "commit": SOURCE_COMMIT,
            "path": SOURCE_PATH,
            "git_blob_sha1": SOURCE_GIT_BLOB_SHA1,
            "raw_sha256": actual_source_sha256,
            "algorithm_boundary": "community reverse-engineering; source comments cite two blogs; not official TDX contract",
            "pandas_used": False,
        },
        "input": {
            "file": raw_path.name,
            "file_bytes": len(raw),
            "sha256": sha256(raw),
            "count_u32_le": count,
            "record_width": RECORD_SIZE,
            "expected_bytes": expected_bytes,
            "length_formula": "4 + count * 29",
            "length_matches": len(raw) == expected_bytes,
        },
        "decoded_count": decoded_count,
        "invalid_code_count": invalid_code_count,
        "calendar_invalid_date_count": calendar_invalid_date_count,
        "date_sentinel_or_out_of_range_count": date_sentinel_or_out_of_range_count,
        "global_date_min": min(all_dates) if all_dates else None,
        "global_date_max": max(all_dates) if all_dates else None,
        "category_values_all_decoded": sorted(categories),
        "target_records": target_summary,
        "semantic_boundary": {
            "category_meanings": "NOT_VERIFIED; numeric categories are not labeled as dividend/rights/stock-split events",
            "qfq_formula_baseline_sufficiency": "NOT_VERIFIED",
        },
    }
    output_path.write_text(json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True) + "\n")
    print(json.dumps({
        "count": count,
        "file_bytes": len(raw),
        "decoded_count": decoded_count,
        "invalid_code_count": invalid_code_count,
        "calendar_invalid_date_count": calendar_invalid_date_count,
        "date_sentinel_or_out_of_range_count": date_sentinel_or_out_of_range_count,
        "targets": {
            code: {
                "event_count": summary["event_count"],
                "date_min": summary["date_min"],
                "date_max": summary["date_max"],
                "categories": summary["category_values"],
                "2017-05-25_matches": len(summary["exact_2017_05_25_matches"]),
            }
            for code, summary in target_summary.items()
        },
    }, ensure_ascii=False, sort_keys=True))


if __name__ == "__main__":
    main()
