import json
import sys
from datetime import datetime, timezone
from pathlib import Path
from zoneinfo import ZoneInfo

import baostock as bs
import baostock.common.context as context


def stamp():
    now = datetime.now(timezone.utc)
    return {
        "utc": now.isoformat().replace("+00:00", "Z"),
        "asia_shanghai": now.astimezone(ZoneInfo("Asia/Shanghai")).isoformat(),
    }


def main():
    code, start_date, end_date, output_path = sys.argv[1:]
    fields = "date,code,open,high,low,close,volume,amount,adjustflag"
    result = {
        "request": {
            "code": code,
            "fields": fields,
            "start_date": start_date,
            "end_date": end_date,
            "frequency": "d",
            "adjustflag": "2",
        },
        "no_application_retry": True,
        "socket_timeout_seconds": 8,
        "login_requested_at": stamp(),
    }
    login = bs.login()
    result["login"] = {
        "mode": "anonymous_handshake",
        "error_code": login.error_code,
        "error_msg": login.error_msg,
        "user_id": getattr(login, "user_id", ""),
        "received_at": stamp(),
    }
    if hasattr(context, "default_socket") and context.default_socket is not None:
        context.default_socket.settimeout(8.0)
    if login.error_code == "0":
        q = bs.query_history_k_data_plus(
            code, fields, start_date=start_date, end_date=end_date,
            frequency="d", adjustflag="2"
        )
        pages = []
        while q.error_code == "0" and q.data:
            pages.append({"page": str(q.cur_page_num), "rows": [list(x) for x in q.data]})
            q.cur_row_num = len(q.data)
            if not q.next():
                break
        rows = [row for page in pages for row in page["rows"]]
        result["response"] = {
            "error_code": q.error_code,
            "error_msg": q.error_msg,
            "fields": list(q.fields),
            "code": q.code,
            "start_date": q.start_date,
            "end_date": q.end_date,
            "frequency": q.frequency,
            "adjustflag_metadata": q.adjustflag,
            "per_page_count": str(q.per_page_count),
            "page_count": len(pages),
            "rows": rows,
            "row_count": len(rows),
            "first_date": rows[0][0] if rows else None,
            "last_date": rows[-1][0] if rows else None,
        }
    else:
        result["response"] = {"error_code": login.error_code, "error_msg": login.error_msg, "rows": []}
    result["received_at"] = stamp()
    bs.logout()
    Path(output_path).write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
