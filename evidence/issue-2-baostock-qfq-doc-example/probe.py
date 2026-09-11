import hashlib
import json
import sys
from datetime import datetime, timezone
from decimal import Decimal, InvalidOperation, ROUND_HALF_UP
from pathlib import Path
from zoneinfo import ZoneInfo

import baostock as bs
import baostock.common.context as context


CODE = "sh.600000"
START = "2017-05-24"
END = "2017-05-26"
FIELDS = "date,code,open,high,low,close,preclose,volume,amount,adjustflag"
EXPECTED = {
    "qfq": {
        "2017-05-24": {"open": "11.681648", "close": "11.750007", "preclose": "11.719625"},
        "2017-05-25": {"open": "11.75", "close": "12.93", "preclose": "11.75"},
        "2017-05-26": {"open": "12.81", "close": "12.84", "preclose": "12.93"},
    },
    "none": {
        "2017-05-24": {"open": "15.3800", "close": "15.4700", "preclose": "15.4300"},
        "2017-05-25": {"open": "11.7500", "close": "12.9300", "preclose": "11.7500"},
        "2017-05-26": {"open": "12.8100", "close": "12.8400", "preclose": "12.9300"},
    },
}


def stamp():
    now = datetime.now(timezone.utc)
    return {
        "utc": now.isoformat().replace("+00:00", "Z"),
        "asia_shanghai": now.astimezone(ZoneInfo("Asia/Shanghai")).isoformat(),
    }


def sha256(fields, rows):
    raw = json.dumps({"fields": fields, "rows": rows}, ensure_ascii=False, separators=(",", ":")).encode()
    return hashlib.sha256(raw).hexdigest()


def classify(live, documented):
    if live == documented:
        return "live_string_exact"
    try:
        live_decimal = Decimal(live)
        documented_decimal = Decimal(documented)
    except (InvalidOperation, TypeError):
        return "inconsistent"
    if live_decimal == documented_decimal:
        return "numeric_decimal_exact_text_diff"
    # Documentation-display comparison only; this is not a production tolerance.
    places = max(0, -documented_decimal.as_tuple().exponent)
    quantum = Decimal(1).scaleb(-places)
    if live_decimal.quantize(quantum, rounding=ROUND_HALF_UP) == documented_decimal:
        return "documentation_display_precision_consistent_only"
    return "inconsistent"


def main():
    mode, output_path = sys.argv[1:]
    adjustflag = {"qfq": "2", "none": "3"}[mode]
    output = {
        "schema": "x-data-mcp.issue-2.baostock-qfq-doc-example.v1",
        "mode": mode,
        "request": {
            "code": CODE,
            "start_date": START,
            "end_date": END,
            "frequency": "d",
            "adjustflag": adjustflag,
            "fields": FIELDS,
        },
        "no_application_retry": True,
        "socket_timeout_seconds": 8,
        "login": {"mode": "anonymous_handshake", "requested_at": stamp()},
    }
    login = bs.login()
    output["login"].update({
        "received_at": stamp(),
        "error_code": login.error_code,
        "error_msg": login.error_msg,
        "user_id": getattr(login, "user_id", ""),
    })
    if hasattr(context, "default_socket") and context.default_socket is not None:
        context.default_socket.settimeout(8.0)
    if login.error_code == "0":
        query_started = stamp()
        result = bs.query_history_k_data_plus(
            CODE, FIELDS, start_date=START, end_date=END, frequency="d", adjustflag=adjustflag
        )
        rows = [list(row) for row in result.data] if result.error_code == "0" else []
        output["response"] = {
            "requested_at": query_started,
            "received_at": stamp(),
            "error_code": result.error_code,
            "error_msg": result.error_msg,
            "fields": list(result.fields),
            "code": result.code,
            "start_date": result.start_date,
            "end_date": result.end_date,
            "frequency": result.frequency,
            "adjustflag_metadata": result.adjustflag,
            "row_count": len(rows),
            "first_date": rows[0][0] if rows else None,
            "last_date": rows[-1][0] if rows else None,
            "rows": rows,
            "parsed_response_sha256": sha256(result.fields, rows),
        }
        indexes = {name: i for i, name in enumerate(result.fields)}
        comparisons = []
        for row in rows:
            date = row[indexes["date"]]
            expected = EXPECTED[mode].get(date)
            if expected is None:
                continue
            for field, documented in expected.items():
                live = row[indexes[field]]
                comparisons.append({
                    "date": date,
                    "field": field,
                    "live": live,
                    "documented": documented,
                    "classification": classify(live, documented),
                })
        output["comparison"] = {
            "basis": "Issue-provided BaoStock official PDF example values; no float conversion",
            "fields": ["date", "code", "open", "close", "preclose"],
            "results": comparisons,
        }
    else:
        output["response"] = {
            "error_code": login.error_code,
            "error_msg": login.error_msg,
            "row_count": 0,
            "rows": [],
        }
    output["finished_at"] = stamp()
    try:
        bs.logout()
    finally:
        Path(output_path).write_text(json.dumps(output, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
