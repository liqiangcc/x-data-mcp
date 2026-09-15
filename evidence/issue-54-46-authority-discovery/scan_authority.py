#!/usr/bin/env python3
"""issue-54-46-authority-discovery scanner.

Source-first authority inventory: enumerates retained official archives and
staged official documents, records mechanical facts (hashes, member tables,
token-namespace coverage, keyword hits), and emits a deterministic
sources.json. It performs NO semantic inference and asserts NO contract.

Usage:
    python3 scan_authority.py --output sources.json
    python3 scan_authority.py --check sources.json
"""
import argparse
import hashlib
import io
import json
import re
import sys
import zipfile
import zlib
from collections import Counter
from pathlib import Path

EVIDENCE_DIR = Path(__file__).resolve().parent
RETAINED = Path("/home/box/retained-inputs")

DOC_LIKE_EXTS = {".txt", ".md", ".chm", ".hlp", ".ini", ".lst", ".pdf",
                 ".doc", ".docx", ".html", ".cfg", ".dat"}
KEYWORDS = ["除权", "除息", "复权", "分红", "配股", "送转", "股本", "权息",
            "参考价", "行业", "证券代码", "gbbq", "tdxhy", "沪深", "A股"]
TOKEN_RES = {
    "C_field": re.compile(r"\bC[1-4]\b"),
    "T": re.compile(r"\bT[0-9]{2,6}\b"),
    "X": re.compile(r"\bX[0-9]{2,6}\b"),
    "ZX": re.compile(r"\bZX[0-9]{6}\b"),
    "CJ": re.compile(r"\bCJ[0-9]{6}\b"),
    "ZZ": re.compile(r"\bZZ[0-9]{2,8}\b"),
}
PROBE_CODES = ["X4202", "X420202", "X500102", "T1205", "T010101", "T00"]


def sha256_bytes(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()


def decode_best(raw: bytes) -> str:
    for enc in ("utf-8", "gb18030"):
        try:
            return raw.decode(enc)
        except UnicodeDecodeError:
            continue
    return raw.decode("utf-8", errors="replace")


def load_plan() -> dict:
    return json.loads((EVIDENCE_DIR / "plan.json").read_text())


def verify_inputs(plan: dict) -> dict:
    """Fail-closed hash check of every pinned input."""
    results = {}
    files = dict(plan["inputs"]["files"])
    docs = plan["inputs"]["tq_docs"]
    for name, pin in files.items():
        p = RETAINED / name
        actual = sha256_bytes(p.read_bytes())
        ok = actual == pin["sha256"]
        if not ok:
            raise SystemExit(f"PIN MISMATCH {name}: {actual} != {pin['sha256']}")
        results[name] = {"sha256": actual, "bytes": p.stat().st_size, "pin_ok": True}
    for name, pin in docs.items():
        p = RETAINED / "official-tq-docs" / name
        actual = sha256_bytes(p.read_bytes())
        if actual != pin["sha256"]:
            raise SystemExit(f"PIN MISMATCH official-tq-docs/{name}: {actual}")
        results[f"official-tq-docs/{name}"] = {
            "sha256": actual, "bytes": p.stat().st_size, "pin_ok": True}
    return results


def member_inventory(zpath: Path, zip_name: str, hsjday: bool) -> list:
    """Recursive member inventory. hsjday .day members: CRC32 only."""
    out = []

    def walk(zf: zipfile.ZipFile, prefix: str):
        for info in sorted(zf.infolist(), key=lambda i: i.filename):
            ext = Path(info.filename).suffix.lower()
            if hsjday and ext == ".day":
                out.append({"member": prefix + info.filename,
                            "size": info.file_size, "ext": ext,
                            "crc32": f"{info.CRC:08x}",
                            "keyword_hits": [], "doc_like": False})
                continue
            raw = zf.read(info)
            rec = {"member": prefix + info.filename, "size": info.file_size,
                   "ext": ext}
            if info.filename.lower().endswith(".zip"):
                rec["nested_zip_members"] = len(
                    zipfile.ZipFile(io.BytesIO(raw)).infolist())
                rec["sha256"] = sha256_bytes(raw)
                out.append(rec)
                walk(zipfile.ZipFile(io.BytesIO(raw)),
                     prefix + info.filename + "!")
                continue
            if hsjday and ext == ".day":
                rec["crc32"] = f"{info.CRC:08x}"
            else:
                rec["sha256"] = sha256_bytes(raw)
            text = decode_best(raw) if info.file_size < 8_000_000 else ""
            if text:
                hits = [k for k in KEYWORDS if k in text]
                c_hits = len(TOKEN_RES["C_field"].findall(text))
                rec["keyword_hits"] = sorted(hits)
                rec["c_field_token_hits"] = c_hits
                doc_like = (ext in DOC_LIKE_EXTS - {".dat"}
                            or (ext == ".dat" and len(hits) >= 2))
                rec["doc_like"] = doc_like
            else:
                rec["keyword_hits"] = []
                rec["doc_like"] = False
            out.append(rec)
    with zipfile.ZipFile(zpath) as zf:
        walk(zf, zip_name + "!")
    return out


def parse_incon_sections(text: str) -> dict:
    sections = {}
    cur = None
    for line in text.splitlines():
        if line.startswith("#") and "|" not in line:
            cur = line.strip("#").strip()
            sections[cur] = []
        elif not line.strip() or line.startswith("######"):
            cur = None
        elif cur is not None and "|" in line:
            sections[cur].append(line.split("|", 1))
    return sections


def read_member(zpath: Path, *names: str) -> bytes:
    """Read a possibly-nested member ('zhb.zip!incon.dat')."""
    data = zpath.read_bytes()
    for name in names:
        data = zipfile.ZipFile(io.BytesIO(data)).read(name)
    return data


def field_tokens(cfg: bytes, idx: int) -> set:
    out = set()
    for line in cfg.split(b"\r\n"):
        if not line:
            continue
        f = line.split(b"|")
        if len(f) > idx and f[idx]:
            out.add(f[idx].decode())
    return out


def coverage(dict_keys: set, field_vals: set) -> dict:
    return {
        "dict_entries": len(dict_keys),
        "field_distinct": len(field_vals),
        "covered": len(field_vals & dict_keys),
        "uncovered_field_values": sorted(field_vals - dict_keys)[:20],
    }


def analyze_industry_anchors(base_zip: Path, gbbq_zip: Path) -> dict:
    hy = read_member(base_zip, "tdxhy.cfg")
    hy2 = read_member(base_zip, "tdxhy2.cfg")
    t_set = field_tokens(hy, 2)
    x_set = field_tokens(hy, 5)
    zx_set = field_tokens(hy2, 2)
    cj_set = field_tokens(hy2, 3)
    zz_set = field_tokens(hy2, 4)

    inc = decode_best(read_member(gbbq_zip, "zhb.zip", "incon.dat"))
    sect = parse_incon_sections(inc)
    t3 = decode_best(read_member(gbbq_zip, "zhb.zip", "tdxzs3.cfg"))
    t3_map = {}
    for line in t3.splitlines():
        f = line.split("|")
        if len(f) >= 6 and f[5]:
            t3_map[f[5]] = {"name": f[0], "index_code": f[1]}

    pdf_txt = (EVIDENCE_DIR / "industry-classification-extract.txt").read_text()
    pdf_t = set(TOKEN_RES["T"].findall(pdf_txt))
    pdf_x = set(TOKEN_RES["X"].findall(pdf_txt))

    incon_T = {e[0] for e in sect.get("TDXNHY", [])}
    incon_X = {e[0] for e in sect.get("TDXRSHY", [])}
    incon_ZX = {e[0] for e in sect.get("ZXHY", [])}
    incon_CJ = {e[0] for e in sect.get("CJHY", [])}
    incon_ZZ = {e[0] for e in sect.get("ZZHY", [])}
    name_of = {sec: {e[0]: e[1] for e in entries}
               for sec, entries in sect.items()}

    probes = {}
    for code in PROBE_CODES:
        sec = ("TDXRSHY" if code.startswith("X")
               else "TDXNHY" if code.startswith("T") else None)
        probes[code] = {
            "incon_section": sec,
            "incon_name": (name_of.get(sec, {}).get(code)
                           if sec else None),
            "tdxzs3_binding": t3_map.get(code),
            "in_pdf_extract": code in pdf_t or code in pdf_x,
        }

    return {
        "incon_sections": {k: len(v) for k, v in sorted(sect.items())},
        "coverage": {
            "tdxhy_cfg_f2_T_vs_incon_TDXNHY": coverage(incon_T, t_set),
            "tdxhy_cfg_f5_X_vs_incon_TDXRSHY": coverage(incon_X, x_set),
            "tdxhy_cfg_f2_T_vs_pdf": coverage(pdf_t, t_set),
            "tdxhy_cfg_f5_X_vs_pdf": coverage(pdf_x, x_set),
            "tdxhy2_cfg_f2_ZX_vs_incon_ZXHY": coverage(incon_ZX, zx_set),
            "tdxhy2_cfg_f3_CJ_vs_incon_CJHY": coverage(incon_CJ, cj_set),
            "tdxhy2_cfg_f4_ZZ_vs_incon_ZZHY": coverage(incon_ZZ, zz_set),
        },
        "probe_codes": probes,
        "tdxzs3_bindings_total": len(t3_map),
    }


DOC_ANCHORS = {
    "1h10jj7r7jol4.html": ["rs_hycode_sim", "rs_hyname", "blockzscode",
                           "tdx_dycode", "tdx_dyname", "HSStockKind",
                           "TodayDRFlag", "X4202", "881355"],
    "1h10hsiat36k4.html": ["Type", "Bonus", "AlloPrice", "ShareBonus",
                           "Allotment", "除权除息", "扩缩股", "沪深京"],
    "1h10g60jt68sc.html": ["dividend_type", "ForwardFactor", "前复权",
                           "后复权", "none", "front", "back"],
    "1h84ec4p26qus.html": ["BlockCode", "BlockName", "BlockType",
                           "881355", "行业", "概念"],
    "dict.html": [".SZ", ".SH", ".BJ", "dividend_type", "none",
                  "front", "back"],
}


def doc_anchors(plan: dict) -> dict:
    out = {}
    for fn, anchors in DOC_ANCHORS.items():
        raw = (RETAINED / "official-tq-docs" / fn).read_bytes()
        text = re.sub(r"<[^>]+>", " ", decode_best(raw))
        text = re.sub(r"\s+", " ", text)
        out[fn] = {
            "doc": plan["inputs"]["tq_docs"][fn].get("doc"),
            "anchor_strings_present": {a: (a in text) for a in anchors},
        }
    return out


def build_sources(plan: dict, inventory: dict, industry: dict,
                  anchors: dict) -> list:
    """Assemble per-source records: URL/path, hash, scope, gaps, status."""
    src = []

    def rec(**kw):
        kw.setdefault("public", True)
        kw.setdefault("login_required", False)
        kw.setdefault("key_required", False)
        kw.setdefault("status", "VERIFIED")
        src.append(kw)

    # --- official archives ---
    for name, pin in plan["inputs"]["files"].items():
        rec(name=name, type="official_archive",
            url=pin.get("source_url"), path=str(RETAINED / name),
            sha256=pin["sha256"], bytes=pin["bytes"],
            fetched_at="2026-09-15",
            semantic_scope=["raw bytes; member inventory only"],
            cannot_establish=["field semantics", "business contracts"],
            anchor="NONE")
    # --- in-package doc/dictionary members ---
    rec(name="gbbq.zip!zhb.zip!incon.dat", type="in_package_dictionary",
        path="official-gbbq.zip nested member",
        sha256=None,  # member sha recorded in inventory
        semantic_scope=["code->name dictionary across named sections "
                        "(TDXNHY/TDXRSHY/ZXHY/CJHY/ZZHY/ZJHHY/SWHY/...)"],
        cannot_establish=[
            "official statement binding incon.dat sections to tdxhy.cfg/"
            "tdxhy2.cfg fields",
            "generation/version lineage of the dictionary",
        ],
        anchor="PARTIAL_ANCHOR",
        status="CLUE_ONLY",
        note="covers X 349/349, ZX 279/279, CJ 314/314, T 110/111 of "
             "gen-2 field token sets; ZZ 4/256 (different code generation)")
    rec(name="gbbq.zip!zhb.zip!tdxzs3.cfg", type="in_package_table",
        path="official-gbbq.zip nested member",
        semantic_scope=["industry-name -> 880/881xxx index code -> T/X "
                        "token linkage table"],
        cannot_establish=["official statement that tdxzs3 codes bind to "
                          "tdxhy.cfg fields"],
        anchor="PARTIAL_ANCHOR", status="CLUE_ONLY")
    rec(name="gbbq.zip!infoharbor.lst", type="in_package_manifest",
        path="official-gbbq.zip member",
        semantic_scope=["official member->title manifest for infoharbor_* "
                        "text files"],
        cannot_establish=["GBBQ field semantics"],
        anchor="CLUE_ONLY", status="CLUE_ONLY")
    rec(name="gbbq.zip!infoharbor_18.txt", type="in_package_article",
        path="official-gbbq.zip member",
        semantic_scope=["warrant ex-rights/ex-dividend adjustment formulas "
                        "using 除权日参考价/除权前一日收盘价"],
        cannot_establish=["GBBQ C1-C4 field binding",
                          "stock QFQ factor algorithm"],
        anchor="CLUE_ONLY", status="CLUE_ONLY")
    # --- official web docs ---
    docs = plan["inputs"]["tq_docs"]
    doc_scope = {
        "industry-classification.pdf": (
            ["official TDX industry classification methodology; "
             "T-code table (13/56/109 levels) and X/研究行业 table "
             "(30/128/352) with 880/881 index codes"],
            ["explicit binding to tdxhy.cfg field positions",
             "generation pinning to this base.zip snapshot",
             "GBBQ/QFQ semantics"],
            "DIRECT_ANCHOR"),
        "industry-whitepaper.pdf": (["2018 industry sector whitepaper"],
                                    ["current generation", "file bindings"],
                                    "CLUE_ONLY"),
        "calc-rules.doc": (["market statistics rules; warrant adjustment "
                            "formulas; index dividend treatment"],
                           ["GBBQ fields", "QFQ algorithm", "tdxhy"],
                           "CLUE_ONLY"),
        "tq-interface.pdf": (["official TQ API spec (30MB)"],
                             ["binary file layouts"], "PARTIAL_ANCHOR"),
        "dict.html": (["official enums: market type SZ=0/SH=1/BJ=2; "
                       "dividend_type none/front/back; period values"],
                      ["tdxhy field positions", "GBBQ record layout"],
                      "PARTIAL_ANCHOR"),
        "1h10jj7r7jol4.html": (
            ["get_stock_info official fields incl. rs_hycode_sim "
             "(sample X4202/软件服务), rs_hyname, blockzscode (881355), "
             "tdx_dycode/tdx_dyname, HSStockKind type enum 0-10, "
             "TodayDRFlag"],
            ["tdxhy.cfg file binding", "GBBQ semantics"],
            "DIRECT_ANCHOR"),
        "1h10hsiat36k4.html": (
            ["get_divid_factors official fields: Type(1=除权除息,11=扩缩股,"
             "15=重新调整), Bonus 红利, AlloPrice 配股价, ShareBonus "
             "送股/扩缩股比例, Allotment 配股; 沪深京 scope"],
            ["GBBQ binary record layout / C1-C4 ordering", "unit/scale "
             "binding of C1-C4", "float32 representation rule"],
            "PARTIAL_ANCHOR"),
        "1h10g60jt68sc.html": (
            ["get_market_data dividend_type none/front/back; ForwardFactor "
             "semantics incl. 权息变动当天因子沿用 caveat; back-adjustment "
             "depends on returned count"],
            ["exact QFQ formula", "rounding policy", "GBBQ linkage"],
            "PARTIAL_ANCHOR"),
        "1h84ec4p26qus.html": (
            ["get_relation BlockCode/BlockName/BlockType; sample "
             "881355.SH|软件服务|行业 consistent with incon.dat+tdxzs3"],
            ["tdxhy.cfg binding"], "PARTIAL_ANCHOR"),
    }
    for fn, pin in docs.items():
        scope, gaps, anchor = doc_scope.get(
            fn, (["navigation/index or peripheral doc page"],
                 ["direct semantic binding"], "NONE"))
        rec(name=fn, type="official_web_doc",
            url=pin["source_url"],
            path=str(RETAINED / "official-tq-docs" / fn),
            sha256=pin["sha256"],
            last_modified=pin.get("last_modified"),
            fetched_at="2026-09-15",
            semantic_scope=scope, cannot_establish=gaps,
            anchor=anchor,
            status="VERIFIED")
    # --- probed-but-not-fetched / unavailable ---
    for p in plan["url_probes_no_fetch"]:
        rec(name=p["url"], type="official_endpoint_probe", url=p["url"],
            http_status=p["status"],
            semantic_scope=["endpoint availability only"],
            cannot_establish=["any field semantics"],
            anchor="NONE",
            status="VERIFIED" if p["status"] == 200 else
            ("UNREACHABLE" if p["status"] == 403 else "NOT_VERIFIED"),
            note=p.get("note"))
    # --- official behavior ---
    rec(name="official_windows_client_output", type="behavior_oracle",
        url=None,
        semantic_scope=["would bind same-snapshot official display values "
                        "for GBBQ/QFQ and industry names"],
        cannot_establish=["anything on this box"],
        anchor="NONE", status="NOT_RUN", public=False,
        note="Linux box; official client is Windows-only; Wine install "
             "prohibited; TQ API requires running TDX terminal per "
             "install docs (mindoc-1d00kk3jsibbc) -> also NOT_RUN")
    rec(name="tq_api_live_output", type="behavior_oracle",
        url="https://help.tdx.com.cn/quant/docs/markdown/mindoc-1cfsjkbf8f3is/mindoc-1d00kk3jsibbc.html",
        semantic_scope=["would provide official field values against live "
                        "snapshot"],
        cannot_establish=["anything on this box"],
        anchor="NONE", status="NOT_RUN", public=False,
        login_required=True,
        note="TQ functions run inside the TDX terminal client (Windows); "
             "not executable here")
    return src


def run(output: Path):
    plan = load_plan()
    checks = verify_inputs(plan)
    inv = {
        "official-base.zip": member_inventory(
            RETAINED / "official-base.zip", "official-base.zip", False),
        "official-gbbq.zip": member_inventory(
            RETAINED / "official-gbbq.zip", "official-gbbq.zip", False),
        "official-hsjday.zip": member_inventory(
            RETAINED / "official-hsjday.zip", "official-hsjday.zip", True),
    }
    industry = analyze_industry_anchors(
        RETAINED / "official-base.zip", RETAINED / "official-gbbq.zip")
    anchors = doc_anchors(plan)
    sources = build_sources(plan, inv, industry, anchors)

    doc_like = [r for r in
                inv["official-base.zip"] + inv["official-gbbq.zip"]
                if r.get("doc_like")]
    result = {
        "plan_id": plan["plan_id"],
        "generated_under": "freeze-first convention; inputs hash-verified",
        "input_checks": checks,
        "member_inventory": inv,
        "doc_like_members": doc_like,
        "industry_anchor_analysis": industry,
        "doc_anchor_strings": anchors,
        "sources": sources,
        "summary": {
            "n_sources": len(sources),
            "by_status": dict(Counter(s["status"] for s in sources)),
            "by_anchor": dict(Counter(s["anchor"] for s in sources)),
            "n_members": {k: len(v) for k, v in inv.items()},
            "n_doc_like": len(doc_like),
        },
    }
    output.write_text(json.dumps(result, ensure_ascii=False,
                                 indent=1, sort_keys=True) + "\n")
    print(json.dumps(result["summary"], ensure_ascii=False, indent=1))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--output", default="sources.json")
    ap.add_argument("--check", metavar="FILE")
    a = ap.parse_args()
    if a.check:
        import tempfile
        tmp = Path(tempfile.mkdtemp()) / "replayed.json"
        run(tmp)
        expected = Path(a.check).read_bytes()
        actual = tmp.read_bytes()
        if actual != expected:
            print("REPLAY MISMATCH", file=sys.stderr)
            sys.exit(1)
        print("replay byte-identical:", sha256_bytes(actual))
        return
    run(Path(a.output))


if __name__ == "__main__":
    main()
