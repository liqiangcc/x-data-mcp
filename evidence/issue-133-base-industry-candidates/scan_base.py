#!/usr/bin/env python3
"""Deterministically locate fixed industry-code bytes in a retained base.zip.

This is an evidence locator only. It never decodes member content or infers
code/name/hierarchy semantics.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import posixpath
import struct
import zipfile
from pathlib import Path


RULES_VERSION = "issue-133-base-industry-candidate-rules-v1"
ARCHIVE_BYTES = 16_099_192
ARCHIVE_SHA256 = "39de51e26c0ee8fb3fd970ee870f4cb955b8fbbd7f84d752157f3998e49b71d9"
TOKENS = {
    "X4202": b"X4202",
    "X420202": b"X420202",
    "T1205": b"T1205",
    "688318": b"688318",
}
PRIMARY_MEMBERS = {"tdxhy.cfg", "tdxhy2.cfg"}
DICTIONARY_MEMBERS = {
    "infoharbor_ex.code",
    "infoharbor_ex.name",
    "infoharbor_ex.dat",
    "base.dbf",
    "base.map",
}


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def all_offsets(data: bytes, token: bytes) -> list[int]:
    offsets: list[int] = []
    start = 0
    while True:
        offset = data.find(token, start)
        if offset < 0:
            return offsets
        offsets.append(offset)
        start = offset + 1


def safe_ascii(value: bytes) -> str | None:
    return value.decode("ascii") if all(32 <= byte < 127 for byte in value) else None


def member_class(name: str) -> str:
    basename = posixpath.basename(name)
    if basename in PRIMARY_MEMBERS:
        return "PRIMARY_CLASSIFICATION"
    if basename in DICTIONARY_MEMBERS:
        return "DICTIONARY_CANDIDATE"
    return "OTHER_MEMBER"


def local_data_offset(archive_file, info: zipfile.ZipInfo) -> int:
    archive_file.seek(info.header_offset)
    header = archive_file.read(30)
    if len(header) != 30 or header[:4] != b"PK\x03\x04":
        raise ValueError(f"invalid local header for {info.filename}")
    fields = struct.unpack("<4s5H3I2H", header)
    filename_length, extra_length = fields[-2:]
    return info.header_offset + 30 + filename_length + extra_length


def load_retained_index(path: Path) -> dict[str, dict[str, object]]:
    index = json.loads(path.read_text(encoding="utf-8"))
    if index["archive_sha256"] != ARCHIVE_SHA256:
        raise ValueError("retained index archive SHA does not match fixed input")
    if index["content_length_header"] != ARCHIVE_BYTES:
        raise ValueError("retained index archive size does not match fixed input")
    if index["member_count"] != 542 or len(index["members"]) != 542:
        raise ValueError("retained index does not describe exactly 542 members")
    members = {member["name"]: member for member in index["members"]}
    if len(members) != 542:
        raise ValueError("retained index has duplicate member names")
    return members


def scan(archive_path: Path, retained_index_path: Path) -> dict[str, object]:
    archive_bytes = archive_path.read_bytes()
    actual_archive_sha = sha256_bytes(archive_bytes)
    if len(archive_bytes) != ARCHIVE_BYTES or actual_archive_sha != ARCHIVE_SHA256:
        raise ValueError(
            f"archive identity mismatch: bytes={len(archive_bytes)} sha256={actual_archive_sha}"
        )
    retained_members = load_retained_index(retained_index_path)
    members: list[dict[str, object]] = []
    with zipfile.ZipFile(archive_path) as archive:
        infos = sorted(archive.infolist(), key=lambda info: (info.filename, info.header_offset))
        if len(infos) != 542:
            raise ValueError(f"archive member count is {len(infos)}, expected 542")
        for info in infos:
            data = archive.read(info)
            member_sha = sha256_bytes(data)
            retained = retained_members.get(info.filename)
            if retained is None:
                raise ValueError(f"member missing from retained index: {info.filename}")
            if retained["bytes"] != len(data) or retained["sha256"] != member_sha:
                raise ValueError(f"member identity mismatch: {info.filename}")
            hits: list[dict[str, object]] = []
            for token_name, token in TOKENS.items():
                for offset in all_offsets(data, token):
                    matched = data[offset : offset + len(token)]
                    hits.append(
                        {
                            "token": token_name,
                            "byte_offset": offset,
                            "matched_hex": matched.hex(),
                            "matched_ascii": safe_ascii(matched),
                        }
                    )
            hits.sort(key=lambda hit: (hit["byte_offset"], hit["token"]))
            members.append(
                {
                    "name": info.filename,
                    "class": member_class(info.filename),
                    "bytes": len(data),
                    "sha256": member_sha,
                    "compressed_bytes": info.compress_size,
                    "crc32": f"{info.CRC:08x}",
                    "zip_local_header_offset": info.header_offset,
                    "zip_data_offset": local_data_offset(archive.fp, info),
                    "hits": hits,
                }
            )
    members.sort(key=lambda member: (member["name"], member["zip_local_header_offset"]))
    candidate_hits = [
        {"member": member["name"], "class": member["class"], "sha256": member["sha256"], "hit": hit}
        for member in members
        if member["class"] != "OTHER_MEMBER"
        for hit in member["hits"]
    ]
    other_hits = [
        {"member": member["name"], "sha256": member["sha256"], "hit": hit}
        for member in members
        if member["class"] == "OTHER_MEMBER"
        for hit in member["hits"]
    ]
    if candidate_hits and not other_hits:
        result = "FOUND_CANDIDATE"
    elif candidate_hits or other_hits:
        result = "AMBIGUOUS"
    else:
        result = "NO_CANDIDATE_FOUND"
    return {
        "schema": "x-data-mcp.issue-133.base-industry-candidate-scan.v1",
        "rules_version": RULES_VERSION,
        "archive": {
            "path_argument": str(archive_path),
            "bytes": len(archive_bytes),
            "sha256": actual_archive_sha,
            "retained_index_sha256": ARCHIVE_SHA256,
            "retained_index_match": True,
        },
        "scanned_member_count": len(members),
        "result": result,
        "candidate_hits": candidate_hits,
        "other_member_hits": other_hits,
        "members": members,
        "limits": [
            "All matches are raw-byte locator evidence only.",
            "No encoding conversion, code prefix truncation, code-to-name mapping, or hierarchy inference was performed.",
            "FOUND_CANDIDATE does not establish the #46 research-industry level-2 contract or production readiness.",
        ],
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("archive", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument(
        "--retained-index",
        type=Path,
        default=Path("evidence/issue-6-tdx-dbf-source/official/archive-20260913.json"),
    )
    args = parser.parse_args()
    result = scan(args.archive, args.retained_index)
    args.output.write_text(
        json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )


if __name__ == "__main__":
    main()
