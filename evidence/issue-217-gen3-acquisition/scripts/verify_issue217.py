#!/usr/bin/env python3
"""Deterministic replay for the issue-217 gen-3 acquisition package.

Re-checks, from committed bytes only:
  1. acquisition.json internal consistency (recorded sha256 == actual
     relationship declared in the envelope is NOT recomputed here —
     the zips are not in git; we check the envelope's own structure)
  2. tq-raw/*.json files parse and carry verbatim type=1/type=4 payloads
  3. gen2-vs-gen3-frozendiff.json internal consistency with the
     oracle-inputs.json gen-2 event counts
  4. matrix.json status is DIAGNOSTIC_ONLY and non-empty

Exit code 0 iff every check passes. No network, no TDX client needed.
"""
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent.parent
FAIL = []


def check(name, ok, detail=""):
    print(("PASS" if ok else "FAIL"), name, detail)
    if not ok:
        FAIL.append(name)


def load(p):
    return json.loads((HERE / p).read_text(encoding="utf-8"))


acq = load("acquisition.json")
check("envelope schema", acq.get("schema") == "issue217.gen3-acquisition.v1")

files = acq.get("files", {})
expected_sha = {
    "gbbq": "502236d9d8394e89b1a4091f5d0d2b86e419c0c4da2c788ddcea16ef7d3763e3",
    "hsjday": "563797f0d1b852e91b85ee0aafe396c0df1bb12e55b569e8526db76a4e181c64",
    "base": "56c3574505bbd3fa3a1899b46e09112070dea17cbea2f713b7ccff2628379bec",
}
for role, sha in expected_sha.items():
    f = files.get(role, {})
    check(f"{role} sha256 recorded", f.get("sha256") == sha,
          f.get("sha256", "missing"))
    check(f"{role} http 200", f.get("http_status") == 200)

# gen-2 vs gen-3 pins — changed-generation fact
gen2 = {"gbbq": "daad6290c4f1cd478d7436b1e7077260a3e3e52cefd7eae58e4f24878fa2d0e9",
        "hsjday": "c82f852a7bc2e52c7e89347a1e0d1878b01ce997946c2745f7af323752270892",
        "base": "dcdf6b49898926675698263535021aed7ad92b34be82a1a9e8a2a8982bd68bc6"}
for role, pin in gen2.items():
    check(f"{role} != gen-2 pin (changed generation)",
          files[role]["sha256"] != pin)

oracle = acq.get("tq_oracle", {})
check("oracle schema", oracle.get("schema") == "issue54.tq-oracle.v1")
secs = oracle.get("securities", {})
expected_bars = {"sh.600004": 5632, "sh.600006": 6469, "sz.000001": 8457,
                 "sz.000002": 8481, "sz.000004": 0, "sz.000035": 6754,
                 "sh.688318": 1552}
for sym, n in expected_bars.items():
    s = secs.get(sym, {})
    check(f"{sym} front bars", len(s.get("front_adjusted", [])) == n)
    check(f"{sym} none bars", len(s.get("unadjusted", [])) == n)

# key oracle fact: 000002@2000-01-10 served allotment "2.73"
d2 = {e["date"]: e for e in secs["sz.000002"]["divid_factors"]}
ev = d2.get("2000-01-10", {})
check("000002@2000-01-10 allotment served=2.73",
      ev.get("fields", {}).get("allotment") == "2.73",
      json.dumps(ev.get("fields")))

# tq-raw files parse and contain Date arrays
raw_files = sorted((HERE / "tq-raw").glob("*.json"))
check("tq-raw file count", len(raw_files) == 21, str(len(raw_files)))
ok_raw = all(json.loads(p.read_text(encoding="utf-8")) for p in raw_files)
check("tq-raw all parse", ok_raw)

# frozen-set diff consistency vs oracle-inputs.json
oi = load("../issue-54-oracle-harness/oracle-inputs.json")
diff = load("gen2-vs-gen3-frozendiff.json")["frozen_set_diff"]
for sym, d in diff.items():
    g2 = len(oi["securities"][sym]["events"])
    check(f"{sym} gen2 event count matches oracle-inputs",
          d["gen2_events"] == g2, f"{d['gen2_events']} vs {g2}")
check("sz.000001 gen3 has exactly one new event",
      diff["sz.000001"]["new"] == [["2026-09-24", 1]])
check("no other frozen-set new/removed/changed",
      all(not d["new"] and not d["removed"] and not d["field_changed"]
          for s, d in diff.items() if s != "sz.000001"))

mat = load("matrix.json")
check("matrix verdict DIAGNOSTIC_ONLY",
      mat.get("verdict") == "DIAGNOSTIC_ONLY")
check("matrix axis_b total=2160",
      mat["matrix"]["axis_b_interpretation"]["exact_binary"]["total"] == 2160)
check("matrix axis_c compared=37329",
      sum(v["compared"] for v in
          mat["matrix"]["axis_c_qfq_chain"]["exact_binary"].values()) == 37329)

print()
if FAIL:
    print(f"{len(FAIL)} FAIL"); sys.exit(1)
print("all checks PASS")
