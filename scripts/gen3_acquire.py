#!/usr/bin/env python3
"""Issue #217 gen-3 acquisition — Windows runner.

Implements the bounded acquisition spec in
docs/tasks/issue-217-gen3-acquisition.md:

  S1 official files   — re-download gbbq.zip / hsjday.zip / base.zip,
                        record response headers + sha256 verbatim
  S2 TQ oracle        — get_divid_factors + get_market_data(none/front)
                        for the frozen 7-security set, verbatim into
                        issue54.tq-oracle.v1 (plus raw repr for audit)
  S3 metadata         — every version/set datum obtainable is recorded
  S4 boundary note    — emits a #71 template the operator must fill

Stdlib only. Degrades honestly: every step that cannot run records
{"status": "NOT_RUN", "reason": ...} — nothing is faked, nothing is
inferred. Usage:

  python gen3_acquire.py --out gen3-run-20260918 [--skip-download]
                                                   [--skip-tq]
"""
import argparse
import datetime
import hashlib
import json
import os
import sys
import urllib.request

FILES = {
    "gbbq": "https://www.tdx.com.cn/products/data/data/dbf/gbbq.zip",
    "hsjday": "https://data.tdx.com.cn/vipdoc/hsjday.zip",
    "base": "https://www.tdx.com.cn/products/data/data/dbf/base.zip",
}
SECURITIES = ["sh.600004", "sh.600006", "sz.000001", "sz.000002",
              "sz.000004", "sz.000035", "sh.688318"]
ORACLE_FIELDS = ("type", "bonus", "allot_price", "share_bonus",
                 "allotment")


def utcnow():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def step_download(out):
    manifest = {}
    for role, url in FILES.items():
        dest = os.path.join(out, f"{role}.zip")
        try:
            req = urllib.request.Request(url)
            with urllib.request.urlopen(req, timeout=600) as resp:
                headers = {k.lower(): v for k, v in resp.headers.items()}
                status = resp.status
                with open(dest, "wb") as f:
                    while True:
                        chunk = resp.read(1 << 20)
                        if not chunk:
                            break
                        f.write(chunk)
            manifest[role] = {
                "role": role, "url": url, "retrieved_utc": utcnow(),
                "http_status": status,
                "last_modified": headers.get("last-modified"),
                "etag": headers.get("etag"),
                "bytes": os.path.getsize(dest),
                "sha256": sha256_file(dest),
                "response_headers": headers,
            }
        except Exception as e:  # noqa: BLE001 — verbatim capture
            manifest[role] = {"role": role, "url": url,
                              "status": "NOT_RUN",
                              "reason": f"{type(e).__name__}: {e}"}
    return manifest


def norm_code(symbol):
    """sh.600004 -> SH600004; sz.000002 -> SZ000002 (TQ convention)."""
    m, c = symbol.split(".")
    return m.upper() + c


def row_to_dict(row):
    """Tolerant verbatim extraction from a TQ row (dict / Series /
    namedtuple / sequence)."""
    if isinstance(row, dict):
        return {str(k): (None if v is None else str(v))
                for k, v in row.items()}
    if hasattr(row, "to_dict"):
        try:
            return {str(k): (None if v is None else str(v))
                    for k, v in row.to_dict().items()}
        except Exception:  # noqa: BLE001
            pass
    if hasattr(row, "_asdict"):
        return {str(k): (None if v is None else str(v))
                for k, v in row._asdict().items()}
    return {"_raw": repr(row)}


def iter_rows(obj):
    if obj is None:
        return []
    if isinstance(obj, list):
        return obj
    if hasattr(obj, "to_dict") and hasattr(obj, "index"):
        try:  # pandas DataFrame -> records
            return obj.to_dict("records")
        except Exception:  # noqa: BLE001
            pass
    try:
        return list(obj)
    except TypeError:
        return [{"_raw": repr(obj)}]


def pick(row, *names):
    lower = {str(k).lower(): v for k, v in row.items()}
    for n in names:
        if n in lower and lower[n] is not None:
            return lower[n]
    return None


def step_tq(out):
    import importlib
    tq = None
    import_errs = []
    for name in ("tq", "tqcenter.tq", "tqcenter"):
        try:
            mod = importlib.import_module(name)
            tq = getattr(mod, "tq", mod)
            if hasattr(tq, "get_divid_factors") or hasattr(
                    tq, "get_market_data"):
                break
        except Exception as e:  # noqa: BLE001
            import_errs.append(f"{name}: {type(e).__name__}: {e}")
    if tq is None or not (hasattr(tq, "get_divid_factors")
                          or hasattr(tq, "get_market_data")):
        return {"status": "NOT_RUN",
                "reason": "TQ module not importable or lacks expected "
                          "entry points",
                "import_errors": import_errs}

    oracle = {"schema": "issue54.tq-oracle.v1",
              "generator": {"kind": "tq_api", "captured_utc": utcnow(),
                            "environment_note":
                                "windows tqcenter, exact build unknown"},
              "securities": {}, "raw": {}}
    for symbol in SECURITIES:
        code = norm_code(symbol)
        entry = {"divid_factors": [], "front_adjusted": [],
                 "unadjusted": [], "errors": []}
        raw = {}
        # --- get_divid_factors ---
        try:
            df = tq.get_divid_factors(stock_code=code,
                                      start_time="", end_time="")
            raw["divid_factors_repr"] = repr(df)[:200000]
            for r in iter_rows(df):
                rd = row_to_dict(r)
                entry["divid_factors"].append({
                    "date": pick(rd, "date", "ex_date", "exdate",
                                 "dividend_date", "time"),
                    "fields": {f: pick(rd, f, f.replace("_", ""),
                                       f.upper()) for f in ORACLE_FIELDS},
                    "verbatim": rd})
        except Exception as e:  # noqa: BLE001
            entry["errors"].append(
                f"get_divid_factors: {type(e).__name__}: {e}")
        # --- get_market_data none/front (daily close only) ---
        for dtype, key in (("none", "unadjusted"),
                           ("front", "front_adjusted")):
            try:
                md = tq.get_market_data(
                    field_list=["time", "open", "high", "low", "close"],
                    stock_list=[code], period="1d",
                    start_time="", end_time="", count=0,
                    dividend_type=dtype, fill_data=False)
                raw[f"market_data_{dtype}_repr"] = repr(md)[:200000]
                for r in iter_rows(md):
                    rd = row_to_dict(r)
                    entry[key].append({
                        "date": pick(rd, "date", "time", "datetime"),
                        "open": pick(rd, "open"),
                        "high": pick(rd, "high"),
                        "low": pick(rd, "low"),
                        "close": pick(rd, "close"),
                        "verbatim": rd})
            except Exception as e:  # noqa: BLE001
                entry["errors"].append(
                    f"get_market_data({dtype}): "
                    f"{type(e).__name__}: {e}")
        oracle["securities"][symbol] = entry
        oracle["raw"][symbol] = raw
    return oracle


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--out", required=True)
    ap.add_argument("--skip-download", action="store_true")
    ap.add_argument("--skip-tq", action="store_true")
    args = ap.parse_args()
    os.makedirs(args.out, exist_ok=True)

    run = {"schema": "issue217.gen3-acquisition.v1",
           "started_utc": utcnow(), "python": sys.version,
           "cwd": os.getcwd()}

    run["files"] = ({"status": "SKIPPED"} if args.skip_download
                    else step_download(args.out))
    run["tq_oracle"] = ({"status": "SKIPPED"} if args.skip_tq
                        else step_tq(args.out))
    run["boundary_note_template"] = {
        "issue": 71,
        "instruction": "operator writes the FILE_ACCESS_USE boundary "
                       "decision verbatim into boundary-note.md "
                       "(scope/frequency/manner of long-running server "
                       "acquisition and use of official files)"}
    with open(os.path.join(args.out, "acquisition.json"), "w") as f:
        json.dump(run, f, ensure_ascii=False, indent=1)
    with open(os.path.join(args.out, "boundary-note.md"), "w") as f:
        f.write("# Issue #71 FILE_ACCESS_USE boundary decision\n\n"
                "Operator: record the approved scope, frequency and "
                "manner for the long-running server to acquire and use "
                "official files. Verbatim; no inference.\n")
    print(json.dumps({"out": args.out,
                      "files": {k: (v.get("sha256") or v.get("status"))
                                for k, v in run["files"].items()
                                if isinstance(v, dict)},
                      "tq_status": run["tq_oracle"].get("status", "ok")},
                     indent=1))


if __name__ == "__main__":
    main()
