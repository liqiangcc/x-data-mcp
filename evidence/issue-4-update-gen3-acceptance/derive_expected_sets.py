#!/usr/bin/env python3
"""Issue #4 changed-generation update acceptance — S3 expected-set
derivation (PLAN §3).

Mechanically derives, over the pinned gen-2/gen-3 byte pairs:

- S_evt: securities whose decoded gbbq event set differs between
  generations. Both gbbq members are decoded with the SAME pinned
  community decoder (retained-inputs/gbbq_reader.py, sha256 pinned in
  MANIFEST.md). pandas is absent on Box, so the pinned module is imported
  with a minimal `pandas` stand-in that returns the raw record list —
  the pinned file itself is never modified.
- S_day: securities whose hsjday `.day` member differs between the two
  zips (zip member CRC32+size as content discriminator; member name ==
  security file, e.g. sh600004.day).

Emits expected_sets.json with both input sha256s, per-set security
lists, and the union S_chg. Member-name -> symbol mapping:
{sh,sz,bj}{code}.day -> {sh|sz|bj}.{code}; non-.day members and members
whose name does not parse are recorded verbatim, not guessed.
"""
import hashlib
import json
import os
import sys
import types
import zipfile

RETAINED = "/home/box/retained-inputs"
GEN2_DIR = "/home/box/acceptance-run-issue3/snapshot"
GEN3_DIR = "/home/box/retained-inputs/gen3"
DECODER = os.path.join(RETAINED, "gbbq_reader.py")
DECODER_SHA256 = "e28fae76760b1247a960cab285aa357c2d263b3212e7cabfb3f1ebfcc127d6ba"
HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "expected_sets.json")


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def extract_member(zip_path, wanted_suffix):
    """Return the single zip member whose name ends with wanted_suffix."""
    with zipfile.ZipFile(zip_path) as z:
        names = [n for n in z.namelist() if n.lower().endswith(wanted_suffix)]
        if len(names) != 1:
            raise SystemExit(
                f"{zip_path}: expected 1 *{wanted_suffix} member, got {names}")
        return z.read(names[0])


def decode_gbbq_records(member_bytes, tmp_path):
    """Decode a gbbq member with the pinned community decoder.

    Imports the pinned module with a stub `pandas` so `get_df` returns the
    raw record list. The pinned decoder file itself is untouched.
    """
    real_sha = sha256_file(DECODER)
    if real_sha != DECODER_SHA256:
        raise SystemExit(f"pinned decoder mismatch: {real_sha}")
    with open(tmp_path, "wb") as f:
        f.write(member_bytes)

    class _FakeDF(list):
        pass

    fake_pd = types.ModuleType("pandas")
    fake_pd.DataFrame = lambda data=None, columns=None: _FakeDF(data or [])
    sys.modules["pandas"] = fake_pd
    sys.path.insert(0, RETAINED)
    try:
        import gbbq_reader
        rows = gbbq_reader.GbbqReader().get_df(tmp_path)
    finally:
        sys.path.remove(RETAINED)
        sys.modules.pop("pandas", None)
    # row: (market, code, datetime, category, hongli, peigujia, songgu, peigu)
    # market byte mapping per src/official_market.rs: 1 -> sh, 0 -> sz.
    # Unknown bytes are kept verbatim as "<n>.<code>" — recorded, not guessed.
    per_security = {}
    for market, code, dt, cat, h, pj, sg, pg in rows:
        prefix = {1: "sh", 0: "sz"}.get(market, str(market))
        sym = f"{prefix}.{code}".lower()
        per_security.setdefault(sym, []).append(
            [dt, cat, repr(h), repr(pj), repr(sg), repr(pg)])
    return per_security


def day_member_symbols(zip_path):
    """Map member name -> parsed symbol for .day members."""
    out = {}
    skipped = []
    with zipfile.ZipFile(zip_path) as z:
        for info in z.infolist():
            name = info.filename
            # Official zips use Windows-style backslash member paths.
            base = os.path.basename(name.replace("\\", "/")).lower()
            if not base.endswith(".day"):
                skipped.append(name)
                continue
            stem = base[:-4]
            if len(stem) == 8 and stem[:2] in ("sh", "sz", "bj") \
                    and stem[2:].isdigit():
                sym = f"{stem[:2]}.{stem[2:]}"
                out[sym] = (name, info.CRC, info.file_size)
            else:
                skipped.append(name)
    return out, skipped


def main():
    gen2_gbbq = extract_member(os.path.join(GEN2_DIR, "gbbq.zip"), "gbbq")
    gen3_gbbq = extract_member(os.path.join(GEN3_DIR, "gbbq.zip"), "gbbq")
    g2_events = decode_gbbq_records(gen2_gbbq, "/tmp/gen2-gbbq-member")
    g3_events = decode_gbbq_records(gen3_gbbq, "/tmp/gen3-gbbq-member")

    s_evt = sorted(
        s for s in set(g2_events) | set(g3_events)
        if g2_events.get(s) != g3_events.get(s))

    g2_day, g2_skip = day_member_symbols(os.path.join(GEN2_DIR, "hsjday.zip"))
    g3_day, g3_skip = day_member_symbols(os.path.join(GEN3_DIR, "hsjday.zip"))
    s_day = sorted(
        s for s in set(g2_day) | set(g3_day)
        if g2_day.get(s) != g3_day.get(s))

    result = {
        "schema": "issue4.gen3-update-expected-sets.v1",
        "inputs": {
            "gen2_gbbq_zip": sha256_file(os.path.join(GEN2_DIR, "gbbq.zip")),
            "gen3_gbbq_zip": sha256_file(os.path.join(GEN3_DIR, "gbbq.zip")),
            "gen2_hsjday_zip": sha256_file(os.path.join(GEN2_DIR, "hsjday.zip")),
            "gen3_hsjday_zip": sha256_file(os.path.join(GEN3_DIR, "hsjday.zip")),
            "gbbq_decoder_sha256": DECODER_SHA256,
            "gen2_gbbq_member_sha256": hashlib.sha256(gen2_gbbq).hexdigest(),
            "gen3_gbbq_member_sha256": hashlib.sha256(gen3_gbbq).hexdigest(),
            "gen2_gbbq_event_count": sum(len(v) for v in g2_events.values()),
            "gen3_gbbq_event_count": sum(len(v) for v in g3_events.values()),
        },
        "S_evt": s_evt,
        "S_day": s_day,
        "S_chg": sorted(set(s_evt) | set(s_day)),
        "skipped_members": {"gen2": g2_skip, "gen3": g3_skip},
    }
    with open(OUT, "w") as f:
        json.dump(result, f, ensure_ascii=False, indent=1)
    print(json.dumps({
        "S_evt": len(s_evt), "S_day": len(s_day),
        "S_chg": len(result["S_chg"]),
        "gen2_events": result["inputs"]["gen2_gbbq_event_count"],
        "gen3_events": result["inputs"]["gen3_gbbq_event_count"],
        "gen2_day_members": len(g2_day), "gen3_day_members": len(g3_day),
        "gen2_skipped": len(g2_skip), "gen3_skipped": len(g3_skip),
    }, ensure_ascii=False, indent=1))


if __name__ == "__main__":
    main()
