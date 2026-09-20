#!/usr/bin/env python3
"""Issue #54 — discrimination analysis over a produced oracle matrix.

Reads a matrix file emitted by `oracle_compare.py compare` (schema
issue54.oracle-matrix.v1) and emits the mechanical discrimination
findings: canonical field mapping, per-candidate interpretation scores
on the mapped pairs, 2dp-resolution rate of residual misses, and the
axis-C chain scoreboard. DIAGNOSTIC_ONLY — never emits a verdict.
"""
import json
import sys
from collections import defaultdict
from decimal import Decimal as D, ROUND_HALF_UP
from pathlib import Path

MAPPED = {('bonus', 'C1'), ('allot_price', 'C2'),
          ('share_bonus', 'C3'), ('allotment', 'C4')}


def nz(value):
    try:
        return D(str(value)) != 0
    except Exception:
        return False


def analyse(matrix_path):
    doc = json.loads(Path(matrix_path).read_text())
    assert doc.get('schema') == 'issue54.oracle-matrix.v1', 'matrix schema'
    matrix = doc['matrix']

    # ---- axis A: mapping support on oracle-non-zero entries
    pair_support = defaultdict(lambda: [0, 0])
    for e in matrix['axis_a_field_mapping']:
        if not nz(e['oracle']) or e['interpretation'] != 'shortest_roundtrip':
            continue
        pair_support[(e['oracle_field'], e['c_field'])][bool(e['match'])] += 1

    # ---- axis B: per-candidate score on the four mapped pairs, plus the
    # 2dp-resolution rate of the residual misses (display-precision proof)
    mapped_scores = defaultdict(lambda: [0, 0])
    resolved_2dp = defaultdict(int)
    residual = []
    for e in matrix['axis_a_field_mapping']:
        pair = (e['oracle_field'], e['c_field'])
        if pair not in MAPPED or not nz(e['oracle']):
            continue
        mapped_scores[e['interpretation']][bool(e['match'])] += 1
        if e['interpretation'] == 'shortest_roundtrip' and not e['match']:
            oracle, member = D(e['oracle']), D(e['member'])
            if member.quantize(D('0.01'), rounding=ROUND_HALF_UP) == oracle:
                resolved_2dp[e['oracle_field']] += 1
            else:
                residual.append({'symbol': e['symbol'], 'date': e['date'],
                                 'pair': list(pair), 'oracle': e['oracle'],
                                 'member': e['member']})

    # ---- axis C: per-candidate chain scoreboard
    chain = {}
    for name, per in matrix['axis_c_qfq_chain'].items():
        compared = sum(v['compared'] for v in per.values())
        within = sum(v['within_0p005'] for v in per.values())
        exact = sum(v['exact'] for v in per.values())
        no_bar = sum(1 for v in per.values()
                     for miss in v['misses']
                     if miss.get('kind') == 'no_day_bar')
        chain[name] = {'compared': compared, 'within_0p005': within,
                       'exact': exact, 'no_day_bar_misses_in_sample': no_bar,
                       'within_rate': str(D(within) / compared)}

    return {
        'schema': 'issue54.discrimination.v1',
        'verdict': 'DIAGNOSTIC_ONLY',
        'status': 'GBBQ_FLOAT32_BUSINESS_DECIMAL_NORMALIZATION_NOT_VERIFIED',
        'axis_a_mapping_support': {
            f'{of}x{cf}': {'miss': v[0], 'match': v[1]}
            for (of, cf), v in sorted(pair_support.items())},
        'axis_b_mapped_pair_scores': {
            name: {'miss': v[0], 'match': v[1]}
            for name, v in sorted(mapped_scores.items())},
        'mapped_pair_misses_resolved_at_2dp': dict(resolved_2dp),
        'mapped_pair_residual_after_2dp': residual,
        'axis_c_chain': chain,
    }


if __name__ == '__main__':
    if len(sys.argv) != 3:
        raise SystemExit('usage: discriminate.py <matrix.json> <out.json>')
    out = analyse(sys.argv[1])
    Path(sys.argv[2]).write_text(
        json.dumps(out, indent=1, sort_keys=True, ensure_ascii=False) + '\n')
    print(f"discrimination -> {sys.argv[2]} (DIAGNOSTIC_ONLY)")
