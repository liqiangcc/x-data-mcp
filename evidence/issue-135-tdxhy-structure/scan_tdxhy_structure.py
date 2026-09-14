#!/usr/bin/env python3
"""Offline byte-structure evidence for the two frozen TDX industry members."""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import zipfile
from collections import Counter, defaultdict
from pathlib import Path

ARCHIVE_BYTES = 16_099_192
ARCHIVE_SHA256 = "39de51e26c0ee8fb3fd970ee870f4cb955b8fbbd7f84d752157f3998e49b71d9"
TARGETS = ("tdxhy.cfg", "tdxhy2.cfg")
SAMPLES = ("688318", "600000", "000001")
TOKEN_RE = re.compile(rb"(?<![0-9])([0-9]{6})(?![0-9])")
RULES_VERSION = "structure-rules-v1"


def sha256(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def optional_ascii(value: bytes) -> str | None:
    return value.decode("ascii") if all(32 <= b < 127 for b in value) else None


def retained_members(path: Path) -> dict[str, dict[str, object]]:
    index = json.loads(path.read_text(encoding="utf-8"))
    if index.get("archive_sha256") != ARCHIVE_SHA256:
        raise ValueError("retained archive SHA mismatch")
    if index.get("actual_bytes") != ARCHIVE_BYTES:
        raise ValueError("retained archive byte count mismatch")
    if index.get("member_count") != 542 or len(index.get("members", [])) != 542:
        raise ValueError("retained index must contain 542 members")
    members = {str(item["name"]): item for item in index["members"]}
    if len(members) != 542:
        raise ValueError("retained index has duplicate member names")
    missing = [name for name in TARGETS if name not in members]
    if missing:
        raise ValueError(f"retained index missing target: {missing}")
    return members


def parse_lines(data: bytes) -> list[dict[str, object]]:
    lines: list[dict[str, object]] = []
    start = 0
    number = 0
    i = 0
    while i < len(data):
        byte = data[i]
        if byte not in (10, 13):
            i += 1
            continue
        end = i
        if byte == 13 and i + 1 < len(data) and data[i + 1] == 10:
            eol = "CRLF"
            next_start = i + 2
        else:
            eol = "CR" if byte == 13 else "LF"
            next_start = i + 1
        content = data[start:end]
        lines.append(
            {
                "line_number": number,
                "line_start_offset": start,
                "content": content,
                "eol": eol,
            }
        )
        number += 1
        start = next_start
        i = next_start
    if start < len(data):
        lines.append(
            {
                "line_number": number,
                "line_start_offset": start,
                "content": data[start:],
                "eol": None,
            }
        )
    return lines


def field_index(line: bytes, offset: int) -> int:
    return line[:offset].count(b"|")


def token_observations(data: bytes, lines: list[dict[str, object]]) -> list[dict[str, object]]:
    observations: list[dict[str, object]] = []
    for line in lines:
        content = line["content"]
        assert isinstance(content, bytes)
        line_start = int(line["line_start_offset"])
        line_number = int(line["line_number"])
        for match in TOKEN_RE.finditer(content):
            token = match.group(1)
            offset = match.start(1)
            raw_line = content
            observations.append(
                {
                    "token": token.decode("ascii"),
                    "member_offset": line_start + offset,
                    "line_number": line_number,
                    "line_start_offset": line_start,
                    "line_offset": offset,
                    "field_index": field_index(content, offset),
                    "raw_line_hex": raw_line.hex(),
                    "raw_line_ascii": optional_ascii(raw_line),
                    "raw_hex": token.hex(),
                    "raw_ascii": token.decode("ascii"),
                    "matched_hex": token.hex(),
                    "matched_ascii": token.decode("ascii"),
                }
            )
    return observations


def member_report(name: str, data: bytes, expected: dict[str, object]) -> dict[str, object]:
    actual_sha = sha256(data)
    if len(data) != expected["bytes"] or actual_sha != expected["sha256"]:
        raise ValueError(f"member identity mismatch: {name}")
    lines = parse_lines(data)
    eol_counts = Counter(str(line["eol"]) for line in lines if line["eol"] is not None)
    blank_count = sum(1 for line in lines if line["content"] == b"")
    nonblank = [line for line in lines if line["content"] != b""]
    field_counts = Counter(
        len(line["content"].split(b"|")) for line in nonblank
    )
    observations = token_observations(data, lines)
    by_token: dict[str, list[dict[str, object]]] = defaultdict(list)
    for observation in observations:
        by_token[str(observation["token"])].append(observation)
    token_stats = []
    for token in sorted(by_token):
        hits = sorted(by_token[token], key=lambda item: (item["member_offset"], item["line_number"]))
        compact_hits = [
            [
                hit["member_offset"],
                hit["line_number"],
                hit["line_offset"],
                hit["field_index"],
                hit["raw_hex"],
            ]
            for hit in hits
        ]
        line_numbers = sorted({int(hit["line_number"]) for hit in hits})
        token_stats.append(
            {
                "token": token,
                "occurrence_count": len(hits),
                "line_count": len(line_numbers),
                "line_numbers": line_numbers,
                "hit_tuple_fields": [
                    "member_offset",
                    "line_number",
                    "line_offset",
                    "field_index",
                    "raw_hex",
                ],
                "hits": compact_hits,
            }
        )
    sample_hits = {sample: [hit for hit in observations if hit["token"] == sample] for sample in SAMPLES}
    return {
        "name": name,
        "bytes": len(data),
        "sha256": actual_sha,
        "expected_bytes": expected["bytes"],
        "expected_sha256": expected["sha256"],
        "identity_match": True,
        "line_count": len(lines),
        "record_count": len(nonblank),
        "blank_line_count": blank_count,
        "line_ending_counts": dict(sorted(eol_counts.items())),
        "final_line_terminated": bool(lines and lines[-1]["eol"] is not None),
        "field_count_distribution": {str(k): field_counts[k] for k in sorted(field_counts)},
        "sample_hits": sample_hits,
        "six_digit_token_set": sorted(by_token),
        "six_digit_token_stats": token_stats,
    }


def scan(archive_path: Path, retained_path: Path) -> dict[str, object]:
    archive_data = archive_path.read_bytes()
    archive_sha = sha256(archive_data)
    if len(archive_data) != ARCHIVE_BYTES or archive_sha != ARCHIVE_SHA256:
        raise ValueError(f"archive identity mismatch: bytes={len(archive_data)} sha256={archive_sha}")
    retained = retained_members(retained_path)
    with zipfile.ZipFile(archive_path) as archive:
        infos = {info.filename: info for info in archive.infolist()}
        if len(infos) != 542 or set(infos) != set(retained):
            raise ValueError("archive member table does not match retained 542-member index")
        reports = []
        for name in TARGETS:
            # Only the two frozen target members are read for content evidence.
            reports.append(member_report(name, archive.read(infos[name]), retained[name]))
    reports.sort(key=lambda item: item["name"])
    left, right = (set(report["six_digit_token_set"]) for report in reports)
    by_name = {report["name"]: report for report in reports}
    intersection = sorted(left & right)
    only_left = sorted(left - right)
    only_right = sorted(right - left)
    overlap = []
    ambiguous = False
    for token in intersection:
        lstat = next(item for item in by_name[TARGETS[0]]["six_digit_token_stats"] if item["token"] == token)
        rstat = next(item for item in by_name[TARGETS[1]]["six_digit_token_stats"] if item["token"] == token)
        cardinality = lstat["line_count"] * rstat["line_count"]
        if lstat["line_count"] > 1 or rstat["line_count"] > 1:
            ambiguous = True
        overlap.append(
            {
                "token": token,
                "left_occurrence_count": lstat["occurrence_count"],
                "right_occurrence_count": rstat["occurrence_count"],
                "left_line_count": lstat["line_count"],
                "right_line_count": rstat["line_count"],
                "line_cartesian_cardinality": cardinality,
            }
        )
    if not intersection:
        classification = "NO_STRUCTURAL_RELATION_FOUND"
    elif ambiguous:
        classification = "STRUCTURAL_RELATION_AMBIGUOUS"
    else:
        classification = "STRUCTURAL_RELATION_FOUND"
    return {
        "schema": "x-data-mcp.issue-135.tdxhy-structure.v1",
        "rules_version": RULES_VERSION,
        "archive": {
            "path_argument": str(archive_path),
            "bytes": len(archive_data),
            "sha256": archive_sha,
            "retained_archive_sha256": ARCHIVE_SHA256,
            "retained_archive_match": True,
            "member_count": 542,
        },
        "target_members": list(TARGETS),
        "member_identity_verified": True,
        "classification": classification,
        "members": reports,
        "token_overlap": {
            "left_member": TARGETS[0],
            "right_member": TARGETS[1],
            "left_set_cardinality": len(left),
            "right_set_cardinality": len(right),
            "intersection_cardinality": len(intersection),
            "only_left_cardinality": len(only_left),
            "only_right_cardinality": len(only_right),
            "only_left": only_left,
            "only_right": only_right,
            "intersection": overlap,
        },
        "limits": [
            "All lines and token hits retain deterministic raw_hex; raw_ascii is optional display metadata only.",
            "Six-digit tokens are byte patterns, not asserted security identifiers.",
            "No encoding, name, hierarchy, parent/child, level-2, or code semantics are inferred.",
            "The structural classification does not close #46 or #6 and is not production evidence.",
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
        json.dumps(result, ensure_ascii=False, separators=(",", ":"), sort_keys=True) + "\n",
        encoding="utf-8",
    )


if __name__ == "__main__":
    main()
