#!/usr/bin/env python3
"""Persist metadata for the invalid pre-prefix-filter BaoStock probe only."""

from __future__ import annotations

import json
import sys
from pathlib import Path


EXPECTED = ["sh.000002", "sh.000003", "sz.000004", "sz.000006"]


def main() -> None:
    source_path, output_path = map(Path, sys.argv[1:3])
    source = json.loads(source_path.read_text())
    if source.get("selection") != EXPECTED:
        raise SystemExit(f"unexpected invalid-selection probe set: {source.get('selection')}")
    queries = []
    for item in source["queries"]:
        query = item.get("query") or {}
        queries.append({
            "security": item["request"]["code"],
            "request": item["request"],
            "requested_at": item["requested_at"],
            "received_at": item.get("received_at"),
            "login": {
                "mode": item.get("login", {}).get("mode"),
                "error_code": item.get("login", {}).get("error_code"),
                "error_msg": item.get("login", {}).get("error_msg"),
            },
            "query_status": "success" if query.get("error_code") == "0" else "failure",
            "error_code": query.get("error_code"),
            "error_msg": query.get("error_msg"),
            "row_count": query.get("row_count"),
            "first_date": query.get("first_date"),
            "last_date": query.get("last_date"),
        })
    output = {
        "schema": "x-data-mcp.issue-3.cross-stock.pre-probe-selection-bug.v1",
        "incident": "PRE_PROBE_SELECTION_BUG",
        "occurred_before": "A-share code-range proxy was added to select_samples.py",
        "invalid_selection": EXPECTED,
        "excluded_from_validation_conclusions": True,
        "raw_rows_not_copied": True,
        "selection_was_not_reused": True,
        "queries": queries,
        "correction": "The six-digit .day-member proxy was too broad and admitted non-A-share instruments; the corrected selection uses explicit SH/SZ A-share code prefixes and remains fixed before any new probe.",
    }
    output_path.write_text(json.dumps(output, ensure_ascii=False, indent=2, sort_keys=True) + "\n")
    print(json.dumps({"incident": output["incident"], "excluded": True, "requests": [{"security": q["security"], "status": q["query_status"], "row_count": q["row_count"]} for q in queries]}, ensure_ascii=False, sort_keys=True))


if __name__ == "__main__":
    main()
