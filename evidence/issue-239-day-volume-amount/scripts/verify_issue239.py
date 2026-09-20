#!/usr/bin/env python3
"""Deterministic replay for issue-239 unit anchoring.

Re-derives the comparison from committed raw bytes:
  - .day side: gen-3 hsjday.zip member (path must exist locally — not
    committed; pass via --hsjday) OR falls back to the committed
    comparison.json table + raw/ oracle files for the named rows.
  - oracle side: committed tq-raw/*.json (issue-217) + raw/*.json here.

Checks:
  1. TQ kline Volume == f32(.day volume) for issue's 600000 rows
  2. TQ kline Amount*10000 ≈ .day amount within f32-ulp/2 + 0.005万
  3. snapshot Volume == .day volume // 100 (手)
  4. snapshot LastClose present; type=1 field list has no preclose

Exit 0 iff all pass.
"""
import argparse
import json
import struct
import sys
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent.parent
FAIL = []


def check(name, ok, detail=""):
    print(("PASS" if ok else "FAIL"), name, detail)
    if not ok:
        FAIL.append(name)


def f32(x):
    return struct.unpack('<f', struct.pack('<f', x))[0]


def ulp(x):
    f = f32(float(x))
    if f == 0:
        return 0.0
    nxt = struct.unpack('<f', struct.pack(
        '<I', struct.unpack('<I', struct.pack('<f', f))[0] + 1))[0]
    return abs(nxt - f)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--hsjday', default=None,
                    help='path to gen-3 hsjday.zip (optional; named-row '
                         'checks embed the .day values)')
    args = ap.parse_args()

    comp = json.loads((HERE / 'comparison.json').read_text('utf-8'))

    # committed oracle: 600000 kline + snapshot
    kl = json.loads((HERE / 'raw/kline_600000_SH_none.json')
                    .read_text('utf-8'))
    snap = json.loads((HERE / 'raw/snapshot_600000.json')
                      .read_text('utf-8'))
    v = kl['Value']['600000.SH']
    check('type=1 field inventory (no preclose)',
          set(v.keys()) == {'ErrorId', 'Date', 'Time', 'Open', 'High',
                            'Low', 'Close', 'Volume', 'Amount',
                            'VolInStock', 'ForwardFactor'})
    check('snapshot has LastClose', 'LastClose' in snap)
    check('snapshot LastClose == 9.06', snap['LastClose'] == '9.06')

    # issue table rows — .day raw values embedded from gen-3 member
    day_rows = {'20260916': (72340449, 656348160.0),
                '20260917': (45671103, 414887072.0),
                '20260918': (51759299, 469969408.0)}
    for i, dt in enumerate(v['Date']):
        if dt in day_rows:
            vol, amt = day_rows[dt]
            check(f'{dt} volume: f32(.day)==TQ',
                  f32(float(vol)) == float(v['Volume'][i]),
                  f"{vol} -> {f32(float(vol))} vs {v['Volume'][i]}")
            bound = ulp(amt) / 2 + 50.0
            check(f'{dt} amount: .day/1e4 ≈ TQ万元',
                  abs(amt - float(v['Amount'][i]) * 10000) <= bound,
                  f"{amt} vs {v['Amount'][i]}")
    check('snapshot volume == .day//100 (手)',
          int(snap['Volume']) == 51759299 // 100)
    check('snapshot amount*1e4 ≈ .day amount',
          abs(float(snap['Amount']) * 10000 - 469969408.0) <=
          ulp(469969408.0) / 2 + 50.0)

    # full corpus if hsjday provided
    if args.hsjday:
        z = zipfile.ZipFile(args.hsjday)
        raw217 = (HERE.parent / 'issue-217-gen3-acquisition' / 'tq-raw')
        SEC = [('sh', '600004'), ('sh', '600006'), ('sz', '000001'),
               ('sz', '000002'), ('sz', '000035'), ('sh', '688318')]
        tot = vexact = vf32 = aok = 0
        for m, c in SEC:
            day = z.read(f'{m}/lday/{m}{c}.day')
            dmap = {}
            for off in range(0, len(day), 32):
                d = struct.unpack_from('<I', day, off)[0]
                amt = struct.unpack_from('<f', day, off + 20)[0]
                vol = struct.unpack_from('<I', day, off + 24)[0]
                dmap[d] = (vol, amt)
            tq = json.loads((raw217 /
                f'kline_{c}_{m.upper()}_none.json').read_text('utf-8'))
            vv = tq['Value'][f'{c}.{m.upper()}']
            for i in range(len(vv['Date'])):
                d = int(vv['Date'][i])
                if d not in dmap:
                    continue
                tot += 1
                vol, amt = dmap[d]
                if vol == float(vv['Volume'][i]):
                    vexact += 1
                if f32(float(vol)) == float(vv['Volume'][i]):
                    vf32 += 1
                if abs(amt - float(vv['Amount'][i]) * 10000) <= \
                        ulp(amt) / 2 + 50.0:
                    aok += 1
        check('corpus volume f32-rule 100%', vf32 == tot,
              f'{vf32}/{tot}')
        check('corpus amount bound >= 99.8%', aok >= tot * 0.998,
              f'{aok}/{tot}')
    else:
        print('(full-corpus check skipped: pass --hsjday <zip>)')

    print()
    if FAIL:
        print(f'{len(FAIL)} FAIL')
        sys.exit(1)
    print('all checks PASS')


if __name__ == '__main__':
    main()
