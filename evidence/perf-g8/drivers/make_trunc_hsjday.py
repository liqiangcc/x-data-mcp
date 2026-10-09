#!/usr/bin/env python3
"""gen-A hsjday: drop the trailing 32-byte record from every .day member.
Non-.day members copied byte-identically. Streaming rewrite via zipfile."""
import json, sys, zipfile
SRC = "/root/x-perf-g8/input/hsjday.zip"
DST = "/root/x-perf-g8/input-old/hsjday.zip"
stats = {"members": 0, "day_members": 0, "truncated": 0, "bytes_removed": 0,
         "day_not_mod32": [], "day_too_small": [], "non_day": []}
with zipfile.ZipFile(SRC) as zin, zipfile.ZipFile(DST, "w", zipfile.ZIP_DEFLATED) as zout:
    for info in zin.infolist():
        stats["members"] += 1
        name = info.filename.replace("\\", "/")
        data = zin.read(info.filename)
        if name.lower().endswith(".day"):
            stats["day_members"] += 1
            if len(data) % 32 != 0:
                stats["day_not_mod32"].append(info.filename)
            elif len(data) <= 32:
                stats["day_too_small"].append(info.filename)
            else:
                data = data[:-32]
                stats["truncated"] += 1
                stats["bytes_removed"] += 32
        else:
            stats["non_day"].append(info.filename)
        zi = zipfile.ZipInfo(info.filename, date_time=info.date_time)
        zi.external_attr = info.external_attr
        zi.compress_type = zipfile.ZIP_DEFLATED
        zout.writestr(zi, data)
print(json.dumps(stats, indent=1))
