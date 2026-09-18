#!/usr/bin/env python3
"""Issue #54 — TQ oracle discrimination harness. DIAGNOSTIC ONLY.

Three modes:
  extract  — decode the pinned gen-2 gbbq member, emit all events of the
             frozen 7-security oracle set plus .day context -> inputs file
  compare  — load a captured TQ/client oracle (issue54.tq-oracle.v1) and
             emit the discrimination matrix -> matrix file
  selftest — run compare against a mechanical-fixture oracle generated
             from a chosen (mapping, interpretation); proves comparator
             mechanics only, never semantic truth.

Never a verdict: output status is always DIAGNOSTIC_ONLY /
GBBQ_FLOAT32_BUSINESS_DECIMAL_NORMALIZATION_NOT_VERIFIED.
"""
import argparse
import hashlib
import importlib.util
import json
import struct
import sys
import zipfile
from decimal import Decimal as D, localcontext, ROUND_HALF_UP
from pathlib import Path

HERE = Path(__file__).resolve().parent
COMMUNITY = HERE.parent / 'issue-3-tdx-gbbq-community'
MEMBER = Path('/home/box/retained-inputs/official-gbbq-member')
DECODER_SRC = Path('/home/box/retained-inputs/gbbq_reader.py')
HSJDAY = Path('/home/box/acceptance-run-issue3/snapshot/hsjday.zip')
PINS = {
    'member_sha256': '4a3e9b862247a8a3fec406cc25b9a5a6ee3303f740780822519c99decaa09d77',
    'member_bytes': 5967508,
    'member_count': 205776,
    'decoder_sha256': 'e28fae76760b1247a960cab285aa357c2d263b3212e7cabfb3f1ebfcc127d6ba',
    'hsjday_sha256': 'c82f852a7bc2e52c7e89347a1e0d1878b01ce997946c2745f7af323752270892',
}
SECURITIES = ('sh.600004', 'sh.600006', 'sz.000001', 'sz.000002',
              'sz.000004', 'sz.000035', 'sh.688318')
INTERPRETATIONS = ('frozen_repr', 'exact_binary', 'shortest_roundtrip',
                   'fixed_4dp', 'fixed_6dp')
ORACLE_FIELDS = ('bonus', 'allot_price', 'share_bonus', 'allotment')
SELFTEST_MAPPING = {'bonus': 0, 'share_bonus': 1,
                    'allotment': 2, 'allot_price': 3}
SELFTEST_INTERP = 'shortest_roundtrip'
PRECISION = 60


def sha(data):
    return hashlib.sha256(data).hexdigest()


def encode(value):
    return (json.dumps(value, indent=1, sort_keys=True,
                       ensure_ascii=False) + '\n').encode()


def shortest(value):
    for digits in range(1, 10):
        candidate = D(format(value, f'.{digits}g'))
        if struct.pack('<f', float(candidate)) == struct.pack('<f', value):
            return candidate
    raise ValueError('float32 roundtrip unavailable')


def interpret(values, name):
    if name == 'frozen_repr':
        return [D(repr(v)) for v in values]
    if name == 'exact_binary':
        return [D.from_float(v) for v in values]
    if name == 'shortest_roundtrip':
        return [shortest(v) for v in values]
    digits = int(name.split('_')[1][:-2])
    return [D.from_float(v).quantize(D(10) ** -digits,
                                     rounding=ROUND_HALF_UP)
            for v in values]


def load_decoder():
    source = DECODER_SRC.read_bytes()
    assert sha(source) == PINS['decoder_sha256'], 'decoder source pin'
    spec = importlib.util.spec_from_file_location(
        'community_gbbq_decoder', COMMUNITY / 'decode_gbbq.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module, module.key_from_source(source)


def decode_member():
    raw = MEMBER.read_bytes()
    assert len(raw) == PINS['member_bytes'] and sha(raw) == PINS['member_sha256'], 'member pin'
    module, key = load_decoder()
    count = struct.unpack_from('<I', raw, 0)[0]
    assert count == PINS['member_count']
    events = {s: [] for s in SECURITIES}
    for index in range(count):
        rec = module.parse_clear_record(
            module.decode_encrypted_record(
                raw[4 + index * 29:4 + (index + 1) * 29], key), index)
        symbol = ('sh' if rec['market'] == 1 else 'sz') + '.' + rec['code']
        if symbol in events:
            events[symbol].append(rec)
    return events


def day_members():
    with open(HSJDAY, 'rb') as stream:
        assert hashlib.file_digest(stream, 'sha256').hexdigest() \
            == PINS['hsjday_sha256'], 'hsjday pin'
    out = {}
    with zipfile.ZipFile(HSJDAY) as archive:
        for symbol in SECURITIES:
            market, code = symbol.split('.')
            data = archive.read(f'{market}\\lday\\{market}{code}.day')
            assert len(data) % 32 == 0
            rows = []
            for off in range(0, len(data), 32):
                dr, o, h, l, c = struct.unpack_from('<5I', data, off)
                rows.append({'date_raw': dr,
                             'date': f'{dr // 10000:04d}-{dr // 100 % 100:02d}-{dr % 100:02d}',
                             'open': D(o) / 100, 'high': D(h) / 100,
                             'low': D(l) / 100, 'close': D(c) / 100})
            out[symbol] = rows
    return out


def extract(output):
    events = decode_member()
    days = day_members()
    payload = {'schema': 'issue54.oracle-inputs.v1', 'pins': PINS,
               'securities': {}}
    for symbol in SECURITIES:
        rows = []
        for rec in events[symbol]:
            prior = [r for r in days[symbol]
                     if r['date_raw'] < rec['date_raw_int']]
            rows.append({
                'record_index': rec['record_index'],
                'date': rec['date_iso'], 'category': rec['category'],
                'float32_repr': [repr(v)
                                 for v in rec['float32_repr_diagnostic']],
                'raw_bits': rec['float32_raw_bits_hex'],
                'prev_date': prior[-1]['date'] if prior else None,
                'prev_close': str(prior[-1]['close']) if prior else None,
            })
        payload['securities'][symbol] = {
            'events': rows,
            'day': [{'date': r['date'], 'close': str(r['close'])}
                    for r in days[symbol]],
        }
    output.write_bytes(encode(payload))
    print(f'extracted {sum(len(v["events"]) for v in payload["securities"].values())} events -> {output}')


def chain_factors(events, interp):
    """Per category=1 event: affine map x -> x*A + B from the frozen
    community reference formula (prev_close*10 - c1 + c4*c2)/(10+c4+c3).
    Returns {date: (A,B)} for events with date set."""
    factors = {}
    for e in events:
        if e['category'] != 1 or e['date'] is None:
            continue
        c1, c2, c3, c4 = interpret(
            [float(v) for v in e['float32_repr']], interp)
        a = D(10) / (10 + c4 + c3)
        b = (c4 * c2 - c1) / (10 + c4 + c3)
        factors[e['date']] = (a, b)
    return factors


def front_adjusted(day_rows, factors):
    """Predicted front-adjusted close for each .day row: compose the
    affine maps of every category=1 event strictly after the bar date."""
    out = {}
    dates = sorted(factors)
    for row in day_rows:
        x = D(row['close'])
        for d in dates:
            if d > row['date']:
                a, b = factors[d]
                x = x * a + b
        out[row['date']] = x
    return out


def compare(inputs, oracle):
    assert oracle.get('schema') == 'issue54.tq-oracle.v1', 'oracle schema'
    kind = (oracle.get('generator') or {}).get('kind')
    assert kind in ('tq_api', 'client_manual', 'mechanical_fixture')
    secs = inputs['securities']
    matrix = {'axis_a_field_mapping': [], 'axis_b_interpretation': {},
              'axis_c_qfq_chain': {}, 'presence': []}

    # ---- axes A+B: per (oracle_field, c_field, interp) exact-match rates
    interp_scores = {n: [0, 0] for n in INTERPRETATIONS}
    for symbol, osec in oracle.get('securities', {}).items():
        if symbol not in secs:
            continue
        by_key = {}
        for e in secs[symbol]['events']:
            if e['date'] is not None:
                by_key.setdefault((e['date'], e['category']),
                                  []).append(e)
        for row in osec.get('divid_factors', []):
            fields0 = row.get('fields') or {}
            try:
                otype = int(str(fields0.get('type'))) \
                    if fields0.get('type') is not None else 1
            except ValueError:
                otype = 1
            candidates = by_key.get((row.get('date'), otype), [])
            # member can hold several records for one (date, category);
            # pick the last and record the ambiguity verbatim
            event = candidates[-1] if candidates else None
            matrix['presence'].append({
                'symbol': symbol, 'date': row.get('date'),
                'oracle_present': True,
                'member_present': event is not None,
                'category': event['category'] if event else None,
                'same_key_records': len(candidates)})
            if event is None:
                continue
            values = [float(v) for v in event['float32_repr']]
            fields = row.get('fields') or {}
            for of in ORACLE_FIELDS:
                ov = fields.get(of)
                if ov is None:
                    continue
                od = D(str(ov))
                for cf in range(4):
                    for name in INTERPRETATIONS:
                        got = interpret(values, name)[cf]
                        match = got == od
                        matrix['axis_a_field_mapping'].append({
                            'symbol': symbol, 'date': row['date'],
                            'oracle_field': of, 'c_field': f'C{cf + 1}',
                            'interpretation': name,
                            'oracle': str(od), 'member': str(got),
                            'match': match})
                        interp_scores[name][1] += 1
                        if match:
                            interp_scores[name][0] += 1
    matrix['axis_b_interpretation'] = {
        n: {'exact': s[0], 'total': s[1],
            'rate': str(D(s[0]) / s[1]) if s[1] else None}
        for n, s in interp_scores.items()}

    # ---- axis C: predicted front-adjusted close vs oracle per interp
    for symbol, osec in oracle.get('securities', {}).items():
        if symbol not in secs:
            continue
        oracle_rows = {r['date']: D(str(r['close']))
                       for r in osec.get('front_adjusted', [])
                       if r.get('close') is not None}
        if not oracle_rows:
            continue
        day_rows = secs[symbol]['day']
        for name in INTERPRETATIONS:
            factors = chain_factors(secs[symbol]['events'], name)
            predicted = front_adjusted(day_rows, factors)
            exact = within = total = 0
            misses = []
            for date, oclose in oracle_rows.items():
                if date not in predicted:
                    misses.append({'date': date, 'kind': 'no_day_bar'})
                    continue
                total += 1
                diff = abs(predicted[date] - oclose)
                if diff == 0:
                    exact += 1
                elif diff <= D('0.005'):
                    within += 1
                else:
                    misses.append({'date': date,
                                   'predicted': str(predicted[date]),
                                   'oracle': str(oclose)})
            matrix['axis_c_qfq_chain'].setdefault(name, {})[symbol] = {
                'oracle_rows': len(oracle_rows), 'compared': total,
                'exact': exact, 'within_0p005': within,
                'misses': misses[:10]}
    return {
        'schema': 'issue54.oracle-matrix.v1',
        'status': 'GBBQ_FLOAT32_BUSINESS_DECIMAL_NORMALIZATION_NOT_VERIFIED',
        'verdict': 'DIAGNOSTIC_ONLY',
        'oracle_generator': oracle.get('generator'),
        'matrix': matrix,
    }


def selftest(inputs):
    """Mechanical-fixture oracle generated under SELFTEST_MAPPING +
    SELFTEST_INTERP. Comparator must recover exactly that interpretation
    as the unique top scorer on axis B and exact chain match on axis C."""
    secs = inputs['securities']
    oracle = {'schema': 'issue54.tq-oracle.v1',
              'generator': {'kind': 'mechanical_fixture',
                            'note': 'selftest only, not official'},
              'securities': {}}
    for symbol in ('sz.000004', 'sz.000001', 'sh.688318', 'sz.000002'):
        entries = []
        for e in secs[symbol]['events']:
            if e['category'] != 1 or e['date'] is None:
                continue
            cs = interpret([float(v) for v in e['float32_repr']],
                           SELFTEST_INTERP)
            fields = {'type': '1'}
            fields.update({of: str(cs[cf]) for of, cf in
                           SELFTEST_MAPPING.items()})
            entries.append({'date': e['date'], 'fields': fields})
        factors = chain_factors(secs[symbol]['events'], SELFTEST_INTERP)
        predicted = front_adjusted(secs[symbol]['day'], factors)
        sample = sorted(predicted)[-40:]
        oracle['securities'][symbol] = {
            'divid_factors': entries,
            'front_adjusted': [{'date': d, 'close': str(predicted[d])}
                               for d in sample],
            'unadjusted': []}
    result = compare(inputs, oracle)
    scores = result['matrix']['axis_b_interpretation']
    best = max(s['exact'] for s in scores.values())
    tied = [n for n, s in scores.items() if s['exact'] == best]
    # every generating-pair entry must match under the generating interp
    gen_pairs = {(of, f'C{cf + 1}') for of, cf in SELFTEST_MAPPING.items()}
    gen_entries = [e for e in result['matrix']['axis_a_field_mapping']
                   if (e['oracle_field'], e['c_field']) in gen_pairs
                   and e['interpretation'] == SELFTEST_INTERP]
    chains = result['matrix']['axis_c_qfq_chain'].get(
        SELFTEST_INTERP, {})
    ok = (SELFTEST_INTERP in tied
          and gen_entries and all(e['match'] for e in gen_entries)
          and all(v['exact'] == v['compared'] and v['compared'] > 0
                  for v in chains.values()))
    print(json.dumps({'selftest': 'PASS' if ok else 'FAIL',
                      'top_interpretations_tied': tied,
                      'generating_pairs_matched':
                          f"{sum(e['match'] for e in gen_entries)}/{len(gen_entries)}",
                      'scores': scores,
                      'chain_exact': {s: v['exact']
                                      for s, v in chains.items()}},
                     indent=1))
    if not ok:
        sys.exit(1)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='mode', required=True)
    p = sub.add_parser('extract')
    p.add_argument('output', type=Path)
    p = sub.add_parser('compare')
    p.add_argument('inputs', type=Path)
    p.add_argument('oracle', type=Path)
    p.add_argument('output', type=Path)
    p = sub.add_parser('selftest')
    p.add_argument('inputs', type=Path)
    args = parser.parse_args()

    if args.mode == 'extract':
        extract(args.output)
        return
    inputs = json.loads(args.inputs.read_bytes())
    assert inputs.get('schema') == 'issue54.oracle-inputs.v1'
    if args.mode == 'selftest':
        selftest(inputs)
        return
    oracle = json.loads(args.oracle.read_bytes())
    result = compare(inputs, oracle)
    args.output.write_bytes(encode(result))
    print(f'matrix -> {args.output} (DIAGNOSTIC_ONLY)')


if __name__ == '__main__':
    main()
