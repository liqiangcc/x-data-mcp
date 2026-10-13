#!/usr/bin/env python3
"""Build the S5 "old generation" input: drop the trailing K 32-byte records
from every .day member of a hsjday.zip (parameterised successor of
drivers/make_trunc_hsjday.py, which hard-codes paths and K=1). base.zip and
gbbq.zip are copied byte-identically. Writes SHA256SUMS and trunc-stats.json.
The source and any existing destination must be given explicitly; the
forbidden roots are refused."""
import argparse, hashlib, json, os, shutil, struct, sys, zipfile
from collections import Counter

FORBIDDEN = ("/opt/x-data-mcp-preview", "/root/x-perf-g8", "/root/x-data-mcp-owner", "/tmp")


def check(path: str) -> str:
    real = os.path.realpath(path)
    for f in FORBIDDEN:
        if real == f or real.startswith(f.rstrip("/") + "/") or (f == "/root/x-perf-g8" and real.startswith(f)):
            raise SystemExit(f"refused path {path}")
    return real


def sha256_file(p: str) -> str:
    h = hashlib.sha256()
    with open(p, "rb") as fh:
        for b in iter(lambda: fh.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def truncate(src: str, dst: str, k: int, keep=()) -> dict:
    keep = {n.lower() for n in keep}
    stats = {"k": k, "kept_untruncated": sorted(keep), "members": 0, "day_members": 0, "truncated": 0, "bytes_removed": 0,
             "day_not_mod32": [], "day_too_small": [], "non_day": [],
             "src_last_dates": Counter(), "dst_last_dates": Counter()}
    def last_date(data: bytes) -> int:
        return struct.unpack_from("<I", data, len(data) - 32)[0]
    with zipfile.ZipFile(src) as zin, zipfile.ZipFile(dst, "w", zipfile.ZIP_DEFLATED) as zout:
        for info in zin.infolist():
            stats["members"] += 1
            data = zin.read(info.filename)
            if info.filename.replace("\\", "/").lower().endswith(".day"):
                stats["day_members"] += 1
                if len(data) % 32 != 0:
                    stats["day_not_mod32"].append(info.filename)
                elif info.filename.replace("\\", "/").lower() in keep:
                    pass
                elif len(data) <= 32 * k:
                    stats["day_too_small"].append(info.filename)
                else:
                    stats["src_last_dates"][last_date(data)] += 1
                    data = data[: -32 * k]
                    stats["dst_last_dates"][last_date(data)] += 1
                    stats["truncated"] += 1
                    stats["bytes_removed"] += 32 * k
            else:
                stats["non_day"].append(info.filename)
            zi = zipfile.ZipInfo(info.filename, date_time=info.date_time)
            zi.external_attr = info.external_attr
            zi.compress_type = zipfile.ZIP_DEFLATED
            zout.writestr(zi, data)
    for key in ("src_last_dates", "dst_last_dates"):
        stats[key] = dict(sorted(stats[key].items(), key=lambda kv: -kv[1])[:8])
    return stats


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--src-dir", required=True)
    ap.add_argument("--dst-dir", required=True)
    ap.add_argument("--k", type=int, required=True)
    ap.add_argument("--keep-member", action="append", default=[],
                    help="member (e.g. sh/lday/sh880312.day) left untruncated; used to keep the "
                         "acquire recency probe (freshest sampled bar within 14 days of the "
                         "acquisition date) satisfiable. Choose members outside the catalog scope.")
    a = ap.parse_args()
    src, dst = check(a.src_dir), check(a.dst_dir)
    if a.k < 1 or os.path.exists(dst) and os.listdir(dst):
        raise SystemExit("k must be >=1 and dst-dir must be empty/nonexistent")
    os.makedirs(dst, mode=0o700, exist_ok=True)
    for n in ("base.zip", "gbbq.zip"):
        shutil.copy2(os.path.join(src, n), os.path.join(dst, n))
    stats = truncate(os.path.join(src, "hsjday.zip"), os.path.join(dst, "hsjday.zip"), a.k, a.keep_member)
    with open(os.path.join(dst, "trunc-stats.json"), "w") as fh:
        json.dump(stats, fh, indent=1)
    sums = {n: sha256_file(os.path.join(dst, n)) for n in ("base.zip", "gbbq.zip", "hsjday.zip")}
    with open(os.path.join(dst, "SHA256SUMS"), "w") as fh:
        for n, s in sums.items():
            fh.write(f"{s}  {n}\n")
    print(json.dumps({"sums": sums, "stats": {k: v for k, v in stats.items() if not isinstance(v, list)}}, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
