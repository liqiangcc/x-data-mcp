import hashlib
import json
import sys
from datetime import datetime, timezone
from pathlib import Path
from zoneinfo import ZoneInfo

import baostock as bs
import baostock.common.context as context


OUT = Path(sys.argv[1])
FIELDS = "date,code,open,high,low,close,volume,amount,adjustflag"
SAMPLES = ["sh.600000", "sz.000001"]


def stamps():
    received = datetime.now(timezone.utc)
    return {
        "utc": received.isoformat().replace("+00:00", "Z"),
        "asia_shanghai": received.astimezone(ZoneInfo("Asia/Shanghai")).isoformat(),
    }


def parsed_sha256(fields, rows):
    payload = json.dumps(
        {"fields": fields, "rows": rows},
        ensure_ascii=False,
        separators=(",", ":"),
    ).encode("utf-8")
    return hashlib.sha256(payload).hexdigest()


def query(code, start_date, end_date, label):
    requested_at = stamps()
    result = bs.query_history_k_data_plus(
        code,
        FIELDS,
        start_date=start_date,
        end_date=end_date,
        frequency="d",
        adjustflag="2",
    )
    pages = []
    pagination_complete = None
    pagination_error = None
    while result.error_code == "0" and result.data:
        page_rows = [list(row) for row in result.data]
        pages.append(
            {
                "page": str(result.cur_page_num),
                "row_count": len(page_rows),
                "rows": page_rows,
            }
        )
        result.cur_row_num = len(result.data)
        if not result.next():
            pagination_complete = len(result.data) < int(result.per_page_count)
            if not pagination_complete and result.error_code != "0":
                pagination_error = {
                    "error_code": result.error_code,
                    "error_msg": result.error_msg,
                }
            break
    rows = [row for page in pages for row in page["rows"]]
    response_at = stamps()
    date_index = 0
    first_date = rows[0][date_index] if rows else None
    last_date = rows[-1][date_index] if rows else None
    return {
        "label": label,
        "request": {
            "code": code,
            "fields": FIELDS,
            "start_date": start_date,
            "end_date": end_date,
            "frequency": "d",
            "adjustflag": "2",
        },
        "requested_at": requested_at,
        "received_at": response_at,
        "response": {
            "error_code": result.error_code,
            "error_msg": result.error_msg,
            "fields": list(result.fields),
            "code": result.code,
            "start_date": result.start_date,
            "end_date": result.end_date,
            "frequency": result.frequency,
            "adjustflag": result.adjustflag,
            "per_page_count": str(result.per_page_count),
            "page_count": len(pages),
            "rows": rows,
            "first_date": first_date,
            "last_date": last_date,
            "row_count": len(rows),
            "parsed_response_sha256": parsed_sha256(result.fields, rows),
            "pagination_complete": pagination_complete,
            "pagination_error": pagination_error,
        },
    }


def main():
    output = {
        "probe": {
            "purpose": "bounded BaoStock qfq daily evidence; no source-year request",
            "client": "baostock",
            "python": sys.version,
            "timezone": "Asia/Shanghai",
            "started_at": stamps(),
            "no_application_retry": True,
            "socket_timeout_seconds": 8,
        },
        "login": {},
        "queries": [],
    }
    login_started = stamps()
    login = bs.login()
    output["login"] = {
        "mode": "anonymous_handshake",
        "requested_at": login_started,
        "received_at": stamps(),
        "error_code": login.error_code,
        "error_msg": login.error_msg,
        "user_id": getattr(login, "user_id", ""),
    }
    if hasattr(context, "default_socket") and context.default_socket is not None:
        context.default_socket.settimeout(8.0)
    if login.error_code != "0":
        output["finished_at"] = stamps()
        OUT.write_text(json.dumps(output, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
        return 2

    for code in SAMPLES:
        output["queries"].append(query(code, "2025-01-01", "2026-09-10", "fixed_history"))
        first = query(code, "2025-01-06", "2025-01-10", "repeat_small_1")
        second = query(code, "2025-01-06", "2025-01-10", "repeat_small_2")
        first_rows = first["response"]["rows"]
        second_rows = second["response"]["rows"]
        first["comparison"] = {
            "compared_with": "repeat_small_2",
            "field_order_equal": first["response"]["fields"] == second["response"]["fields"],
            "rows_equal": first_rows == second_rows,
            "parsed_response_sha256_equal": first["response"]["parsed_response_sha256"] == second["response"]["parsed_response_sha256"],
        }
        output["queries"].extend([first, second])
        output["queries"].append(query(code, "1990-01-01", "2026-09-10", "long_history_observation"))
        output["queries"].append(query(code, "2026-09-11", "2026-09-11", "current_day_observation"))

    output["finished_at"] = stamps()
    bs.logout()
    OUT.write_text(json.dumps(output, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
