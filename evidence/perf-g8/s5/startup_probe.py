#!/usr/bin/env python3
"""Cold/warm data_service startup-check probe (schema-21). WRITE-ONLY kit —
not run by its author.

Answers: does the cold-cache startup check (~11–13 s measured on the
S2b synthetic fixture, OWNER-ACCEPT-LATEST-ONLY-S2) exceed the tunnel's
10 s wait?

Per run: copy the DB into a private --work-dir, for cold runs
`posix_fadvise(DONTNEED)` the copy, spawn `data_service <db> <sock>`,
and time until the unix socket accepts a connection. Warm runs skip the
drop. Service is SIGTERMed and reaped between runs.

usage:
  startup_probe.py --db SRC --work-dir DIR --data-service BIN \
      --gen-root DIR --sock PATH --out JSON [--runs N]
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from s5_common import guard_paths, utc_now, write_json


def drop_caches_file(path: str) -> None:
    fd = os.open(path, os.O_RDONLY)
    try:
        # dirty pages from the fresh copy are not droppable until written back
        os.fsync(fd)
        os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
    finally:
        os.close(fd)


def wait_connectable(sock_path: str, proc: subprocess.Popen,
                     timeout_s: float = 180.0) -> float:
    t0 = time.monotonic()
    while True:
        if proc.poll() is not None:
            raise RuntimeError(
                f"data_service exited {proc.returncode} before socket connectable")
        if os.path.exists(sock_path):
            try:
                s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                s.connect(sock_path)
                s.close()
                return time.monotonic() - t0
            except OSError:
                pass
        if time.monotonic() - t0 > timeout_s:
            raise RuntimeError(f"socket {sock_path} not connectable in {timeout_s}s")
        time.sleep(0.05)


def one_run(idx: int, cold: bool, db_copy: str, sock: str, ds_bin: str,
            gen_root: str, log) -> dict:
    rec = {"run": idx, "mode": "cold" if cold else "warm"}
    if cold:
        t0 = time.monotonic()
        drop_caches_file(db_copy)
        rec["fadvise_s"] = round(time.monotonic() - t0, 3)
    if os.path.exists(sock):
        os.remove(sock)
    env = dict(os.environ)
    env.pop("X_DATA_MCP_STAGE_TRACE", None)
    env["X_DATA_MCP_GENERATION_ROOT"] = gen_root
    t0 = time.monotonic()
    proc = subprocess.Popen([ds_bin, db_copy, sock], env=env,
                            stdout=log, stderr=subprocess.STDOUT)
    try:
        rec["pid"] = proc.pid
        rec["exe"] = os.readlink(f"/proc/{proc.pid}/exe")
        rec["startup_s"] = round(wait_connectable(sock, proc), 3)
        rec["monotonic_start_ms"] = round(t0 * 1000, 1)
    finally:
        if proc.poll() is None:
            proc.send_signal(signal.SIGTERM)
            try:
                proc.wait(timeout=30)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
        rec["exit_code"] = proc.returncode
    return rec


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--db", required=True,
                    help="source DB to copy (must already be a private copy; "
                         "forbidden roots are refused for both source and copy)")
    ap.add_argument("--work-dir", required=True)
    ap.add_argument("--data-service", required=True)
    ap.add_argument("--gen-root", required=True)
    ap.add_argument("--sock", required=True, help="probe socket path")
    ap.add_argument("--out", required=True)
    ap.add_argument("--use-db-in-place", action="store_true",
                    help="treat --db as the private copy itself (no extra copy; for low-disk "
                         "windows). The db is modified by the probe runs; do not point it at a "
                         "result you must keep untouched.")
    ap.add_argument("--runs", type=int, default=1,
                    help="cold runs; each cold run is followed by one warm run")
    args = ap.parse_args()

    if not hasattr(os, "posix_fadvise"):
        sys.exit("posix_fadvise unavailable on this platform")

    safe = guard_paths({"db": args.db, "work_dir": args.work_dir,
                        "data_service": args.data_service,
                        "gen_root": args.gen_root, "sock": args.sock,
                        "out": args.out})
    work = Path(safe["work_dir"])
    work.mkdir(parents=True, exist_ok=True)
    os.chmod(work, 0o700)  # the service refuses a non-private runtime/socket directory
    db_copy = safe["db"] if args.use_db_in_place else str(work / "probe.sqlite")

    out = {"started_at": utc_now(), "source_db": safe["db"],
           "runs": [], "note": "cold = posix_fadvise(DONTNEED) on the private copy before start"}
    log_path = work / "probe-service.log"
    for i in range(args.runs):
        t0 = time.monotonic()
        if not args.use_db_in_place:
            for ext in ("", "-wal", "-shm"):
                if os.path.exists(db_copy + ext):
                    os.remove(db_copy + ext)
            shutil.copy2(safe["db"], db_copy)
            os.chmod(db_copy, 0o600)
        copy_s = time.monotonic() - t0
        with open(log_path, "ab") as log:
            cold = one_run(2 * i, True, db_copy, safe["sock"],
                           safe["data_service"], safe["gen_root"], log)
            cold["copy_s"] = round(copy_s, 2)
            out["runs"].append(cold)
            warm = one_run(2 * i + 1, False, db_copy, safe["sock"],
                           safe["data_service"], safe["gen_root"], log)
            out["runs"].append(warm)
    out["finished_at"] = utc_now()
    write_json(safe["out"], out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
