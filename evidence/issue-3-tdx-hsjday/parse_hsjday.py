#!/usr/bin/env python3
"""Offline manifest and two-sample parser for an already downloaded hsjday.zip.

No network access is performed. The parser reads the ZIP central directory,
computes directory statistics, and reads only the two target members. Amount
bytes stay raw; no float decoding is performed.
"""

from __future__ import annotations

import hashlib
import json
import struct
import sys
import zipfile
from collections import Counter, defaultdict
from datetime import date
from pathlib import Path


TARGET_BASENAMES = {"sh600000.day", "sz000001.day"}


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def decimal_cents(value: int) -> str:
    sign = "-" if value < 0 else ""
    value = abs(value)
    return f"{sign}{value // 100}.{value % 100:02d}"


def parse_date(value: int) -> str | None:
    year, month, day = value // 10000, value // 100 % 100, value % 100
    try:
        date(year, month, day)
    except ValueError:
        return None
    return f"{year:04d}-{month:02d}-{day:02d}"


def parse_record(raw: bytes, index: int) -> dict:
    if len(raw) != 32:
        raise ValueError(f"record {index} is not 32 bytes")
    words = struct.unpack_from("<5I", raw, 0)
    volume = struct.unpack_from("<I", raw, 24)[0]
    reserved = struct.unpack_from("<I", raw, 28)[0]
    return {
        "record_index": index,
        "raw_hex": raw.hex(),
        "date_raw_int": words[0],
        "date_iso": parse_date(words[0]),
        "open_raw_int": words[1],
        "high_raw_int": words[2],
        "low_raw_int": words[3],
        "close_raw_int": words[4],
        "price_scale_rule": "integer / 100; rendered without float",
        "open_decimal": decimal_cents(words[1]),
        "high_decimal": decimal_cents(words[2]),
        "low_decimal": decimal_cents(words[3]),
        "close_decimal": decimal_cents(words[4]),
        "amount_raw_u32": struct.unpack_from("<I", raw, 20)[0],
        "amount_raw_hex": raw[20:24].hex(),
        "amount_decoding": "not decoded; field type/unit not confirmed by official page/package",
        "volume_raw_u32": volume,
        "volume_unit": "unknown",
        "reserved_raw_u32": reserved,
    }


def main() -> None:
    archive = Path(sys.argv[1])
    output = Path(sys.argv[2])
    with zipfile.ZipFile(archive) as z:
        members = []
        by_top = defaultdict(lambda: {"member_count": 0, "compressed_bytes": 0, "uncompressed_bytes": 0})
        extensions = Counter()
        targets = {}
        for info in z.infolist():
            path = info.filename.replace("\\", "/")
            members.append(
                {
                    "path": info.filename,
                    "compressed_size": info.compress_size,
                    "uncompressed_size": info.file_size,
                    "crc32": f"{info.CRC:08x}",
                    "timestamp": list(info.date_time),
                    "compression": info.compress_type,
                    "is_dir": info.is_dir(),
                }
            )
            top = path.split("/", 1)[0]
            by_top[top]["member_count"] += 1
            by_top[top]["compressed_bytes"] += info.compress_size
            by_top[top]["uncompressed_bytes"] += info.file_size
            extensions[Path(path).suffix.lower() or "<no-extension>"] += 1
            if Path(path).name.lower() in TARGET_BASENAMES:
                targets[Path(path).name.lower()] = info.filename

        output.mkdir(parents=True, exist_ok=True)
        (output / "members.json").write_text(
            json.dumps(
                {
                    "member_count": len(members),
                    "testzip_bad_member": z.testzip(),
                    "members": members,
                },
                ensure_ascii=False,
                indent=2,
            )
            + "\n"
        )
        summary = {
            "member_count": len(members),
            "extensions": dict(sorted(extensions.items())),
            "by_top_level": dict(sorted(by_top.items())),
            "targets": {},
        }
        for basename, path in sorted(targets.items()):
            raw = z.read(path)
            if len(raw) % 32:
                raise ValueError(f"{path}: byte length is not divisible by 32")
            count = len(raw) // 32
            first = parse_record(raw[:32], 0)
            last = parse_record(raw[-32:], count - 1)
            summary["targets"][basename] = {
                "path": path,
                "compressed_size": z.getinfo(path).compress_size,
                "uncompressed_size": len(raw),
                "file_sha256": sha256_bytes(raw),
                "record_count": count,
                "first_date": first["date_iso"],
                "last_date": last["date_iso"],
                "first_last_records": [first, last],
            }
        (output / "summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2) + "\n")
        print(json.dumps(summary, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
