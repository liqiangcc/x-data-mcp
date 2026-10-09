import re, sys, json, collections
path, out = sys.argv[1], sys.argv[2]
pat = re.compile(r"data_service\s+(\d+)\s+\[\d+\]\s+(\d+\.\d+):\s+xp011:(\w+):")
# event name -> func
calls = []            # list of call dicts
open_stack = {}       # tid -> list of frames
cur_call = None
def newcall(ts):
    return {"t0": ts, "func": collections.OrderedDict(), "events": 0}
for line in open(path, errors="replace"):
    m = pat.search(line)
    if not m: continue
    tid, ts, ev = int(m.group(1)), float(m.group(2)), m.group(3)
    st = open_stack.setdefault(tid, [])
    is_ret = ev.endswith("_ret")
    fn = ev[:-4] if is_ret else ev
    if not is_ret:
        st.append([fn, ts, 0.0])
        if fn == "progress":
            cur_call = newcall(ts); calls.append({"tid": tid, "call": cur_call})
            cur_call["tid"] = tid
    else:
        # pop nearest open frame with same fn
        idx = None
        for i in range(len(st)-1, -1, -1):
            if st[i][0] == fn:
                idx = i; break
        if idx is None: continue
        f = st.pop(idx)
        dur = ts - f[1]
        excl = dur - f[2]
        # charge dur to parent's child time
        if idx-1 >= 0 and len(st) > idx-1:
            st[idx-1][2] += dur
        tgt = cur_call["func"].setdefault(fn, {"n":0,"incl":0.0,"excl":0.0}) if cur_call else None
        if tgt is not None:
            tgt["n"] += 1; tgt["incl"] += dur; tgt["excl"] += excl
        if fn == "progress" and cur_call:
            cur_call["t1"] = ts
            cur_call = None
# emit
res = []
for c in calls:
    cl = c["call"]
    if "t1" not in cl: continue
    row = {"tid": c["tid"], "span_s": round(cl["t1"]-cl["t0"],3),
           "funcs": {k: {"n": v["n"], "incl_ms": round(v["incl"]*1000,1), "excl_ms": round(v["excl"]*1000,1)} for k,v in cl["func"].items()}}
    res.append(row)
json.dump(res, open(out,"w"), indent=1)
print(json.dumps([{ "span_s": r["span_s"], "funcs": {k:v["n"] for k,v in r["funcs"].items()}} for r in res], indent=0)[:3000])
