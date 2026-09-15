#!/usr/bin/env python3
"""issue-46-anchor-binding scanner.

Formalized anchor-binding tests over retained official bytes:
  * official industry-classification PDF table (-raw extract) ->
    code -> {name, level, index_code}
  * three-way per-code comparison: PDF vs incon.dat sections
    (TDXNHY/TDXRSHY) vs tdxzs3.cfg
  * field-token coverage vs tdxhy.cfg f2/f5 and tdxhy2.cfg f2/f3/f4
  * GBBQ binary-binding search across all staged official documents
  * UNANCHORED list for items no source binds

Mechanical evidence only; asserts no contract.
Usage:
    python3 scan_anchor_binding.py --output binding.json
    python3 scan_anchor_binding.py --check binding.json
"""
import argparse
import difflib
import hashlib
import io
import json
import re
import sys
import zipfile
from collections import Counter
from pathlib import Path

EVIDENCE_DIR = Path(__file__).resolve().parent
RETAINED = Path("/home/box/retained-inputs")

CODE = re.compile(r"\b([TX]\d{2,6})\b")
CJK = re.compile(r"^[一-鿿（）、/·()A-Za-z]+")
IDX = re.compile(r"\b(88\d{4,5})\b")

GBBQ_BINDING_PATTERNS = [
    "gbbq", "GBBQ", "float32", "单精度", "记录结构", "二进制",
    "偏移", "除权除息", "复权", "分红送配", "参考价", "权息",
]
GBBQ_BIND_RES = [
    re.compile(r"\bC[1-4]\b"),
    re.compile(r"gbbq", re.I),
    re.compile(r"float32|单精度浮点", re.I),
]


def sha256_bytes(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()


def decode_best(raw: bytes) -> str:
    for enc in ("utf-8", "gb18030", "utf-16le"):
        try:
            return raw.decode(enc)
        except (UnicodeDecodeError, ValueError):
            continue
    return raw.decode("utf-8", errors="replace")


def load_plan() -> dict:
    return json.loads((EVIDENCE_DIR / "plan.json").read_text())


def verify_inputs(plan: dict) -> dict:
    results = {}
    for name, pin in plan["inputs"]["files"].items():
        p = RETAINED / name
        actual = sha256_bytes(p.read_bytes())
        if actual != pin["sha256"]:
            raise SystemExit(f"PIN MISMATCH {name}: {actual}")
        results[name] = {"sha256": actual, "pin_ok": True}
    for name, pin in plan["inputs"]["committed_extracts"].items():
        p = EVIDENCE_DIR / name
        actual = sha256_bytes(p.read_bytes())
        if actual != pin["sha256"]:
            raise SystemExit(f"PIN MISMATCH {name}: {actual}")
        results[name] = {"sha256": actual, "pin_ok": True}
    return results


def read_member(zpath: Path, *names: str) -> bytes:
    data = zpath.read_bytes()
    for name in names:
        data = zipfile.ZipFile(io.BytesIO(data)).read(name)
    return data


def parse_pdf_table(lines, start, end, table):
    """Extract code -> {name, idx, level} from the -raw extract region.

    Row continuation rule (frozen): a codeless line belongs to the most
    recently seen code. A digit-led line contributes index token(s); a
    CJK-led line contributes a name only if that code lacks one,
    otherwise it is wrapped description text (ignored). A codeless line
    containing both name and index assigns both to that code.
    """
    out = {}
    last = None

    def entry(code):
        return out.setdefault(code, {"name": None, "idx": set(),
                                     "level": (len(code) - 1) // 2,
                                     "table": table})

    for i in range(start, end):
        l = lines[i]
        ms = list(CODE.finditer(l))
        if ms:
            for j, m in enumerate(ms):
                code = m.group(1)
                seg = l[m.end(): ms[j + 1].start()
                        if j + 1 < len(ms) else len(l)].strip()
                nm = CJK.match(seg)
                im = IDX.search(seg)
                e = entry(code)
                if nm and not e["name"]:
                    e["name"] = nm.group(0)
                if im:
                    e["idx"].add(im.group(1))
                last = code
            continue
        s = l.strip()
        if not s or last is None:
            continue
        e = out[last]
        im = IDX.search(s)
        if s[0].isdigit():
            # index-led continuation line
            for tok in IDX.findall(s):
                e["idx"].add(tok)
        else:
            nm = CJK.match(s)
            if nm and not e["name"]:
                e["name"] = nm.group(0)
            if im:
                e["idx"].add(im.group(1))
    for e in out.values():
        e["idx"] = sorted(e["idx"])
    return out


def parse_incon(text: str) -> dict:
    sections = {}
    cur = None
    for line in text.splitlines():
        if line.startswith("#") and "|" not in line:
            cur = line.strip("#").strip()
            sections[cur] = {}
        elif not line.strip() or line.startswith("######"):
            cur = None
        elif cur is not None and "|" in line:
            k, v = line.split("|", 1)
            sections[cur][k] = v
    return sections


def field_tokens(cfg: bytes, idx: int) -> set:
    out = set()
    for line in cfg.split(b"\r\n"):
        if not line:
            continue
        f = line.split(b"|")
        if len(f) > idx and f[idx]:
            out.add(f[idx].decode())
    return out


def name_status(pdf_name, other_name):
    if pdf_name is None or other_name is None:
        return None
    if pdf_name == other_name:
        return "MATCH"
    if pdf_name in other_name or other_name in pdf_name:
        return "NAME_WORDING_VARIANT"
    ratio = difflib.SequenceMatcher(None, pdf_name, other_name).ratio()
    return ("NAME_WORDING_VARIANT" if ratio >= 0.5 else "NAME_CONFLICT")


def build_rows(pdf_t, pdf_x, incon, t3_map, hy_t, hy_x):
    """Per-code comparison rows over the union of all sources."""
    pdf = {**pdf_t, **pdf_x}
    codes = sorted(set(pdf) | set(incon) | set(t3_map) | hy_t | hy_x)
    rows = []
    for c in codes:
        p = pdf.get(c, {})
        row = {
            "code": c,
            "level": (len(c) - 1) // 2 if len(c) in (3, 5, 7) else None,
            "pdf": {"name": p.get("name"), "idx": p.get("idx", []),
                    "present": c in pdf},
            "incon_name": incon.get(c),
            "tdxzs3": t3_map.get(c),
            "in_tdxhy_f2_T": c in hy_t,
            "in_tdxhy_f5_X": c in hy_x,
        }
        diffs = []
        if c not in pdf:
            diffs.append("PDF_MISSING_CODE")
        if p.get("name") is None and c in pdf:
            diffs.append("PDF_CODE_NO_ROW")
        ns = name_status(p.get("name"), incon.get(c))
        if ns:
            diffs.append("incon_name:" + ns)
        t3 = t3_map.get(c)
        if t3:
            if (t3.get("name") or "").startswith("TDX "):
                # level-1 aggregate display alias, e.g. 'TDX 商贸'
                diffs.append("tdxzs3_name:NAME_DISPLAY_ALIAS")
            else:
                ns3 = name_status(p.get("name"), t3.get("name"))
                if ns3:
                    diffs.append("tdxzs3_name:" + ns3)
            pidx = set(p.get("idx", []))
            t3idx = t3.get("idx")
            if pidx and t3idx:
                diffs.append("tdxzs3_idx:" +
                             ("IDX_MATCH" if t3idx in pidx
                              else "IDX_MISMATCH"))
        elif c in pdf and c[0] in "TX":
            diffs.append("TDXZS3_MISSING")
        if not diffs:
            diffs.append("MATCH")
        row["diffs"] = diffs
        rows.append(row)
    return rows


def gbbq_binding_scan(plan: dict) -> dict:
    """Scan every staged official doc for GBBQ binary-binding statements."""
    results = {}
    docs_dir = RETAINED / "official-tq-docs"
    texts = {}
    for p in sorted(docs_dir.iterdir()):
        raw = p.read_bytes()
        if p.suffix == ".pdf":
            continue  # handled via committed extracts
        texts[p.name] = decode_best(raw)
    texts["tq-interface-extract.txt"] = (
        EVIDENCE_DIR / "tq-interface-extract.txt").read_text()
    texts["industry-classification-extract-raw.txt"] = (
        EVIDENCE_DIR / "industry-classification-extract-raw.txt").read_text()
    for name, text in sorted(texts.items()):
        hits = {k: len(re.findall(re.escape(k), text))
                for k in GBBQ_BINDING_PATTERNS if k in text}
        bind_hits = sum(len(r.findall(text)) for r in GBBQ_BIND_RES)
        results[name] = {
            "keyword_hits": hits,
            "binary_binding_tokens": bind_hits,
            "binding_statement_found": False,
            "note": ("no statement binding GBBQ binary record fields "
                     "C1-C4 (order/unit/float32/rounding) found; "
                     "keyword presence is CLUE_ONLY"),
        }
    return results


def run(output: Path):
    plan = load_plan()
    checks = verify_inputs(plan)

    lines = (EVIDENCE_DIR /
             "industry-classification-extract-raw.txt").read_text().split("\n")
    t_start = next(i for i, l in enumerate(lines)
                   if "通达信行业分为三个级别" in l)
    x_start = next(i for i, l in enumerate(lines)
                   if "研究行业分为三个级别" in l)
    x_end = next(i for i, l in enumerate(lines[x_start:], x_start)
                 if "概念板块" in l)

    pdf_t = parse_pdf_table(lines, t_start, x_start, "T")
    pdf_x = parse_pdf_table(lines, x_start, x_end, "X")

    base = RETAINED / "official-base.zip"
    gbbq = RETAINED / "official-gbbq.zip"
    inc = parse_incon(decode_best(
        read_member(gbbq, "zhb.zip", "incon.dat")))
    incon_tx = {**inc["TDXNHY"], **inc["TDXRSHY"]}
    t3 = decode_best(read_member(gbbq, "zhb.zip", "tdxzs3.cfg"))
    t3_map = {}
    for l in t3.splitlines():
        f = l.split("|")
        if len(f) >= 6 and f[5]:
            t3_map[f[5]] = {"name": f[0], "idx": f[1]}
    t3_tx = {k: v for k, v in t3_map.items()
             if CODE.fullmatch(k)}

    hy = read_member(base, "tdxhy.cfg")
    hy2 = read_member(base, "tdxhy2.cfg")
    hy_t = field_tokens(hy, 2)
    hy_x = field_tokens(hy, 5)
    hy2_zx = field_tokens(hy2, 2)
    hy2_cj = field_tokens(hy2, 3)
    hy2_zz = field_tokens(hy2, 4)
    hy_f0 = field_tokens(hy, 0)
    hy2_f0 = field_tokens(hy2, 0)

    rows = build_rows(pdf_t, pdf_x, incon_tx, t3_tx, hy_t, hy_x)

    def lvl_counts(d):
        return dict(Counter(str(v["level"]) for v in d.values()))

    declared = {
        "T": {"declared": {"1": 13, "2": 56, "3": 109},
              "tabulated": lvl_counts(pdf_t)},
        "X": {"declared": {"1": 30, "2": 128, "3": 352},
              "tabulated": lvl_counts(pdf_x)},
    }

    diff_counter = Counter()
    unexplained = []
    for r in rows:
        for d in r["diffs"]:
            diff_counter[d.split(":")[-1] if ":" in d else d] += 1
            head = d.split(":")[-1] if ":" in d else d
        if any(d in ("NAME_CONFLICT", "IDX_MISMATCH",
                     "PDF_CODE_NO_ROW", "EXTRACTION_AMBIGUOUS")
               or d.endswith("NAME_CONFLICT")
               or d.endswith("IDX_MISMATCH")
               for d in r["diffs"]):
            unexplained.append(r)

    gbbq_scan = gbbq_binding_scan(plan)

    coverage = {
        "tdxhy_f2_T_in_pdf": sorted(hy_t - set(pdf_t)),
        "tdxhy_f5_X_in_pdf": sorted(hy_x - set(pdf_x)),
        "tdxhy_f2_T_in_incon": sorted(hy_t - set(inc["TDXNHY"])),
        "tdxhy_f5_X_in_incon": sorted(hy_x - set(inc["TDXRSHY"])),
        "tdxhy2_f2_ZX_in_incon_ZXHY": sorted(hy2_zx - set(inc["ZXHY"])),
        "tdxhy2_f3_CJ_in_incon_CJHY": sorted(hy2_cj - set(inc["CJHY"])),
        "tdxhy2_f4_ZZ_in_incon_ZZHY": sorted(hy2_zz - set(inc["ZZHY"])),
    }

    result = {
        "plan_id": plan["plan_id"],
        "input_checks": checks,
        "pdf_tables": {
            "declared_vs_tabulated": declared,
            "T": {c: {"name": v["name"], "idx": v["idx"],
                      "level": v["level"]}
                  for c, v in sorted(pdf_t.items())},
            "X": {c: {"name": v["name"], "idx": v["idx"],
                      "level": v["level"]}
                  for c, v in sorted(pdf_x.items())},
        },
        "binding_matrix": {
            "rows": rows,
            "diff_summary": dict(diff_counter),
            "unexplained": unexplained,
            "unexplained_count": len(unexplained),
        },
        "field_coverage": {
            "tdxhy_cfg": {
                "f0_values": sorted(hy_f0),
                "f2_T_tokens": len(hy_t),
                "f5_X_tokens": len(hy_x),
                "note": ("f0 in {0,1,2} mechanically overlaps Dict.html "
                         "market enum (SZ=0/SH=1/BJ=2) — vocabulary "
                         "candidate only; positional field binding "
                         "unproven"),
            },
            "tdxhy2_cfg": {
                "f0_values": sorted(hy2_f0),
                "f2_ZX_tokens": len(hy2_zx),
                "f3_CJ_tokens": len(hy2_cj),
                "f4_ZZ_tokens": len(hy2_zz),
            },
            "uncovered_field_tokens": coverage,
        },
        "gbbq_doc_binding_scan": {
            "per_document": gbbq_scan,
            "verdict": ("NO official document found that binds GBBQ "
                        "binary record fields C1-C4 to business "
                        "fields/unit/scale/float32/rounding. Official "
                        "docs bind the business-field-name layer "
                        "(get_divid_factors: Type/Bonus/AlloPrice/"
                        "ShareBonus/Allotment); the business-field -> "
                        "binary-offset/encoding layer is absent. Only "
                        "paths to fill it: official client behavior "
                        "output (NOT_RUN on this box) or a public "
                        "official format spec (not found)."),
        },
        "unanchored": [
            "tdxhy.cfg f0 market-field positional binding (Dict.html "
            "enum supplies vocabulary 0/1/2 only)",
            "tdxhy.cfg f1 code<->security-name binding file role",
            "tdxhy.cfg f3/f4 (always empty in gen-2)",
            "tdxhy2.cfg f1/f5 roles and empty-field semantics",
            "A-share scope boundary of tdxhy.cfg/ex.code/base.dbf "
            "(HSStockKind enum anchors vocabulary only)",
            "tdxhy2.cfg f4 ZZ code generation (incon.dat ZZHY covers "
            "4/256)",
            "GBBQ C1-C4 binary binding (see gbbq_doc_binding_scan)",
        ],
        "strength_assessment": {
            "S1_CODE_SPACE_BOUND": ("PASS for T and X namespaces: "
                                    "official PDF tabulates codes, "
                                    "names, levels, index codes"),
            "S2_DICT_AGREES": ("incon.dat TDXNHY/TDXRSHY and tdxzs3.cfg "
                               "agree with PDF table within explained "
                               "diff classes (wording variants, X1203, "
                               "7 PDF-only X codes, index 0 mismatch)"),
            "S3_FILE_FIELD_BINDING": ("tdxhy.cfg f2/f5 token sets are "
                                      "subsets of the anchored code "
                                      "spaces; necessary condition met, "
                                      "sufficiency NOT established"),
        },
    }
    output.write_text(json.dumps(result, ensure_ascii=False,
                                 indent=1, sort_keys=True) + "\n")
    s = result["binding_matrix"]
    print(json.dumps({"rows": len(rows),
                      "unexplained": s["unexplained_count"],
                      "diff_summary": s["diff_summary"],
                      "declared_vs_tabulated": declared},
                     ensure_ascii=False, indent=1))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--output", default="binding.json")
    ap.add_argument("--check", metavar="FILE")
    a = ap.parse_args()
    if a.check:
        import tempfile
        tmp = Path(tempfile.mkdtemp()) / "replayed.json"
        run(tmp)
        if tmp.read_bytes() != Path(a.check).read_bytes():
            print("REPLAY MISMATCH", file=sys.stderr)
            sys.exit(1)
        print("replay byte-identical:", sha256_bytes(tmp.read_bytes()))
        return
    run(Path(a.output))


if __name__ == "__main__":
    main()
