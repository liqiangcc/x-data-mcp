#!/usr/bin/env python3
"""Read-only post-rebuild readback (operations tooling; makes NO write calls).
usage: readback_prod.py <yearly-driver-summary.json> [expect.json]
Same socket/output guard as the rebuild drivers (prod_guard): private socket by default, the production
socket only when XP015_PRODUCTION_SOCKET and XP015_SOCKET both equal it exactly. XP015_OUT is required."""
import json, os, socket, sys, time
from datetime import datetime, timezone
from pathlib import Path
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from prod_guard import socket_path, out_dir

SOCK, MODE = socket_path()
OUT = Path(out_dir(MODE)) / os.environ.get("G8_RUN_ID", "deploy31-readback")
OUT = OUT.with_suffix(".json")
YEARLY = json.load(open(sys.argv[1]))
EXPECT = json.load(open(sys.argv[2])) if len(sys.argv) > 2 else {
    "catalog_entities": 5228, "daily_published": 5224, "yearly_succeeded": 5223,
    "absent": ["sz.001381", "sz.301569", "sz.301660", "sz.301718"], "empty": ["sz.001246"],
    "samples": ["sh.600000", "sz.000001", "sh.601398", "sz.300750", "sh.688981"]}
READ_ONLY = {"list_data_versions", "query_data", "get_data_availability", "get_data_capabilities"}

class Mcp:
    def __init__(self, path):
        self.s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM); self.s.connect(path)
        self.f = self.s.makefile("r", encoding="utf-8"); self.n = 0
    def call(self, name, args):
        assert name in READ_ONLY, f"read-only script refuses {name}"
        self.n += 1
        self.s.sendall((json.dumps({"jsonrpc": "2.0", "id": self.n, "method": "tools/call", "params": {"name": name, "arguments": args}}, separators=(",", ":")) + "\n").encode())
        while True:
            line = self.f.readline()
            if not line: raise RuntimeError(f"{name} EOF")
            r = json.loads(line)
            if r.get("id") != self.n: continue
            if "error" in r: raise RuntimeError(f"{name} rpc {r['error']}")
            res = r["result"]
            if res.get("isError"): raise RuntimeError(f"{name} tool {json.dumps(res.get('structuredContent') or res.get('content'))[:300]}")
            return res.get("structuredContent") or {}

m = Mcp(SOCK)
res = {"mode": MODE, "socket": SOCK, "utc": datetime.now(timezone.utc).isoformat(), "checks": {}}
sel = {"kind": "catalog", "scope": "current_sh_sz_a"}
ver = m.call("list_data_versions", {"dataset": "securities.catalog", "contract_version": "v1", "selection": sel, "page_size": 10})["items"][0]["version_ref"]["version_id"]
recs, cur = [], None
while True:
    a = {"dataset": "securities.catalog", "contract_version": "v1", "selection": sel, "filter": {"kind": "catalog"},
         "version": {"kind": "catalog_version", "version_id": ver}, "page_size": 1000}
    if cur: a["cursor"] = cur
    p = m.call("query_data", a); recs += p.get("records") or []; cur = p.get("next_cursor")
    if not cur: break
ents, seen = [], set()
for r in recs:
    e = r.get("entity") or {}; k = (e.get("exchange"), e.get("code"))
    if k[0] and k[1] and k not in seen: seen.add(k); ents.append({"exchange": k[0], "code": k[1]})
res["catalog_version"] = ver
daily, absent, last_days = 0, [], {}
yearly_heads = 0
for i in range(0, len(ents), 100):
    chunk = ents[i:i + 100]
    d = m.call("get_data_availability", {"dataset": "market.daily", "contract_version": "v1", "selection": {"kind": "batch", "entities": chunk},
        "filter": {"kind": "daily", "start_date": "1990-12-19", "end_date": "2099-12-31"}, "version": {"kind": "latest_per_entity"}, "page_size": 100})
    for it in d.get("items") or []:
        e = it["entity"]; key = f"{e['exchange']}.{e['code']}"
        if it.get("version_ref") and it.get("last_record"):
            daily += 1; last_days[it["last_record"]] = last_days.get(it["last_record"], 0) + 1
        else: absent.append(key)
    y = m.call("get_data_availability", {"dataset": "market.yearly", "contract_version": "v1", "selection": {"kind": "batch", "entities": chunk},
        "filter": {"kind": "yearly", "start_year": 1900, "end_year": 2099}, "version": {"kind": "latest_per_entity"}, "page_size": 100})
    yearly_heads += sum(1 for it in y.get("items") or [] if it.get("version_ref"))
ysucc = sum(int(c.get("succeeded") or 0) for c in YEARLY.get("chunks", []))
yempty = sorted({f"{x['entity'][0]}.{x['entity'][1]}" if isinstance(x.get("entity"), list) else str(x.get("entity")) for c in YEARLY.get("chunks", []) for x in c.get("empty", [])})
res["observed"] = {"catalog_entities": len(ents), "daily_published": daily, "daily_head_last_day_counts": dict(sorted(last_days.items())),
                   "yearly_heads": yearly_heads, "yearly_succeeded": ysucc, "absent": sorted(absent), "yearly_empty": yempty,
                   "yearly_driver_totals": YEARLY.get("outcome_totals"), "yearly_unqualified": YEARLY.get("unqualified")}
def chk(name, ok, **kw): res["checks"][name] = dict(ok=bool(ok), **kw)
chk("catalog_entities", len(ents) == EXPECT["catalog_entities"], expected=EXPECT["catalog_entities"], observed=len(ents))
chk("daily_published", daily == EXPECT["daily_published"], expected=EXPECT["daily_published"], observed=daily)
chk("yearly_succeeded", ysucc == EXPECT["yearly_succeeded"], expected=EXPECT["yearly_succeeded"], observed=ysucc)
chk("absent_list", sorted(absent) == sorted(EXPECT["absent"]), expected=EXPECT["absent"], observed=sorted(absent))
chk("yearly_empty_list", yempty == sorted(EXPECT["empty"]), expected=EXPECT["empty"], observed=yempty)
chk("daily_head_days_present", bool(last_days), latest=max(last_days) if last_days else None)
samples = []
for sym in EXPECT["samples"]:
    ex, code = sym.split("."); ent = {"exchange": ex, "code": code}; row = {"entity": sym}
    try:
        dl = m.call("query_data", {"dataset": "market.daily", "contract_version": "v1", "selection": {"kind": "single", "entity": ent},
            "filter": {"kind": "daily", "start_date": "2026-09-01", "end_date": "2099-12-31"}, "version": {"kind": "latest_per_entity"}, "page_size": 50})["records"]
        yl = m.call("query_data", {"dataset": "market.yearly", "contract_version": "v1", "selection": {"kind": "single", "entity": ent},
            "filter": {"kind": "yearly", "start_year": 2020, "end_year": 2099}, "version": {"kind": "latest_per_entity"}, "page_size": 50})["records"]
        row.update(daily_records=len(dl), last_daily=(dl[-1]["date"], dl[-1]["close"]) if dl else None, yearly_records=len(yl),
                   last_yearly=(yl[-1]["date"], yl[-1]["close"]) if yl else None, ok=bool(dl and yl))
    except Exception as exc:
        row.update(ok=False, error=str(exc)[:200])
    samples.append(row)
res["samples"] = samples
chk("samples_readable", all(s["ok"] for s in samples), count=len(samples))
res["pass"] = all(v["ok"] for v in res["checks"].values())
OUT.write_text(json.dumps(res, indent=1, ensure_ascii=False))
print(json.dumps({"pass": res["pass"], **{k: v["ok"] for k, v in res["checks"].items()}}))
sys.exit(0 if res["pass"] else 3)
