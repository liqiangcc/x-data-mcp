#!/usr/bin/env python3
"""Build the gen-3 `x-data-mcp.official-file-input.v1` snapshot manifest
from the verbatim acquisition facts committed in
evidence/issue-217-gen3-acquisition/acquisition.json.

Interval fields are derived honestly: acquisition_started_at is the
earliest observed UTC timestamp for that file (attempt response `Date`
header or retrieved_utc, whichever is earlier); acquisition_completed_at
is the runner-recorded retrieved_utc. Nothing else is invented.
"""
import email.utils
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
ACQ = os.path.join(REPO, "evidence/issue-217-gen3-acquisition/acquisition.json")
OUT = os.path.join(HERE, "snapshot-manifest-gen3.json")

ROLE_MAP = {"base": "base_industry", "gbbq": "gbbq", "hsjday": "day"}


def rfc3339(ts):
    return ts.replace("+00:00", "Z")


def attempt_start(f):
    times = [f["retrieved_utc"]]
    for a in f.get("attempts") or []:
        d = (a.get("response_headers") or {}).get("date")
        if d:
            times.append(
                email.utils.parsedate_to_datetime(d)
                .isoformat().replace("+00:00", "+00:00"))
    return rfc3339(min(times))


def main():
    acq = json.load(open(ACQ))
    files = []
    for key, role in ROLE_MAP.items():
        f = acq["files"][key]
        files.append({
            "role": role,
            "relative_path": f["url"].rsplit("/", 1)[-1],
            "source_url": f["url"],
            "acquisition_started_at": attempt_start(f),
            "acquisition_completed_at": rfc3339(f["retrieved_utc"]),
            "last_modified": f.get("last_modified"),
            "etag": f.get("etag"),
            "bytes": f["bytes"],
            "sha256": f["sha256"],
        })
    manifest = {"schema": "x-data-mcp.official-file-input.v1",
                "files": files}
    with open(OUT, "w") as fh:
        json.dump(manifest, fh, ensure_ascii=False, indent=1)
    print(OUT)


if __name__ == "__main__":
    main()
