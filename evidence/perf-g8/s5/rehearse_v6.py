#!/usr/bin/env python3
"""rehearse_v6.py — pre-deploy rehearsal of prod_rebuild_v6.py on the private
stack. NOT for production; every path is guarded outside production roots.

Sequence (mirrors run_s5_detached.sh, minus the perf phases):
  fresh init21 db -> start data_service + acquire_service from the release
  package -> mirror inputs -> acquire+verify+activate the baseline
  generation (stack setup, outside the driver whitelist) -> write the
  s5-gena.json marker so watch_s5.py reads the rebuild as a non-acquire
  phase -> spawn prod_rebuild_v6.py as a subprocess with the env gate
  exercised for real (PROD_REBUILD_V6_* vars, rehearsal socket).

usage:
  rehearse_v6.py --stack-root DIR --pkg-bin DIR --gen-root DIR \
                 --inputs DIR --out DIR [--request-prefix rv6]
"""
from __future__ import annotations

import argparse
import os
import subprocess
import sys
import time
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import s5_run  # noqa: E402
from s5_common import Mcp, utc_now, write_json  # noqa: E402


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--stack-root", required=True)
    ap.add_argument("--pkg-bin", required=True)
    ap.add_argument("--gen-root", required=True)
    ap.add_argument("--inputs", required=True, help="baseline generation input dir (hsjday.zip etc)")
    ap.add_argument("--out", required=True)
    ap.add_argument("--request-prefix", default="rv6")
    args = ap.parse_args()

    from s5_common import guard_paths
    safe = guard_paths({
        "stack_root": args.stack_root, "pkg_bin": args.pkg_bin,
        "gen_root": args.gen_root, "inputs": args.inputs, "out": args.out,
    })
    out_dir = Path(safe["out"]); out_dir.mkdir(parents=True, exist_ok=True)
    run = {"started_at": utc_now(), "args": vars(args)}

    stack = s5_run.Stack(safe["stack_root"], safe["pkg_bin"], safe["gen_root"],
                         str(Path(safe["stack_root"]) / "mirror"))
    stack.dirs()
    try:
        run["init_out"] = stack.init_db("init21")
        stack.start()
        run["startup"] = stack.wait_sockets()

        # Stack setup: stage the baseline generation via the stack's own
        # acquire service — mirrors production, where the generation already
        # exists and the driver never touches acquire tools.
        installed = s5_run.mirror_inputs(stack.mirror, safe["inputs"])
        acq = Mcp(stack.acq_sock)
        run["gen"] = s5_run.acquire_and_activate(acq, args.request_prefix, "gena")
        run["gen"]["mirror_files"] = installed
        acq.close()
        # watch_s5.py phase inference: s5-gena.json present => the rest of the
        # run is a non-acquire phase (MemAvailable stop-line 300MB).
        write_json(str(out_dir / "s5-gena.json"), run["gen"])

        env = dict(os.environ)
        env["PROD_REBUILD_V6_SOCKET"] = stack.mcp_sock
        env["PROD_REBUILD_V6_DB"] = stack.db
        env["PROD_REBUILD_V6_OUT"] = str(out_dir)
        env["PROD_REBUILD_V6_PREFIX"] = args.request_prefix
        env.pop("PROD_REBUILD_V6_PRODUCTION", None)
        t0 = time.monotonic()
        proc = subprocess.run(
            [sys.executable, os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                          "prod_rebuild_v6.py")],
            env=env)
        run["driver_rc"] = proc.returncode
        run["driver_wall_s"] = round(time.monotonic() - t0, 1)
        run["finished_at"] = utc_now()
        write_json(str(out_dir / "rehearse-run.json"), run)
        return proc.returncode
    except Exception as exc:
        run["error"] = str(exc)
        write_json(str(out_dir / "rehearse-run.json"), run)
        raise
    finally:
        stack.stop()


if __name__ == "__main__":
    sys.exit(main())
