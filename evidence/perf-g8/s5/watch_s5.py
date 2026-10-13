#!/usr/bin/env python3
"""Stop-line watcher for one s5_run.py process (user-approved rules, GO #4).

usage: watch_s5.py --pid PID --out-dir S5_OUT_DIR --log FILE --stop-file FILE \
    [--root /] [--kill-pattern SUBSTR]
Phase is inferred from the files s5_run writes into --out-dir:
  acquire phase = no s5-gena.json yet, OR s5-yearly-first.json exists and s5-genb.json not yet.
Rules (any hit -> stop the s5_run pid, write the reason to --stop-file):
  acquire phase : MemAvailable < 250MB | cumulative swap-out > 1GB (this phase) | swap-in > 10MB/s x3 samples
  other phases  : MemAvailable < 300MB | swap-out > 20MB/s x3 samples
  always        : SwapFree < 4GB | free disk < 5GB on --root
Samples every 2 s. Writes per-phase minimum MemAvailable to <log>.min on exit.

Stop escalation (2026-10-13 fix): SIGINT alone was a no-op when s5_run was
launched as a shell background job (POSIX: SIGINT inherited SIG_IGN, so
Python never installed a KeyboardInterrupt handler — the r5 watch fired,
the run sailed on). Now: SIGINT -> wait --int-grace s -> SIGTERM to the
pid + its /proc descendants + any --kill-pattern cmdline match -> wait
--term-grace s -> SIGKILL, then a final pattern sweep for reparented
strays. Pass --kill-pattern with the stack root (e.g. /path/stack/r5/)
so children reparented to init are still caught; keep it stack-specific
or it can hit unrelated services.
"""
import argparse, os, signal, subprocess, time


def kv(name, f="/proc/meminfo"):
    for line in open(f):
        if line.startswith(name):
            return int(line.split()[1])


def _alive(pid):
    # signal-0 alone is not enough: an unreaped zombie still accepts it.
    try:
        os.kill(pid, 0)
    except OSError:
        return False
    try:
        with open("/proc/%d/stat" % pid) as f:
            return f.read().rsplit(")", 1)[1].split()[0] != "Z"
    except OSError:
        return False


def descendants(root_pid):
    """All descendant pids of root_pid via /proc ppid links (any depth)."""
    pp = {}
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        try:
            with open("/proc/%s/stat" % d) as f:
                pp[int(d)] = int(f.read().rsplit(")", 1)[1].split()[1])
        except (OSError, IndexError, ValueError):
            continue
    out, frontier = [], [root_pid]
    while frontier:
        cur = frontier.pop()
        kids = [p for p, q in pp.items() if q == cur]
        out.extend(kids)
        frontier.extend(kids)
    return out


def pattern_pids(pattern, exclude=()):
    """Pids whose /proc cmdline contains `pattern` (never self/excluded)."""
    me, skip = os.getpid(), set(exclude)
    hits = []
    for d in os.listdir("/proc"):
        if not d.isdigit():
            continue
        p = int(d)
        if p == me or p in skip:
            continue
        try:
            with open("/proc/%s/cmdline" % d, "rb") as f:
                if pattern.encode() in f.read():
                    hits.append(p)
        except OSError:
            continue
    return hits


def terminate(pid, pattern=None, log=None, int_grace=5.0, term_grace=10.0):
    """SIGINT -> int_grace -> SIGTERM -> term_grace -> SIGKILL.

    Escalates on the watched pid only; SIGTERM/SIGKILL volleys also cover
    the pid's descendants and --kill-pattern matches (stack children
    reparented to init once s5_run dies). Returns the level that stopped
    the pid: already-dead | sigint | sigterm | sigkill | alive."""
    def say(msg):
        if log:
            log.write("%.0f terminate: %s\n" % (time.time(), msg))
            log.flush()

    def targets():
        t = {pid}
        t.update(descendants(pid))
        if pattern:
            t.update(pattern_pids(pattern, exclude=t))
        return sorted(p for p in t if _alive(p))

    def volley(sig):
        ts = targets()
        for p in ts:
            try:
                os.kill(p, sig)
            except OSError:
                pass
        return ts

    if not _alive(pid):
        return "already-dead"
    try:
        os.kill(pid, signal.SIGINT)
    except OSError:
        return "already-dead"
    deadline = time.time() + int_grace
    while time.time() < deadline and _alive(pid):
        time.sleep(0.2)
    level = "sigint"
    if _alive(pid):
        ts = volley(signal.SIGTERM)
        say("SIGINT ignored %.0fs; SIGTERM -> %s" % (int_grace, ts))
        deadline = time.time() + term_grace
        while time.time() < deadline and _alive(pid):
            time.sleep(0.5)
        level = "sigterm"
    if _alive(pid):
        ts = volley(signal.SIGKILL)
        say("SIGTERM ignored %.0fs; SIGKILL -> %s" % (term_grace, ts))
        time.sleep(0.5)
        level = "sigkill" if not _alive(pid) else "alive"
    if pattern:
        strays = [p for p in pattern_pids(pattern, exclude={pid}) if _alive(p)]
        for p in strays:
            try:
                os.kill(p, signal.SIGKILL)
            except OSError:
                pass
        if strays:
            say("final pattern sweep SIGKILL -> %s" % strays)
    return level


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--pid", type=int, required=True)
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--log", required=True)
    ap.add_argument("--stop-file", required=True)
    ap.add_argument("--root", default="/root")
    ap.add_argument("--kill-pattern", default=None,
                    help="cmdline substring for stack children cleanup "
                         "(e.g. the stack root path); SIGTERM/SIGKILL volleys "
                         "and the final sweep cover matches")
    ap.add_argument("--int-grace", type=float, default=5.0)
    ap.add_argument("--term-grace", type=float, default=10.0)
    a = ap.parse_args()

    def alive():
        return _alive(a.pid)

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
            level = terminate(a.pid, a.kill_pattern, log, a.int_grace, a.term_grace)
            log.write(f"{now:.0f} stop-line '{why}': terminate -> {level}\n")
            log.flush()
            break
    open(a.log + ".min", "w").write(str(mn) + "\n")


if __name__ == "__main__":
    main()
