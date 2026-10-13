#!/usr/bin/env python3
"""Read-only query_data timing for 5 fixed entities (S5 §4 numbers).
usage: query_timing.py --sock PATH --out JSON [--end-date 2026-10-08] [--reps 3]
Only calls query_data. The socket must be a private-stack socket (guard_paths)."""
import argparse, json, os, sys, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from s5_common import Mcp, guard_paths, write_json

ENTS = [{"exchange": "sh", "code": c} for c in ("600000", "600519", "601318")] + \
       [{"exchange": "sz", "code": c} for c in ("000001", "300750")]


def timed(m, sel, ds, flt):
    t = time.perf_counter(); cur = None; n = pages = 0
    while True:
        a = {"dataset": ds, "contract_version": "v1", "selection": sel, "filter": flt,
             "version": {"kind": "latest_per_entity"}, "page_size": 1000}
        if cur:
            a["cursor"] = cur
        r = m.call("query_data", a)
        n += len(r.get("records") or []); pages += 1; cur = r.get("next_cursor")
        if not cur:
            return [round((time.perf_counter() - t) * 1000, 1), n, pages]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--sock", required=True); ap.add_argument("--out", required=True)
    ap.add_argument("--end-date", default="2026-10-08"); ap.add_argument("--reps", type=int, default=3)
    a = ap.parse_args()
    s = guard_paths({"sock": a.sock, "out": a.out})
    m = Mcp(s["sock"]); sel = {"kind": "batch", "entities": ENTS}
    res = {}
    for name, ds, flt in (
        ("daily_full", "market.daily", {"kind": "daily", "start_date": "1990-12-19", "end_date": a.end_date}),
        ("daily_40d", "market.daily", {"kind": "daily", "start_date": "2026-09-01", "end_date": a.end_date}),
        ("yearly_all", "market.yearly", {"kind": "yearly", "start_year": 1900, "end_year": 2100})):
        res[name] = [timed(m, sel, ds, flt) for _ in range(a.reps)]   # [ms, records, pages]
    write_json(s["out"], res)
    print(json.dumps(res))


if __name__ == "__main__":
    main()
