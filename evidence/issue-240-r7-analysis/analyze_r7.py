#!/usr/bin/env python3
"""Issue #240 — .day 第 8 字段 (r7) 语义取证。

对 pin 的 gen-3 官方输入做四层机械验证:

1. r7 分类统计 — 每成员 prev_close 命中 / 常量 0x10000 / 零值 / 其他
2. r7-vs-gbbq 事件日对账 — r7≠prev_close 且 r7∉{0,65536} 的日期
   是否落在同证券 gbbq cat1/cat2 事件日
3. 公式复现 — cat1 事件 (f0=每10股红利元, f2=每10股送股, f3=每10股转增):
   basis = round((prev_close - f0/10) / (1 + f2/10 + f3/10), 2dp)
   与 r7/100 比较 (精确命中 / ±1分 舍入边界 / 不符)
4. 退化边界 — r7 从 prev_close 语义退化为 0x10000 的最后非退化的日期

输入 (全部 pin sha256, 见 results.json 头部):
  hsjday.zip      gen-3 日线快照 (official-inputs)
  gbbq.zip        gen-3 股本变迁快照, 成员 "gbbq" 为加密记录流
  gbbq_reader.py  pytdx @0d6f806 固定源, 仅用于提取 hexdump_keys
                  (evidence/issue-3-tdx-gbbq-community/source-provenance.json pin)

用法:
  python3 analyze_r7.py <hsjday.zip> <gbbq.zip> <gbbq_reader.py> [sample_n]

输出: r7-analysis.json (当前目录)。诊断工具,非裁决。
"""

import hashlib
import importlib.util
import json
import random
import struct
import sys
import zipfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
DECODER = REPO / "evidence/issue-3-tdx-gbbq-community/decode_gbbq.py"

EQUITY_PREFIXES = ("600", "601", "603", "605", "000", "001", "002", "003", "300", "301", "688")


def sha256(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()


def load_decoder():
    spec = importlib.util.spec_from_file_location("dec", str(DECODER))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def decode_gbbq(gbbq_zip_bytes: bytes, reader_source: bytes):
    """返回 {(market, code): {date_int: [(category, floats)]}} 及记录总数。"""
    mod = load_decoder()
    key = mod.key_from_source(reader_source)
    z = zipfile.ZipFile(__import__("io").BytesIO(gbbq_zip_bytes))
    raw = z.read("gbbq")
    count = struct.unpack_from("<I", raw, 0)[0]
    assert 4 + count * 29 == len(raw), "gbbq member layout: 4B count + N*29B records"
    events = {}
    for i in range(count):
        r = mod.parse_clear_record(
            mod.decode_encrypted_record(raw[4 + i * 29 : 4 + (i + 1) * 29], key), i
        )
        if r.get("date_calendar_valid"):
            events.setdefault((r["market"], r["code"]), {}).setdefault(
                r["date_raw_int"], []
            ).append((r["category"], r["float32_repr_diagnostic"]))
    return events, count


def scan_member(raw: bytes):
    """单 .day 成员全量 r7 分类。"""
    n = len(raw) // 32
    recs = [struct.unpack("<IIIIIfII", raw[i * 32 : (i + 1) * 32]) for i in range(n)]
    out = {
        "records": n,
        "match_prev_close": 0,
        "const_65536": 0,
        "zero": 0,
        "first_record_r7": recs[0][7] if n else None,
        "last_non65536_date": None,
        "mismatches": [],  # (date, prev_close_fen, r7)
    }
    for i in range(n):
        r7 = recs[i][7]
        if r7 == 65536:
            out["const_65536"] += 1
        elif r7 == 0:
            out["zero"] += 1
        else:
            out["last_non65536_date"] = recs[i][0]
            if i > 0 and r7 == recs[i - 1][4]:
                out["match_prev_close"] += 1
            elif i > 0:
                out["mismatches"].append((recs[i][0], recs[i - 1][4], r7))
    return out, recs


def main():
    hsjday_path, gbbq_path, reader_path = sys.argv[1], sys.argv[2], sys.argv[3]
    sample_n = int(sys.argv[4]) if len(sys.argv) > 4 else 50

    hsjday_bytes = Path(hsjday_path).read_bytes()
    gbbq_bytes = Path(gbbq_path).read_bytes()
    reader_bytes = Path(reader_path).read_bytes()

    events, gbbq_count = decode_gbbq(gbbq_bytes, reader_bytes)

    z = zipfile.ZipFile(__import__("io").BytesIO(hsjday_bytes))
    day_names = sorted(
        n for n in z.namelist() if n.lower().endswith(".day") and "\\lday\\" in n.lower()
    )

    fixed = [
        "sh\\lday\\sh600000.day",
        "sz\\lday\\sz000001.day",
        "sh\\lday\\sh600006.day",
        "sz\\lday\\sz000002.day",
        "sh\\lday\\sh600519.day",
    ]
    random.seed(42)
    sample = fixed + random.sample(
        [n for n in day_names if n not in fixed], min(sample_n, len(day_names) - len(fixed))
    )

    per_member = []
    for name in sample:
        stem = name.split("\\")[-1].replace(".day", "")
        code, market = stem[2:], (1 if stem.startswith("sh") else 0)
        equity = code.startswith(EQUITY_PREFIXES)
        scan, recs = scan_member(z.read(name))
        ev_map = events.get((market, code), {})

        exact_hit, adj_hit, no_event = 0, 0, []
        formula_ok, formula_round1, formula_fail = [], [], []
        for d, prev_fen, r7 in scan["mismatches"]:
            day_events = ev_map.get(d, [])
            cats = {c for c, _ in day_events}
            cat1 = next((f for c, f in day_events if c == 1), None)
            if day_events:
                exact_hit += 1
                if cat1:
                    f0, f1, f2, f3 = cat1
                    pred = round((prev_fen / 100 - f0 / 10) / (1 + f2 / 10 + f3 / 10) * 100)
                    diff = r7 - pred
                    entry = (d, prev_fen / 100, r7 / 100, cat1, pred / 100)
                    if diff == 0:
                        formula_ok.append(entry)
                    elif abs(diff) == 1:
                        formula_round1.append(entry)
                    else:
                        formula_fail.append(entry)
            else:
                no_event.append(d)

        per_member.append(
            {
                "member": name,
                "equity_prefix": equity,
                "records": scan["records"],
                "match_prev_close": scan["match_prev_close"],
                "const_65536": scan["const_65536"],
                "zero": scan["zero"],
                "first_record_r7": scan["first_record_r7"],
                "last_non65536_date": scan["last_non65536_date"],
                "mismatches": len(scan["mismatches"]),
                "mismatch_on_event_date": exact_hit,
                "mismatch_no_event": no_event[:10],
                "formula_exact": len(formula_ok),
                "formula_round_1fen": len(formula_round1),
                "formula_fail": len(formula_fail),
                "formula_samples": (formula_ok + formula_round1 + formula_fail)[:12],
            }
        )

    equities = [m for m in per_member if m["equity_prefix"]]
    others = [m for m in per_member if not m["equity_prefix"]]
    result = {
        "schema": "x-data-mcp.issue-240-r7-analysis.v1",
        "diagnostic_only": True,
        "inputs": {
            "hsjday_zip_sha256": sha256(hsjday_bytes),
            "gbbq_zip_sha256": sha256(gbbq_bytes),
            "gbbq_reader_py_sha256": sha256(reader_bytes),
            "decoder_path": str(DECODER.relative_to(REPO)),
        },
        "gbbq_records": gbbq_count,
        "day_members_total": len(day_names),
        "sample_size": len(per_member),
        "equity_sample": {
            "count": len(equities),
            "match_prev_close": sum(m["match_prev_close"] for m in equities),
            "const_65536": sum(m["const_65536"] for m in equities),
            "zero": sum(m["zero"] for m in equities),
            "mismatches": sum(m["mismatches"] for m in equities),
            "mismatch_on_event_date": sum(m["mismatch_on_event_date"] for m in equities),
            "formula_exact": sum(m["formula_exact"] for m in equities),
            "formula_round_1fen": sum(m["formula_round_1fen"] for m in equities),
            "formula_fail": sum(m["formula_fail"] for m in equities),
        },
        "non_equity_sample": {
            "count": len(others),
            "mismatches": sum(m["mismatches"] for m in others),
            "mismatch_on_event_date": sum(m["mismatch_on_event_date"] for m in others),
        },
        "per_member": per_member,
    }
    out = Path(__file__).parent / "r7-analysis.json"
    out.write_text(json.dumps(result, ensure_ascii=False, indent=1))
    print(json.dumps({k: result[k] for k in ("equity_sample", "non_equity_sample")}, indent=1))
    print(f"wrote {out}")


if __name__ == "__main__":
    main()
