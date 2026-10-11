#!/usr/bin/env python3
"""Regression for analyze.py tid-keyed attribution (XP-011 rounds 2-3).

Case A is the Owner-authored interleaved trace: two handler tids plus a
background tid that must contribute to neither call. Expected values are
exact n/incl/excl. Case B drops tid10's cv_fixed return: tid10's
progress_ret then sees cv_fixed on top of its stack, so the call is
invalid (NOT_MEASURED) rather than silently paired.

Round-3 cases (Owner "Completed Parser Review"):
Case C appends a PERF_RECORD_LOST diagnostic line to an otherwise valid
capture — lost events make exact totals underivable, so EVERY call in
the capture (closed ones included) is invalidated: 0 valid calls.
Case D is a progress call whose return (1.0) precedes its enter (2.0):
backward timestamp + negative duration -> NOT_MEASURED.
"""
import json, os, subprocess, sys, tempfile

D = os.path.dirname(os.path.abspath(__file__))
ANALYZE = os.path.join(D, "analyze.py")


def line(tid, ts, ev):
    return f"    data_service {tid} [000] {ts:.6f}: xp011:{ev}: (deadbeef <- cafe)\n"


def run(events):
    with tempfile.NamedTemporaryFile("w", suffix=".trace", delete=False) as f:
        for e in events:
            f.write(e if isinstance(e, str) else line(*e))
        path = f.name
    out = path + ".json"
    subprocess.run([sys.executable, ANALYZE, path, out],
                   check=True, capture_output=True)
    with open(out) as f:
        return json.load(f)


EVENTS = [
    (10, 1.000, "progress"), (10, 1.100, "cv_fixed"),
    (20, 1.200, "progress"), (20, 1.300, "cv_fixed"),
    (30, 1.400, "cv_fixed"), (30, 1.500, "cv_fixed_ret"),
    (10, 1.600, "cv_fixed_ret"), (10, 1.800, "progress_ret"),
    (20, 1.900, "cv_fixed_ret"), (20, 2.000, "progress_ret"),
]

res = run(EVENTS)
assert len(res["calls"]) == 2, res["calls"]
calls = {c["tid"]: c for c in res["calls"]}
assert calls[10]["funcs"]["progress"] == {"n": 1, "incl_ms": 800.0, "excl_ms": 300.0}
assert calls[10]["funcs"]["cv_fixed"] == {"n": 1, "incl_ms": 500.0, "excl_ms": 500.0}
assert calls[20]["funcs"]["progress"] == {"n": 1, "incl_ms": 800.0, "excl_ms": 200.0}
assert calls[20]["funcs"]["cv_fixed"] == {"n": 1, "incl_ms": 600.0, "excl_ms": 600.0}
assert all(c["valid"] for c in res["calls"])
assert res["summary"]["calls_valid"] == 2
assert res["summary"]["calls_invalid"] == 0

# Lost return: tid10's call must be invalidated, tid20 unaffected.
res2 = run([e for e in EVENTS if e != (10, 1.600, "cv_fixed_ret")])
c10 = [c for c in res2["calls"] if c["tid"] == 10][0]
assert c10["valid"] is False and c10["measurement"] == "NOT_MEASURED"
assert res2["summary"]["non_top_returns"] == 1
assert res2["summary"]["unterminated_calls"] == 1
assert res2["summary"]["calls_invalid"] == 1
assert res2["summary"]["calls_valid"] == 1

# Case C: a LOST diagnostic line invalidates the WHOLE capture —
# both already-closed calls included, zero valid calls remain.
LOST_LINE = "    data_service 99 [000] 2.500000: PERF_RECORD_LOST lost 7 samples\n"
for marker in [LOST_LINE,
               "    data_service 99 [000] 2.500000: PERF_RECORD_LOST_SAMPLES lost 7\n",
               "warning: processed 10 events and LOST 2 chunks!\n"]:
    res3 = run(EVENTS + [marker])
    assert res3["summary"]["lost_marker_lines"] == 1, res3["summary"]
    assert res3["summary"]["capture_loss_invalidated"] == 2
    assert res3["summary"]["calls_valid"] == 0
    assert res3["summary"]["calls_invalid"] == 2
    assert all(not c["valid"] and c["measurement"] == "NOT_MEASURED"
               for c in res3["calls"])

# Case D: progress enter=2.0 / return=1.0 — backward timestamp and
# negative duration invalidate the call; it reports NOT_MEASURED.
res4 = run([(10, 2.000, "progress"), (10, 1.000, "progress_ret")])
assert len(res4["calls"]) == 1
c = res4["calls"][0]
assert c["valid"] is False and c["measurement"] == "NOT_MEASURED"
assert res4["summary"]["backward_timestamps"] == 1
assert res4["summary"]["negative_durations"] == 1
assert res4["summary"]["calls_valid"] == 0

print("analyze.py regression PASS")
