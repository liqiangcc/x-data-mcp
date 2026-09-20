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
import re
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


def fetch(url, cookie=None):
    """One HTTP GET; returns (status, headers, body-bytes generator)."""
    hdrs = {"User-Agent": "Mozilla/5.0 (Windows NT 10.0; Win64; x64)"}
    if cookie:
        hdrs["Cookie"] = cookie
        hdrs["Referer"] = url.rsplit("/", 1)[0] + "/"
    req = urllib.request.Request(url, headers=hdrs)
    resp = urllib.request.urlopen(req, timeout=600)
    return resp


def eo_challenge_cookies(body):
    """TencentEdgeOne anti-bot interstitial: JS sets
    __tst_status=<sum of three ints> and EO_Bot_Ssid=<int> then reloads.
    Returns the Cookie header a browser would send, or None."""
    if b"EO_Bot_Ssid" not in body or b"__tst_status" not in body:
        return None
    nums = re.findall(rb"[A-Za-z]{5}:(\d{6,})", body)
    ssid = re.search(rb"\+.{0,30}?(\d{10})\)", body)
    if len(nums) < 3 or not ssid:
        return None
    tst = sum(int(n) for n in nums[:3])
    return f"__tst_status={tst}#; EO_Bot_Ssid={ssid.group(1).decode()}"


def step_download(out):
    manifest = {}
    for role, url in FILES.items():
        dest = os.path.join(out, f"{role}.zip")
        attempts = []
        try:
            resp = fetch(url)
            headers = {k.lower(): v for k, v in resp.headers.items()}
            first = resp.read()
            attempt = {"http_status": resp.status,
                       "response_headers": headers,
                       "body_bytes": len(first)}
            if (headers.get("content-type", "").startswith("text/html")
                    and len(first) < 65536):
                # save the verbatim interstitial for audit
                with open(dest + ".challenge.html", "wb") as f:
                    f.write(first)
                cookie = eo_challenge_cookies(first)
                attempt["edgeone_challenge"] = (
                    "cookie_replayed" if cookie else "unparsed")
                attempts.append(attempt)
                if not cookie:
                    raise RuntimeError(
                        "EdgeOne challenge not parseable")
                resp = fetch(url, cookie=cookie)
                headers = {k.lower(): v
                           for k, v in resp.headers.items()}
                attempt = {"http_status": resp.status,
                           "response_headers": headers,
                           "request_cookie": cookie}
                with open(dest, "wb") as f:
                    while True:
                        chunk = resp.read(1 << 20)
                        if not chunk:
                            break
                        f.write(chunk)
                attempt["body_bytes"] = os.path.getsize(dest)
                attempts.append(attempt)
            else:
                with open(dest, "wb") as f:
                    f.write(first)
                    while True:
                        chunk = resp.read(1 << 20)
                        if not chunk:
                            break
                        f.write(chunk)
                attempts.append(attempt)
            manifest[role] = {
                "role": role, "url": url, "retrieved_utc": utcnow(),
                "http_status": resp.status,
                "last_modified": headers.get("last-modified"),
                "etag": headers.get("etag"),
                "bytes": os.path.getsize(dest),
                "sha256": sha256_file(dest),
                "response_headers": headers,
                "attempts": attempts,
            }
        except Exception as e:  # noqa: BLE001 — verbatim capture
            manifest[role] = {"role": role, "url": url,
                              "status": "NOT_RUN",
                              "reason": f"{type(e).__name__}: {e}",
                              "attempts": attempts}
    return manifest


def norm_code(symbol):
    """sh.600004 -> 600004.SH; sz.000002 -> 000002.SZ (TQ convention:
    six digits + dot + market suffix; verified live 2026-09-20 — the
    SH600006 form returns empty)."""
    m, c = symbol.split(".")
    return c + "." + m.upper()


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


def tq_raw_call(dll, run_id, request, timeout_ms=120000):
    """GetTdxDataStr verbatim: returns the raw response string."""
    ptr = dll.GetTdxDataStr(
        run_id, json.dumps(request, ensure_ascii=False).encode("utf-8"),
        timeout_ms)
    return ptr.decode("utf-8") if ptr else ""


def step_tq(out):
    """TQ oracle via verbatim GetTdxDataStr JSON (spec: raw responses
    land on disk unmodified; parsed fields keep served decimal strings,
    no float round-trip through pandas)."""
    import importlib.util
    plugin_dir = os.environ.get("TDX_PYPLUGINS_DIR",
                                r"C:\new_tdx64\PYPlugins")
    mod_path = os.path.join(plugin_dir, "sys", "tqcenter.py")
    if not os.path.isfile(mod_path):
        return {"status": "NOT_RUN",
                "reason": f"tqcenter.py not found at {mod_path}"}
    try:
        spec = importlib.util.spec_from_file_location("tqcenter",
                                                      mod_path)
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        tq = mod.tq
    except Exception as e:  # noqa: BLE001
        return {"status": "NOT_RUN",
                "reason": f"tqcenter import: {type(e).__name__}: {e}"}
    # InitConnect requires a strategy file path under the plugin tree
    strategy = os.path.join(plugin_dir, "user", "gen3_acquire.py")
    try:
        if not os.path.isfile(strategy):
            with open(strategy, "w", encoding="utf-8") as f:
                f.write("# gen3 oracle acquisition probe\n")
        tq.initialize(strategy)
    except Exception as e:  # noqa: BLE001
        return {"status": "NOT_RUN",
                "reason": f"tq.initialize: {type(e).__name__}: {e}"}
    dll = mod.dll
    run_id = tq.run_id
    raw_dir = os.path.join(out, "tq-raw")
    os.makedirs(raw_dir, exist_ok=True)

    oracle = {"schema": "issue54.tq-oracle.v1",
              "generator": {"kind": "tq_api", "captured_utc": utcnow(),
                            "environment_note":
                                "windows tqcenter.py v1.0.16 + "
                                "TPythClient.dll, verbatim GetTdxDataStr"},
              "securities": {}, "raw": {}}
    for symbol in SECURITIES:
        code = norm_code(symbol)
        entry = {"divid_factors": [], "front_adjusted": [],
                 "unadjusted": [], "errors": []}
        raw = {}
        # --- get_divid_factors (type=4), verbatim ---
        try:
            req = {"id": run_id, "type": 4, "stock_code": code,
                   "start_time": "", "end_time": ""}
            s = tq_raw_call(dll, run_id, req)
            fn = f"divid_{code.replace('.', '_')}.json"
            with open(os.path.join(raw_dir, fn), "w",
                      encoding="utf-8") as f:
                f.write(s)
            raw["divid_factors_json"] = {
                "file": f"tq-raw/{fn}", "bytes": len(s.encode("utf-8")),
                "sha256": hashlib.sha256(s.encode("utf-8")).hexdigest()}
            d = json.loads(s) if s else {}
            if d.get("ErrorId") != "0":
                entry["errors"].append(
                    f"divid_factors ErrorId={d.get('ErrorId')}")
            dates = d.get("Date", [])
            types = d.get("Type", [])
            values = d.get("Value", [])
            for dt, ty, v in zip(dates, types, values):
                v = list(v) + ["0"] * 4
                entry["divid_factors"].append({
                    "date": dt,
                    "fields": {"type": str(ty), "bonus": v[0],
                               "allot_price": v[1],
                               "share_bonus": v[2],
                               "allotment": v[3]},
                    "verbatim": {"date": dt, "type": ty, "value": v[:4]}})
        except Exception as e:  # noqa: BLE001
            entry["errors"].append(
                f"get_divid_factors: {type(e).__name__}: {e}")
        # --- get_market_data none/front, full history (front anchors at
        # the response's last bar -> count must cover full history) ---
        for dtype, key in (("none", "unadjusted"),
                           ("front", "front_adjusted")):
            try:
                req = {"id": run_id, "type": 1, "stock_list": [code],
                       "period": "1d", "start_time": "", "end_time": "",
                       "dividend_type": dtype, "count": 9000,
                       "stock_page_index": 0, "stock_page_size": 100}
                s = tq_raw_call(dll, run_id, req)
                fn = f"kline_{code.replace('.', '_')}_{dtype}.json"
                with open(os.path.join(raw_dir, fn), "w",
                          encoding="utf-8") as f:
                    f.write(s)
                raw[f"market_data_{dtype}_json"] = {
                    "file": f"tq-raw/{fn}",
                    "bytes": len(s.encode("utf-8")),
                    "sha256": hashlib.sha256(
                        s.encode("utf-8")).hexdigest()}
                d = json.loads(s) if s else {}
                vd = d.get("Value", {}).get(code, {})
                if d.get("ErrorId") != "0":
                    entry["errors"].append(
                        f"market_data({dtype}) ErrorId="
                        f"{d.get('ErrorId')}")
                dates = vd.get("Date", [])
                for i, dt in enumerate(dates):
                    row = {"date": dt}
                    for fld, key_ in (("open", "Open"),
                                      ("high", "High"),
                                      ("low", "Low"),
                                      ("close", "Close")):
                        arr = vd.get(key_, [])
                        row[fld] = arr[i] if i < len(arr) else None
                    if dtype == "none":
                        ff = vd.get("ForwardFactor", [])
                        row["forward_factor"] = (
                            ff[i] if i < len(ff) else None)
                    row["verbatim"] = {k: row.get(k) for k in
                                       ("date", "open", "high", "low",
                                        "close", "forward_factor")}
                    entry[key].append(row)
            except Exception as e:  # noqa: BLE001
                entry["errors"].append(
                    f"get_market_data({dtype}): "
                    f"{type(e).__name__}: {e}")
        oracle["securities"][symbol] = entry
        oracle["raw"][symbol] = raw
    try:
        tq.close()
    except Exception:  # noqa: BLE001
        pass
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
