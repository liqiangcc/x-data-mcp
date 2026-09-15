#!/usr/bin/env python3
"""issue-54-46-client-package-probe scanner.

Inventories doc/data members of the retained official Kylin amd64 client
.deb, scans them for the frozen keyword set, compares same-name data files
against gen-2 official archives, and emits result.json. Mechanical only.
"""
import argparse, hashlib, io, json, re, subprocess, sys, tempfile, zipfile
from pathlib import Path

RETAINED = Path("/home/box/retained-inputs")
HERE = Path(__file__).resolve().parent
ALLOWED_EXT = {".help", ".chm", ".hlp", ".txt", ".pdf", ".cfg", ".dat", ".ini"}
KEYWORDS = ["gbbq", "GBBQ", "除权", "除息", "复权", "权息", "分红", "配股",
            "送转", "股本", "行业", "incon", "tdxhy", "tdxzs", "base.zip",
            "字段", "偏移", "二进制", "记录结构", "float", "单精度", "参考价",
            "C1", "C2", "C3", "C4"]
SAME_NAME = ["tdxhy.cfg", "tdxhy2.cfg", "tdxzs.cfg", "tdxzs3.cfg", "gbbq",
             "gbbq.map", "base.dbf", "base.map", "infoharbor.lst",
             "infoharbor_ex.code", "infoharbor_ex.name", "incon.dat"]


def sha256(b): return hashlib.sha256(b).hexdigest()


def pin(path, expected):
    b = Path(path).read_bytes()
    h = sha256(b)
    if h != expected:
        sys.exit(f"PIN MISMATCH {path}: {h} != {expected}")
    return b


def is_text(b):
    if not b: return True
    head = b[:2000]
    ok = sum(1 for c in head if 9 <= c <= 13 or 32 <= c <= 126 or c >= 128)
    return ok * 100 // len(head) >= 85


def decode(b):
    for enc in ("utf-8", "gbk"):
        try: return b.decode(enc)
        except UnicodeDecodeError: pass
    return b.decode("gbk", "replace")


def kw_hits(text):
    return {k: n for k in KEYWORDS
            if (n := len(re.findall(re.escape(k), text)))}


def kw_excerpts(text, keywords, ctx=120, cap=6):
    out = []
    for k in keywords:
        n = 0
        for m in re.finditer(re.escape(k), text):
            a = max(0, m.start() - ctx)
            out.append({"keyword": k, "excerpt": text[a:m.end() + ctx]})
            n += 1
            if n >= cap: break
    return out


def incon_sections(b):
    secs, cur = {}, "?"
    for ln in decode(b).splitlines():
        ln = ln.strip()
        if ln.startswith("#"):
            cur = ln
            secs[cur] = []
        elif ln:
            secs.setdefault(cur, []).append(ln.split("|")[0].strip())
    return secs


def tdxhy_code_sets(base_zip):
    z = zipfile.ZipFile(io.BytesIO(base_zip))
    f2, f5 = set(), set()
    for ln in z.read("tdxhy.cfg").decode("ascii").splitlines():
        p = ln.split("|")
        if len(p) > 2 and p[2]: f2.add(p[2])
        if len(p) > 5 and p[5]: f5.add(p[5])
    return f2, f5


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--plan", default=str(HERE / "plan.json"))
    ap.add_argument("--out", default=str(HERE / "result.json"))
    ap.add_argument("--check", action="store_true")
    args = ap.parse_args()

    plan = json.loads(Path(args.plan).read_text())
    inp = plan["inputs"]["files"]

    deb = pin(RETAINED / "official-client-com.tdx.tdxcfv_7.64_amd64.deb",
              inp["official-client-com.tdx.tdxcfv_7.64_amd64.deb"]["sha256"])
    base = pin(RETAINED / "official-base.zip", inp["official-base.zip"]["sha256"])
    gbbq = pin(RETAINED / "official-gbbq.zip", inp["official-gbbq.zip"]["sha256"])
    docs = {}
    for rel, spec in inp.items():
        if rel.startswith("official-tq-docs/"):
            docs[rel] = pin(RETAINED / rel, spec["sha256"])

    # --- extract deb once; keep bytes for allowed doc/data members ---
    with tempfile.TemporaryDirectory(prefix="tdxprobe-") as td:
        debp = Path(td) / "pkg.deb"
        debp.write_bytes(deb)
        root = Path(td) / "x"
        root.mkdir()
        subprocess.run(["dpkg-deb", "-x", str(debp), str(root)], check=True)
        members = sorted(p for p in root.rglob("*") if p.is_file())
        inventory, kw_report, member_bytes = [], {}, {}
        for p in members:
            rel = "/" + str(p.relative_to(root))
            if p.suffix.lower() not in ALLOWED_EXT:
                continue
            b = p.read_bytes()
            txt = is_text(b)
            entry = {"member": rel, "size": len(b), "sha256": sha256(b),
                     "text_like": txt}
            if txt:
                hits = kw_hits(decode(b))
                if hits:
                    entry["keyword_hits"] = hits
                    kw_report[rel] = kw_excerpts(decode(b), list(hits))
            inventory.append(entry)
            member_bytes[p.name.lower()] = b

    # --- same-name comparison ---
    zg = zipfile.ZipFile(io.BytesIO(gbbq))
    zhb = zipfile.ZipFile(io.BytesIO(zg.read("zhb.zip")))
    gen2_incon = zhb.read("incon.dat")

    comparisons = {}
    for name in SAME_NAME:
        key = name.lower()
        if key not in member_bytes:
            comparisons[name] = {"status": "ABSENT_IN_PACKAGE"}
            continue
        b = member_bytes[key]
        comparisons[name] = {"status": "FOUND_MEMBER_CORROBORATION",
                             "sha256": sha256(b), "size": len(b)}

    # --- incon.dat detailed comparison ---
    ds, gs = incon_sections(member_bytes["incon.dat"]), incon_sections(gen2_incon)
    f2, f5 = tdxhy_code_sets(base)
    comparisons["incon.dat"]["incon_detail"] = {
        "deb_sha256": sha256(member_bytes["incon.dat"]),
        "deb_size": len(member_bytes["incon.dat"]),
        "gen2_sha256": sha256(gen2_incon), "gen2_size": len(gen2_incon),
        "byte_identical": member_bytes["incon.dat"] == gen2_incon,
        "deb_sections": {k: len(v) for k, v in ds.items()},
        "gen2_sections": {k: len(v) for k, v in gs.items()},
        "sections_only_in_gen2": sorted(set(gs) - set(ds)),
        "sections_only_in_deb": sorted(set(ds) - set(gs)),
        "tdxhy_f2_coverage": {
            "covered": sum(1 for c in f2 if c in set(ds.get("#TDXNHY", []))),
            "total": len(f2),
            "missing": sorted(c for c in f2 if c not in set(ds.get("#TDXNHY", [])))},
        "tdxhy_f5_coverage": {
            "covered": sum(1 for c in f5 if c in set(ds.get("#TDXRSHY", []))),
            "total": len(f5),
            "missing": sorted(c for c in f5 if c not in set(ds.get("#TDXRSHY", [])))},
    }

    # --- manual corpus scan ---
    manual = decode(docs["official-tq-docs/tdxw-left.html"])
    manual_report = {
        "size": len(docs["official-tq-docs/tdxw-left.html"]),
        "keyword_hits": kw_hits(manual),
        "excerpts": kw_excerpts(manual, ["除权", "复权", "权息", "分红", "配股", "参考价"], cap=4),
        "classification": "FOUND_BUSINESS_LEVEL_ONLY",
        "note": "Official client manual prose describes precise auto-adjustment, front/back adjustment, and rights-database maintenance/display - a business capability statement; no binary member or field offset is named.",
    }

    assessment = [
        {"item": "#46 tdxhy.cfg field<->anchored code-space declaration",
         "status": "NOT_FOUND",
         "detail": "Package ships no tdxhy.cfg/tdxzs*.cfg and no field-spec document; incon.dat corroborates the code space but no text binds tdxhy.cfg field positions to it."},
        {"item": "#46 A-share scope / market-role / field roles (f0,f1,f3,f4)",
         "status": "NOT_FOUND",
         "detail": "No member declares record scope or per-field roles."},
        {"item": "#54 business-field -> GBBQ binary C1..C4 binding",
         "status": "NOT_FOUND",
         "detail": "No member documents gbbq binary layout, C1-C4 order, units, float32 encoding, or rounding; ihelp.dat/manual prose is column-help business text only."},
        {"item": "client ships incon.dat (same dictionary namespace)",
         "status": "FOUND_MEMBER_CORROBORATION",
         "detail": "incon.dat present in deb, different bytes vs gen-2 gbbq.zip copy (one-generation-older dictionary; gen-2 adds #SGXHY/#JPHY sections). Coverage vs tdxhy codes in incon_detail."},
        {"item": "client online manual corpus (tdxw)",
         "status": "FOUND_BUSINESS_LEVEL_ONLY",
         "detail": "Manual states precise auto-adjustment, front/back adjustment, and rights-database maintenance/display."},
        {"item": "new_tdx64.exe Windows installer",
         "status": "NOT_RUN",
         "detail": "Enumerated (URL+HTTP metadata in plan) but not downloaded/unpacked: no innoextract/7z/cabextract/unshield installed; only one package permitted."},
        {"item": "client binary behavior output",
         "status": "NOT_RUN",
         "detail": "Prohibited by scope: no execution, no wine, no RE on binaries."},
    ]

    result = {
        "plan_id": plan["plan_id"],
        "package": {
            "file": "official-client-com.tdx.tdxcfv_7.64_amd64.deb",
            "sha256": inp["official-client-com.tdx.tdxcfv_7.64_amd64.deb"]["sha256"],
            "size": len(deb),
            "dpkg": {"package": "com.tdx.tdxcfv", "version": "7.64",
                     "architecture": "amd64",
                     "maintainer": "tdx <nie@tdx.com.cn>"},
            "source": inp["official-client-com.tdx.tdxcfv_7.64_amd64.deb"],
            "total_files_in_data_tar": len(members),
            "doc_data_members": len(inventory),
        },
        "member_inventory": inventory,
        "same_name_comparison": comparisons,
        "keyword_excerpts": kw_report,
        "manual_corpus": manual_report,
        "assessment": assessment,
        "unanchored": [
            "#46: tdxhy.cfg field-position -> code-space declaration",
            "#46: tdxhy.cfg f0/f1/f3/f4 field roles",
            "#46: A-share scope semantics",
            "#46: tdxhy2.cfg f1/f5 roles",
            "#54: business-field -> GBBQ binary C1..C4 (order/unit/float32/rounding)",
            "#54: official QFQ arithmetic contract",
        ],
    }
    out = json.dumps(result, ensure_ascii=False, sort_keys=True, indent=1) + "\n"
    if args.check:
        if Path(args.out).read_text() != out:
            sys.exit("REPLAY MISMATCH")
        print("REPLAY OK", sha256(out.encode()))
        return
    Path(args.out).write_text(out)
    print("wrote", args.out, sha256(out.encode()))


if __name__ == "__main__":
    main()
