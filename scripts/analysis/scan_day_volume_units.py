#!/usr/bin/env python3
"""Read-only `.day` volume-unit scan for the hand-recorded (「手」) records (#381).

TDX records a day's volume in hands (1 hand = 100 shares) once the share count
approaches the u32 limit of the volume field, and flags the record with
``0xC36400xx`` in r7. This scan reports, over one installed generation's
official ``hsjday.zip``:

* every record carrying the marker, with the raw-value consistency check
  ``low <= amount / (volume * 100) <= high`` (``.day`` fen prices, unadjusted);
* records without the marker whose amount only makes sense as hands
  (``consistent`` without the marker) — the reverse mismatch that must publish
  an unknown volume rather than a guess;
* the A-share catalog scope member set taken from ``base.zip!tdxhy.cfg``
  (frozen rule S: ``f0 in {0,1}`` and both industry fields non-empty), so the
  per-record counts can be split into "what the catalog actually publishes"
  and "everything else in the archive".

It reads the archives member by member in fixed-size chunks and never loads a
whole member into memory; it opens no database and writes nothing except the
optional ``--json`` report.

Usage::

    python3 scripts/analysis/scan_day_volume_units.py --generation <dir> \
        [--json <path>]
"""
from __future__ import annotations

import argparse
import json
import struct
import sys
import zipfile
from array import array

RECORD_BYTES = 32
MARKER = 0xC364_0000
MARKER_MASK = 0xFFFF_FF00
CHUNK_RECORDS = 65536


def scope_members(base_zip: zipfile.ZipFile) -> set[str]:
    """Catalog scope S members as ``sh<code>``/``sz<code>`` day-member stems."""
    data = base_zip.read("tdxhy.cfg")
    if not data.isascii():
        raise SystemExit("tdxhy.cfg is not pure ASCII")
    members: set[str] = set()
    for line in data.decode("ascii").split("\r\n"):
        if not line:
            continue
        fields = line.split("|")
        if len(fields) != 6:
            raise SystemExit(f"tdxhy.cfg line is not six fields: {line!r}")
        if fields[0] not in ("0", "1") or not fields[2] or not fields[5]:
            continue
        members.add(("sh" if fields[0] == "1" else "sz") + fields[1])
    return members


def f32(bits: int) -> float:
    return struct.unpack("<f", struct.pack("<I", bits))[0]


def day_members(archive: zipfile.ZipFile):
    for info in archive.infolist():
        name = info.filename.replace("\\", "/")
        if not name.endswith(".day"):
            continue
        stem = name.rsplit("/", 1)[-1][:-4]
        if len(stem) == 8 and stem[:2] in ("sh", "sz") and stem[2:].isdigit():
            yield stem, info


def scan(generation: str) -> dict:
    base = zipfile.ZipFile(f"{generation}/base.zip")
    scope = scope_members(base)
    days = zipfile.ZipFile(f"{generation}/hsjday.zip")
    stats = {
        "all_members": 0,
        "all_records": 0,
        "scope_members": len(scope),
        "scope_records": 0,
        "scope_marker_pass": 0,
        "scope_marker_fail": 0,
        "scope_nomarker_hands_pass": 0,
        "scope_zero_volume": 0,
        "scope_zero_amount": 0,
        "scope_max_unmarked_volume": 0,
        "scope_min_marked_shares": None,
    }
    marker_bars: list[dict] = []
    reverse_mismatch: list[dict] = []
    for stem, info in day_members(days):
        stats["all_members"] += 1
        in_scope = stem in scope
        with days.open(info) as handle:
            pending = b""
            while True:
                block = handle.read(CHUNK_RECORDS * RECORD_BYTES)
                if not block:
                    break
                buf = pending + block
                usable = len(buf) - (len(buf) % RECORD_BYTES)
                pending = buf[usable:]
                words = array("I")
                words.frombytes(buf[:usable])
                count = usable // RECORD_BYTES
                stats["all_records"] += count
                if in_scope:
                    stats["scope_records"] += count
                for i in range(count):
                    base_index = i * 8
                    r7 = words[base_index + 7]
                    volume = words[base_index + 6]
                    amount = f32(words[base_index + 5])
                    low = words[base_index + 3] / 100.0
                    high = words[base_index + 2] / 100.0
                    marked = (r7 & MARKER_MASK) == MARKER
                    consistent = False
                    if volume and amount > 0.0:
                        consistent = low <= amount / (volume * 100.0) <= high
                    if in_scope and not marked and volume > stats["scope_max_unmarked_volume"]:
                        stats["scope_max_unmarked_volume"] = volume
                    if marked:
                        if in_scope:
                            implied = volume * 100
                            smallest = stats["scope_min_marked_shares"]
                            if smallest is None or implied < smallest:
                                stats["scope_min_marked_shares"] = implied
                        marker_bars.append(
                            {
                                "member": stem,
                                "in_scope": in_scope,
                                "date": words[base_index],
                                "r7": f"0x{r7:08X}",
                                "volume_raw": volume,
                                "amount": amount,
                                "low": low,
                                "high": high,
                                "consistent": consistent,
                            }
                        )
                        if in_scope:
                            stats["scope_marker_pass" if consistent else "scope_marker_fail"] += 1
                    elif in_scope and consistent:
                        stats["scope_nomarker_hands_pass"] += 1
                        reverse_mismatch.append(
                            {
                                "member": stem,
                                "date": words[base_index],
                                "r7": f"0x{r7:08X}",
                                "volume_raw": volume,
                                "amount": amount,
                                "low": low,
                                "high": high,
                            }
                        )
                    elif in_scope and volume == 0:
                        stats["scope_zero_volume"] += 1
                    elif in_scope and amount == 0.0:
                        stats["scope_zero_amount"] += 1
            if pending:
                raise SystemExit(f"member {stem} length is not divisible by 32")
    stats["marker_members"] = len({bar["member"] for bar in marker_bars})
    stats["marker_records"] = len(marker_bars)
    return {
        "generation": generation,
        "stats": stats,
        "marker_bars": marker_bars,
        "reverse_mismatch": reverse_mismatch,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--generation", required=True, help="installed generation directory")
    parser.add_argument("--json", help="write the full report (bars included) here")
    args = parser.parse_args()
    report = scan(args.generation)
    if args.json:
        with open(args.json, "w") as handle:
            json.dump(report, handle, indent=1)
    json.dump(report["stats"], sys.stdout, indent=1)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
