# -*- coding: utf-8 -*-
"""Deterministic replay for issue-166-windows-official-behavior.

Re-checks, from committed inputs only:
  1. sha256 of committed raw/files/* against raw/files_manifest.json
  2. .day records at the observed event dates against transcribed official displays
  3. tdxhy.cfg field bindings (f0/f2/f5) + incon.dat names for the fixed samples
  4. candidate 前复权 chains for sz.000002 (event table from committed #159 matrix)
     vs the client's displayed adjusted OHLC; plus the cent-rounded-per-event
     variant which must be ruled out.
Outputs determinism.json. No network, no client required.
"""
import hashlib, json, os, struct, sys
from decimal import Decimal, ROUND_HALF_UP

EV = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ROOT = os.path.dirname(EV)
M159 = os.path.join(ROOT, 'issue-54-gbbq-full-events', 'result.json')

def sha256(p):
    h = hashlib.sha256()
    with open(p, 'rb') as f:
        for c in iter(lambda: f.read(1 << 20), b''):
            h.update(c)
    return h.hexdigest()

def r2(x):
    return float(Decimal(str(x)).quantize(Decimal('0.01'), rounding=ROUND_HALF_UP))

def day_records(path):
    d = open(path, 'rb').read()
    out = {}
    for i in range(len(d) // 32):
        r = d[i * 32:(i + 1) * 32]
        dt = struct.unpack('<I', r[:4])[0]
        o, h, l, c = (v / 100 for v in struct.unpack('<IIII', r[4:20]))
        amt, vol = struct.unpack('<fI', r[20:28])
        out[dt] = {"O": o, "H": h, "L": l, "C": c, "amount": amt, "vol": vol, "hex": r.hex()}
    return out

def events_for(security):
    m = json.load(open(M159, encoding='utf-8'))
    rows = m['four_security_matrices'][security]['rows']
    evs = []
    for e in rows:
        c = e['candidates']['exact_binary']['C1_C2_C3_C4']
        evs.append({"date": e['date'], "C": [float(Decimal(v)) for v in c],
                    "hex": e['clear_hex']})
    return evs

def chain(events, round_ref=None, prev_close=None):
    """Compose T_i(x) = (x - C1/10 + C4*C2/10) / ((10+C3+C4)/10) oldest->newest.
    round_ref == 'cent': apply delta so that each event's reference equals its
    cent-rounded value (sub-variant ruled out by the observed displays)."""
    A, B = 1.0, 0.0
    for e in events:
        C1, C2, C3, C4 = e['C']
        d_, a_, s = C1 / 10, C4 * C2 / 10, (10 + C3 + C4) / 10
        delta = 0.0
        if round_ref == 'cent':
            pc = prev_close(e['date'])
            ref = (pc - d_ + a_) / s
            delta = r2(ref) - ref
        A, B = A / s, (B - d_ + a_) / s + delta
    return A, B

def main():
    res = {"checks": [], "ok": True}
    def check(name, ok, detail):
        res["checks"].append({"check": name, "ok": bool(ok), "detail": detail})
        res["ok"] = res["ok"] and ok

    # 1. committed file hashes
    fm = json.load(open(os.path.join(EV, 'raw', 'files_manifest.json')))
    bad = []
    for f in fm['files']:
        if not f['copied']:
            continue
        p = os.path.join(EV, 'raw', 'files', os.path.basename(f['path']))
        if sha256(p) != f['sha256']:
            bad.append(f['path'])
    check("committed file hashes", not bad, {"mismatch": bad})

    obs = json.load(open(os.path.join(EV, 'raw', 'observations.json'), encoding='utf-8'))
    d6 = day_records(os.path.join(EV, 'raw', 'files', 'sh600006.day'))
    d4 = day_records(os.path.join(EV, 'raw', 'files', 'sh600004.day'))
    d2 = day_records(os.path.join(EV, 'raw', 'files', 'sz000002.day'))

    # 2. .day vs transcribed displays
    pairs = [
        ("sh.600006 20250716", d6[20250716], {'O': 7.25, 'H': 7.27, 'L': 7.22, 'C': 7.25}),
        ("sh.600006 20250717", d6[20250717], {'O': 7.24, 'H': 7.32, 'L': 7.23, 'C': 7.32}),
        ("sh.600004 20260723", d4[20260723], {'O': 7.92, 'H': 7.99, 'L': 7.87, 'C': 7.96}),
        ("sh.600004 20260724", d4[20260724], {'O': 7.67, 'H': 7.74, 'L': 7.65, 'C': 7.67}),
        ("sz.000002 20000110", d2[20000110], {'O': 10.68, 'H': 11.44, 'L': 10.55, 'C': 11.44}),
        ("sz.000002 19991229", d2[19991229], {'O': 9.53, 'H': 9.69, 'L': 9.45, 'C': 9.65}),
    ]
    for name, rec, exp in pairs:
        check(f"day-record {name}",
              all(abs(rec[k] - exp[k]) < 1e-9 for k in 'OHLC'),
              {"file": {k: rec[k] for k in 'OHLC'}, "observed_display": exp})

    # 3. boundary-reference discrimination (displayed event-day 昨结/涨幅)
    # 600006: raw prev C 7.25, C1=0.05 -> norm 7.245->7.25 (obs +0.97%); exact-f32 -> 7.24
    ref_norm = r2((d6[20250716]['C'] * 10 - 0.05) / 10)
    ref_f32 = (d6[20250716]['C'] * 10 - struct.unpack('<f', bytes.fromhex('cdcc4c3d'))[0]) / 10
    check("600006 reference discriminates normalization",
          abs(ref_norm - 7.25) < 1e-9 and r2(ref_f32) != 7.25,
          {"normalized_ref": ref_norm, "f32_ref": ref_f32, "f32_ref_2dp": r2(ref_f32),
           "observed_昨结": 7.25, "observed_pct": "+0.97%"})
    # 600004: raw prev C 7.96, served C1-equivalent 2.85 -> ref 7.675.
    # Both decimal and f32 inputs round to 7.68 (f32(0.285)=0.28499999642),
    # so it does NOT discriminate input normalization; it DOES show the
    # displayed 昨结 is the CENT-ROUNDED reference: unrounded pct would be
    # (7.67-7.675)/7.675 = -0.065% -> -0.07%, observed -0.13% <-> 昨结 7.68.
    dps = 2.85 / 10
    ref4 = d4[20260723]['C'] - dps
    pct_unrounded = (d4[20260724]['C'] - ref4) / ref4 * 100
    check("600004 event-day 昨结 uses cent-rounded reference",
          abs(r2(ref4) - 7.68) < 1e-9 and
          abs(r2(pct_unrounded) - (-0.13)) > 0.01,
          {"ref": ref4, "ref_2dp": r2(ref4),
           "pct_if_unrounded_ref": pct_unrounded,
           "observed_昨结": 7.68, "observed_pct": "-0.13%"})

    # 4. tdxhy.cfg bindings for fixed samples
    names = dict(l.split('|', 1) for l in
                 open(os.path.join(EV, 'raw', 'files', 'incon.dat'), 'rb')
                 .read().decode('gbk', 'replace').splitlines() if '|' in l)
    rows = {r[1]: r for r in
            (l.split('|') for l in
             open(os.path.join(EV, 'raw', 'files', 'tdxhy.cfg'), 'rb')
             .read().decode('gbk', 'replace').splitlines() if '|' in l) if len(r) >= 6}
    expect = {  # code: (f0, T, T_name, X, X_name, displayed L1—L2 or leaf)
        '600000': ('1', 'T1001', '银行', 'X500102', '股份制银行', '全国性银行-股份制银行'),
        '000001': ('0', 'T1001', '银行', 'X500102', '股份制银行', '全国性银行-股份制银行'),
        '688318': ('1', 'T1205', '软件服务', 'X420202', '行业应用软件', '软件服务-行业应用软件'),
        '600004': ('1', 'T090302', '机场', 'X610202', '机场', '航空机场-机场'),
        '600006': ('1', 'T040201', '汽车整车', 'X260201', '商用载货车', '商用车-商用载货车'),
        '000002': ('0', 'T110201', '全国地产', 'X530101', '住宅开发', '房地产—房地产开发'),
    }
    for code, (f0, t, tn, x, xn, disp) in expect.items():
        r = rows[code]
        ok = (r[0] == f0 and r[2] == t and r[5] == x
              and names.get(t) == tn and names.get(x) == xn)
        check(f"tdxhy binding {code}", ok,
              {"row": '|'.join(r), "T": f"{t}={names.get(t)}", "X": f"{x}={names.get(x)}",
               "displayed": disp})
    check("000004 absent (delisted)", '000004' not in rows,
          {"note": "delisted security has no tdxhy.cfg row; identity demoted to T000004 extended market"})
    n_nonempty = sum(1 for r in rows.values() if r[3].strip() or r[4].strip())
    check("f3/f4 always empty", n_nonempty == 0, {"rows": len(rows)})

    # 5. sz.000002 前复权 chain vs displayed event-window bars
    evs = events_for('sz.000002')
    days = sorted(day_records(os.path.join(EV, 'raw', 'files', 'sz000002.day')))
    dayidx = {d: i for i, d in enumerate(days)}
    def prev_close(datestr):
        di = int(datestr.replace('-', ''))
        i = dayidx.get(di)
        return d2[days[i - 1]]['C'] if i else None
    ev_after = [e for e in evs if e['date'] > '2000-01-10']
    ev_incl = [e for e in evs if e['date'] >= '2000-01-10']
    obs_evt = {'O': -8.14, 'H': -8.06, 'L': -8.15, 'C': -8.06}
    obs_prev = {'O': -8.28, 'H': -8.27, 'L': -8.29, 'C': -8.27}
    raw_evt = {k: d2[20000110][k] for k in 'OHLC'}
    raw_prev = {k: d2[19991229][k] for k in 'OHLC'}
    def preds(norm_dp, rr=None):
        def ch(events):
            A, B = 1.0, 0.0
            for e in events:
                C1, C2, C3, C4 = e['C']
                if norm_dp:
                    q = lambda v: float(Decimal(str(v)).quantize(Decimal(norm_dp), rounding=ROUND_HALF_UP))
                    C1, C2, C3, C4 = q(C1), q(C2), q(C3), q(C4)
                d_, a_, s = C1 / 10, C4 * C2 / 10, (10 + C3 + C4) / 10
                delta = 0.0
                if rr == 'cent':
                    ref = (prev_close(e['date']) - d_ + a_) / s
                    delta = r2(ref) - ref
                A, B = A / s, (B - d_ + a_) / s + delta
            return A, B
        A1, B1 = ch(ev_after)
        A0, B0 = ch(ev_incl)
        return ({k: r2(A1 * raw_evt[k] + B1) for k in 'OHLC'},
                {k: r2(A0 * raw_prev[k] + B0) for k in 'OHLC'})
    score = {}
    for label, dp in (("exact_binary", None), ("fixed_2dp", "0.01"),
                      ("fixed_3dp", "0.001"), ("fixed_4dp", "0.0001")):
        pe, pp = preds(dp)
        n = sum(pe[k] == obs_evt[k] for k in 'OHLC') + \
            sum(pp[k] == obs_prev[k] for k in 'OHLC')
        score[label] = {"matched_fields": n, "evt_pred": pe, "prev_pred": pp}
    check("qfq chain input normalization discriminated",
          score["fixed_2dp"]["matched_fields"] == 8 and
          score["exact_binary"]["matched_fields"] < 8,
          {"scores": {k: v["matched_fields"] for k, v in score.items()},
           "detail": "2dp-normalized business values reproduce all 8 displayed "
                     "fields; exact-binary f32 misses the O boundary (-8.13 vs -8.14)",
           "observed_evt": obs_evt, "observed_prev": obs_prev,
           "events_after": len(ev_after)})
    re_, rp = preds("0.01", "cent")
    mism = sum(1 for k in 'OHLC' if re_[k] != obs_evt[k]) + \
           sum(1 for k in 'OHLC' if rp[k] != obs_prev[k])
    check("qfq chain cent-rounded-ref ruled out", mism > 0,
          {"evt_pred": re_, "prev_pred": rp, "mismatched_fields": mism})
    res["chain_note"] = ("unrounded affine chain must MATCH (8/8 fields); "
                         "cent-rounded-ref variant must FAIL — it predicts -8.08/-8.29 "
                         "vs observed -8.06/-8.27, ruling out per-event cent rounding")
    out = os.path.join(EV, 'determinism.json')
    json.dump(res, open(out, 'w', encoding='utf-8'), ensure_ascii=False, indent=1)
    print(json.dumps({"ok": res['ok'],
                      "checks": {c['check']: c['ok'] for c in res['checks']}},
                     ensure_ascii=False, indent=1))

if __name__ == '__main__':
    main()
