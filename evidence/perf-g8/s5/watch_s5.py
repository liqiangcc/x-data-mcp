#!/usr/bin/env python3
"""Stop-line watcher for one s5_run.py process (user-approved rules, GO #4).

usage: watch_s5.py --pid PID --out-dir S5_OUT_DIR --log FILE --stop-file FILE [--root /]
Phase is inferred from the files s5_run writes into --out-dir:
  acquire phase = no s5-gena.json yet, OR s5-yearly-first.json exists and s5-genb.json not yet.
Rules (any hit -> SIGINT the s5_run pid, write the reason to --stop-file):
  acquire phase : MemAvailable < 250MB | cumulative swap-out > 1GB (this phase) | swap-in > 10MB/s x3 samples
  other phases  : MemAvailable < 300MB | swap-out > 20MB/s x3 samples
  always        : SwapFree < 4GB | free disk < 5GB on --root
Samples every 2 s. Writes per-phase minimum MemAvailable to <log>.min on exit.
"""
import argparse, os, subprocess, time


def kv(name, f="/proc/meminfo"):
    for line in open(f):
        if line.startswith(name):
            return int(line.split()[1])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--pid", type=int, required=True)
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--log", required=True)
    ap.add_argument("--stop-file", required=True)
    ap.add_argument("--root", default="/root")
    a = ap.parse_args()

    def alive():
        try:
            os.kill(a.pid, 0)
            return True
        except OSError:
            return False

    def acq():
        e = lambda n: os.path.exists(os.path.join(a.out_dir, n))
        return (not e("s5-gena.json")) or (e("s5-yearly-first.json") and not e("s5-genb.json"))

    mn = {"acq": 10**9, "other": 10**9}
    so_hits = si_hits = 0
    po, pi, lt = kv("pswpout ", "/proc/vmstat"), kv("pswpin ", "/proc/vmstat"), time.time()
    cum = 0
    prev_acq = False
    log = open(a.log, "a")
    while alive():
        time.sleep(2)
        m = kv("MemAvailable")
        is_acq = acq()
        k = "acq" if is_acq else "other"
        mn[k] = min(mn[k], m)
        now = time.time()
        dt = now - lt
        lt = now
        o, i = kv("pswpout ", "/proc/vmstat"), kv("pswpin ", "/proc/vmstat")
        so, si = (o - po) * 4 / 1024 / dt, (i - pi) * 4 / 1024 / dt
        if is_acq and not prev_acq:
            cum = 0
        if is_acq:
            cum += o - po
        po, pi, prev_acq = o, i, is_acq
        so_hits = so_hits + 1 if so > 20 else 0
        si_hits = si_hits + 1 if si > 10 else 0
        sf = kv("SwapFree")
        d = int(subprocess.check_output(["df", "--output=avail", "-k", a.root]).split()[1])
        cum_mb = cum * 4 / 1024
        log.write(f"{now:.0f} {k} mem={m} swapfree={sf} so={so:.1f} si={si:.1f} cum_out_mb={cum_mb:.0f}\n")
        log.flush()
        why = None
        if m < (250000 if is_acq else 300000):
            why = f"mem {m} kB in {k} phase"
        elif sf < 4 * 1024 * 1024:
            why = f"swapfree {sf}"
        elif d < 5000000:
            why = f"df {d}"
        elif is_acq and cum_mb > 1024:
            why = f"acq cumulative swap-out {cum_mb:.0f}MB"
        elif is_acq and si_hits >= 3:
            why = f"acq swap-in {si:.1f}MB/s x3"
        elif (not is_acq) and so_hits >= 3:
            why = f"swap-out {so:.1f}MB/s x3"
        if why:
            open(a.stop_file, "w").write(why + "\n")
            os.kill(a.pid, 2)
            break
    open(a.log + ".min", "w").write(str(mn) + "\n")


if __name__ == "__main__":
    main()
