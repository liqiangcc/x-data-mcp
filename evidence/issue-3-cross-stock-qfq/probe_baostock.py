#!/usr/bin/env python3
"""One bounded BaoStock qfq-daily query per preselected security."""

from __future__ import annotations

import hashlib
import json
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from zoneinfo import ZoneInfo

import baostock as bs
import baostock.common.context as context


START_DATE = "2025-01-01"
END_DATE = "2026-09-10"
FIELDS = "date,code,open,high,low,close,preclose,volume,amount,adjustflag"
SOCKET_TIMEOUT_SECONDS = 8.0


def stamps() -> dict[str, str]:
    current = datetime.now(timezone.utc)
    return {
        "utc": current.isoformat().replace("+00:00", "Z"),
        "asia_shanghai": current.astimezone(ZoneInfo("Asia/Shanghai")).isoformat(),
    }


def response_hash(fields: list[str], rows: list[list[str]]) -> str:
    payload = json.dumps({"fields": fields, "rows": rows}, ensure_ascii=False, separators=(",", ":")).encode()
    return hashlib.sha256(payload).hexdigest()


def query_one(code: str, label: str) -> dict:
    query_started = stamps()
    result = None
    pages: list[dict] = []
    login_started = stamps()
    login = bs.login()
    login_record = {
        "mode": "anonymous_handshake",
        "requested_at": login_started,
        "received_at": stamps(),
        "error_code": login.error_code,
        "error_msg": login.error_msg,
        "user_id": getattr(login, "user_id", ""),
    }
    try:
        if hasattr(context, "default_socket") and context.default_socket is not None:
            context.default_socket.settimeout(SOCKET_TIMEOUT_SECONDS)
        if login.error_code != "0":
            return {
                "label": label,
                "request": {"code": code, "fields": FIELDS, "start_date": START_DATE, "end_date": END_DATE, "frequency": "d", "adjustflag": "2"},
                "requested_at": query_started,
                "login": login_record,
                "query": None,
                "failure": "anonymous handshake failed; no query attempted",
            }
        result = bs.query_history_k_data_plus(
            code,
            FIELDS,
            start_date=START_DATE,
            end_date=END_DATE,
            frequency="d",
            adjustflag="2",
        )
        while result.error_code == "0" and result.data:
            rows = [list(row) for row in result.data]
            pages.append({
                "page": str(result.cur_page_num),
                "row_count": len(rows),
                "rows": rows,
            })
            result.cur_row_num = len(result.data)
            if not result.next():
                break
        rows = [row for page in pages for row in page["rows"]]
        fields = list(result.fields)
        return {
            "label": label,
            "request": {"code": code, "fields": FIELDS, "start_date": START_DATE, "end_date": END_DATE, "frequency": "d", "adjustflag": "2"},
            "requested_at": query_started,
            "received_at": stamps(),
            "login": login_record,
            "query": {
                "error_code": result.error_code,
                "error_msg": result.error_msg,
                "fields": fields,
                "code": result.code,
                "start_date": result.start_date,
                "end_date": result.end_date,
                "frequency": result.frequency,
                "adjustflag": result.adjustflag,
                "per_page_count": str(result.per_page_count),
                "page_count": len(pages),
                "pages": pages,
                "rows": rows,
                "row_count": len(rows),
                "first_date": rows[0][0] if rows else None,
                "last_date": rows[-1][0] if rows else None,
                "parsed_response_sha256": response_hash(fields, rows),
            },
        }
    except Exception as error:
        return {
            "label": label,
            "request": {"code": code, "fields": FIELDS, "start_date": START_DATE, "end_date": END_DATE, "frequency": "d", "adjustflag": "2"},
            "requested_at": query_started,
            "received_at": stamps(),
            "login": login_record,
            "query": None,
            "failure": f"{type(error).__name__}: {error}",
        }
    finally:
        bs.logout()


def main() -> None:
    selection_path, output_path = map(Path, sys.argv[1:3])
    selection = json.loads(selection_path.read_text())
    result = {
        "schema": "x-data-mcp.issue-3.cross-stock.baostock-probe.v1",
        "purpose": "bounded BaoStock qfq daily comparison after fixed offline selection",
        "client": {"package": "baostock", "version": getattr(bs, "__version__", "unknown"), "socket_timeout_seconds": SOCKET_TIMEOUT_SECONDS},
        "environment": {"python": sys.version, "timezone": "Asia/Shanghai", "no_application_retry": True, "no_source_year_request": True},
        "selection_sha256": hashlib.sha256(selection_path.read_bytes()).hexdigest(),
        "selection": [f"{item['market']}.{item['code']}" for item in selection["selected"]],
        "queries": [],
        "started_at": stamps(),
    }
    for item in selection["selected"]:
        result["queries"].append(query_one(f"{item['market']}.{item['code']}", f"{item['market']}.{item['code']}.fixed_history"))
        time.sleep(0.1)
    result["finished_at"] = stamps()
    output_path.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps({"selection": result["selection"], "queries": [{"label": q["label"], "row_count": q["query"]["row_count"] if q["query"] else None, "failure": q.get("failure")} for q in result["queries"]]}, ensure_ascii=False, sort_keys=True))


if __name__ == "__main__":
    main()
