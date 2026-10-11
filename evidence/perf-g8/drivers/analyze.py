import re, sys, json, collections
# XP-011 uprobe trace analyzer (rev.3, round-3 rework).
#
# Attribution is keyed by tid: a `progress` enter opens a call for THAT
# handler thread only (current_calls[tid]); every event resolves its
# charging target via current_calls.get(tid); a `progress` return closes
# only that tid's call. Events on threads without an open progress call
# (background work, e.g. housekeeping) are paired on their own stack but
# never charged into another handler's call.
#
# Pairing discipline: a return must match the TOP frame of its own
# thread's stack. A return with an empty stack or a non-top match means
# lost/out-of-order events; the affected call is marked invalid
# (measurement=NOT_MEASURED) instead of being silently paired with a
# deeper frame. Calls still open at EOF are unterminated -> invalid.
# Exact totals of every anomaly class are reported in summary.
#
# Round-3 additions (Owner "Completed Parser Review"):
# - Non-event loss diagnostics are recognized: any line carrying a
#   LOST / PERF_RECORD_LOST / PERF_RECORD_LOST_SAMPLES marker is counted
#   in lost_marker_lines, and a capture containing ANY such diagnostic
#   invalidates ALL of its calls (including already-closed calls) —
#   exact totals are not derivable when events were dropped.
# - Timestamps moving backward on a tid, and inclusive/exclusive
#   durations negative beyond timestamp rounding (EPS), invalidate the
#   affected call; sub-EPS negatives are clamped to zero.
path, out = sys.argv[1], sys.argv[2]
pat = re.compile(r"data_service\s+(\d+)\s+\[\d+\]\s+(\d+\.\d+):\s+xp011:(\w+):")
lost_pat = re.compile(r"PERF_RECORD_LOST|\bLOST\b")
EPS = 1e-6                    # trace timestamps carry 6 decimals (~us)

calls = []                    # opened calls in order: {"tid": int, "call": dict}
open_stack = {}               # tid -> [[fn, enter_ts, child_time], ...]
current_calls = {}            # tid -> active call dict
last_ts = {}                  # tid -> timestamp of last event on that tid
stats = collections.Counter()
saw_loss = False

def newcall(ts):
    return {"t0": ts, "func": collections.OrderedDict(), "invalid": False}

def invalidate(tid):
    call = current_calls.get(tid)
    if call is not None:
        call["invalid"] = True

for line in open(path, errors="replace"):
    if lost_pat.search(line):
        stats["lost_marker_lines"] += 1
        saw_loss = True
    m = pat.search(line)
    if not m:
        continue
    stats["events"] += 1
    tid, ts, ev = int(m.group(1)), float(m.group(2)), m.group(3)
    prev = last_ts.get(tid)
    if prev is not None and ts < prev - EPS:
        stats["backward_timestamps"] += 1
        invalidate(tid)
    last_ts[tid] = ts
    st = open_stack.setdefault(tid, [])
    is_ret = ev.endswith("_ret")
    fn = ev[:-4] if is_ret else ev
    if not is_ret:
        if fn == "progress":
            if current_calls.get(tid) is not None:
                stats["overlapping_progress_enter"] += 1
                invalidate(tid)
            call = newcall(ts)
            calls.append({"tid": tid, "call": call})
            current_calls[tid] = call
        st.append([fn, ts, 0.0])
        continue
    # return: pair ONLY with the top frame of this tid's stack
    if not st:
        stats["unmatched_returns"] += 1
        invalidate(tid)
        continue
    if st[-1][0] != fn:
        stats["non_top_returns"] += 1
        invalidate(tid)
        continue
    f = st.pop()
    dur = ts - f[1]
    excl = dur - f[2]
    if dur < -EPS or excl < -EPS:
        stats["negative_durations"] += 1
        invalidate(tid)
    dur = max(dur, 0.0)
    excl = max(excl, 0.0)
    if st:
        st[-1][2] += dur
    call = current_calls.get(tid)
    if call is not None:
        tgt = call["func"].setdefault(fn, {"n": 0, "incl": 0.0, "excl": 0.0})
        tgt["n"] += 1
        tgt["incl"] += dur
        tgt["excl"] += excl
    if fn == "progress":
        if call is None:
            stats["unmatched_returns"] += 1
        else:
            call["t1"] = ts
            current_calls.pop(tid, None)

if saw_loss:
    # Lost-event diagnostics make exact call totals underivable for the
    # WHOLE capture — every call, closed or open, is invalidated.
    stats["capture_loss_invalidated"] = len(calls)
    for c in calls:
        c["call"]["invalid"] = True

res = []
for c in calls:
    cl = c["call"]
    if "t1" not in cl:
        stats["unterminated_calls"] += 1
        cl["invalid"] = True
    row = {"tid": c["tid"], "valid": not cl["invalid"]}
    row["span_s"] = round(cl["t1"] - cl["t0"], 3) if "t1" in cl else None
    row["funcs"] = {
        k: {"n": v["n"], "incl_ms": round(v["incl"] * 1000, 1),
            "excl_ms": round(v["excl"] * 1000, 1)}
        for k, v in cl["func"].items()
    }
    if cl["invalid"]:
        stats["invalid_calls"] += 1
        row["measurement"] = "NOT_MEASURED"
    res.append(row)

open_frames = {str(t): len(s) for t, s in open_stack.items() if s}
summary = {
    "events": stats["events"],
    "calls": len(res),
    "calls_valid": sum(1 for r in res if r["valid"]),
    "calls_invalid": stats["invalid_calls"],
    "unterminated_calls": stats["unterminated_calls"],
    "unmatched_returns": stats["unmatched_returns"],
    "non_top_returns": stats["non_top_returns"],
    "overlapping_progress_enter": stats["overlapping_progress_enter"],
    "lost_marker_lines": stats["lost_marker_lines"],
    "capture_loss_invalidated": stats["capture_loss_invalidated"],
    "backward_timestamps": stats["backward_timestamps"],
    "negative_durations": stats["negative_durations"],
    "open_frames_at_eof": open_frames,
}
json.dump({"calls": res, "summary": summary}, open(out, "w"), indent=1)
print(json.dumps({
    "calls": [
        {"tid": r["tid"], "span_s": r["span_s"], "valid": r["valid"],
         "funcs": {k: v["n"] for k, v in r["funcs"].items()}}
        for r in res
    ],
    "summary": summary,
}, indent=0)[:4000])
