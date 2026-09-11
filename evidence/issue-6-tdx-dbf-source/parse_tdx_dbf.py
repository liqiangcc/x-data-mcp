#!/usr/bin/env python3
"""Offline parser for the downloaded TDX base/GBBQ evidence.

Usage: parse_tdx_dbf.py BASE_ZIP GBBQ_ZIP EXTRACTED_BASE EXTRACTED_GBBQ OUTPUT_DIR

Only Python's standard library is used. It preserves DBF field bytes and
emits diagnostic text separately from raw hex; it intentionally does not
interpret the proprietary gbbq binary.
"""

from __future__ import annotations

import hashlib
import json
import struct
import sys
import zipfile
from pathlib import Path


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def member_manifest(path: Path) -> dict:
    with zipfile.ZipFile(path) as archive:
        bad = archive.testzip()
        members = []
        for item in archive.infolist():
            members.append(
                {
                    "path": item.filename,
                    "compressed_size": item.compress_size,
                    "uncompressed_size": item.file_size,
                    "crc32": f"{item.CRC:08x}",
                    "timestamp": list(item.date_time),
                    "compression": item.compress_type,
                }
            )
        return {
            "file": path.name,
            "sha256": sha256(path),
            "testzip_bad_member": bad,
            "member_count": len(members),
            "members": members,
        }


def dbf_schema(path: Path) -> dict:
    raw = path.read_bytes()
    year, month, day = raw[1:4]
    count = struct.unpack_from("<I", raw, 4)[0]
    header_length, record_length = struct.unpack_from("<HH", raw, 8)
    fields = []
    offset = 1
    cursor = 32
    while cursor < header_length and raw[cursor] != 0x0D:
        name = raw[cursor : cursor + 11].split(b"\0", 1)[0].decode("ascii", "replace")
        fields.append(
            {
                "name": name,
                "type": chr(raw[cursor + 11]),
                "length": raw[cursor + 16],
                "decimal": raw[cursor + 17],
                "offset_in_record": offset,
            }
        )
        offset += raw[cursor + 16]
        cursor += 32

    samples = []
    deleted = 0
    for index in range(count):
        begin = header_length + index * record_length
        record = raw[begin : begin + record_length]
        if len(record) != record_length:
            break
        if record[0] == 0x2A:
            deleted += 1
        if index < 3:
            samples.append(record_sample(index, record, fields))

    targets = {}
    for index in range(count):
        begin = header_length + index * record_length
        record = raw[begin : begin + record_length]
        code_field = next(field for field in fields if field["name"] == "GPDM")
        code_begin = code_field["offset_in_record"]
        code = record[code_begin : code_begin + code_field["length"]].decode("ascii", "replace").strip()
        if code in {"000001", "600000"}:
            targets.setdefault(code, []).append(record_sample(index, record, fields))

    return {
        "file": path.name,
        "file_bytes": len(raw),
        "sha256": sha256(path),
        "dbf_version_byte": f"0x{raw[0]:02x}",
        "last_update_header_date": f"{1900 + year:04d}-{month:02d}-{day:02d}",
        "record_count_header": count,
        "header_length": header_length,
        "record_length": record_length,
        "code_page_byte": f"0x{raw[29]:02x}",
        "code_page_interpretation": "UNKNOWN; any local decoding is diagnostic only",
        "field_count": len(fields),
        "fields": fields,
        "deleted_record_count_reliably_counted_as_0x2a": deleted,
        "eof_marker": f"0x{raw[header_length + count * record_length]:02x}",
        "samples_first_three": samples,
        "target_samples": targets,
    }


def record_sample(index: int, record: bytes, fields: list[dict]) -> dict:
    values = {}
    for field in fields:
        begin = field["offset_in_record"]
        value = record[begin : begin + field["length"]]
        values[field["name"]] = {
            "raw_hex": value.hex(),
            "raw_text_cp936_diagnostic": value.decode("cp936", "replace"),
            "trimmed_text_cp936_diagnostic": value.decode("cp936", "replace").strip(),
        }
    return {"record_index": index, "deleted_flag_hex": record[:1].hex(), "fields": values}


def main() -> None:
    base_zip, gbbq_zip, base, gbbq, output = map(Path, sys.argv[1:6])
    output.mkdir(parents=True, exist_ok=True)
    (output / "base.zip.members.json").write_text(json.dumps(member_manifest(base_zip), indent=2) + "\n")
    (output / "gbbq.zip.members.json").write_text(json.dumps(member_manifest(gbbq_zip), indent=2) + "\n")
    (output / "base-dbf-schema-and-samples.json").write_text(
        json.dumps(dbf_schema(base / "base.dbf"), ensure_ascii=False, indent=2) + "\n"
    )
    print("wrote offline DBF/archive evidence to", output)


if __name__ == "__main__":
    main()
