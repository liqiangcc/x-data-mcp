"""Offline scalar diagnostics, NOT execution/approval of upstream Go or pandas."""
import hashlib
import json
import math
from pathlib import Path
import struct
import sys

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent


def sha(data):
    return hashlib.sha256(data).hexdigest()


def round_away(value):
    """Exact ties-away rounding of an already evaluated finite binary64 value."""
    n, d = value.as_integer_ratio()
    q, r = divmod(abs(n), d)
    return math.copysign(q + (2 * r >= d), value)


def text(value):
    return format(value, '.17g')


def injoyai(fields, prices):
    normalized = [round_away(v * 100) / 100 for v in fields]
    c1, c2, c3, c4 = normalized
    m = (10 + c3 + c4) / 10
    c = (c1 - c4 * c2) / 10
    if m == 0:
        m = 1.0  # Preserve upstream behavior; never adopt as production validation.
    a = 1.0 / m
    b = 0.0 - a * c
    return normalized, [round_away((a * p + b) * 100) / 100 for p in prices]


def quantaxis(fields, prices):
    c1, c2, c3, c4 = fields
    previous_close = prices[3]
    reference = (previous_close * 10 - c1 + c4 * c2) / (10 + c4 + c3)
    return reference, [p * (reference / previous_close) for p in prices]


def verified_bytes(path, expected):
    b = path.read_bytes()
    if sha(b) != expected:
        raise ValueError('evidence digest mismatch: ' + str(path))
    return b


def analyze():
    plan_bytes = (HERE / 'plan.json').read_bytes()
    plan = json.loads(plan_bytes)
    for path, digest in plan['inputs'].items():
        verified_bytes(ROOT / path, digest)
    sources = json.loads(verified_bytes(HERE / 'sources.json', plan['sources_sha256']))
    for source in sources:
        verified_bytes(HERE / source['retained'], source['sha256'])
    inputs = json.loads((ROOT / 'evidence/issue-54-gbbq-decimal/inputs.json').read_bytes())
    old = json.loads((ROOT / 'evidence/issue-54-gbbq-decimal/matrix.json').read_bytes())
    baseline = {(r['symbol'], r['date']): r for r in old['rows']}
    rows = []
    for row in inputs['rows']:
        event = row['event']
        fields = [struct.unpack('<f', int(bits, 16).to_bytes(4, 'little'))[0]
                  for bits in event['float32_raw_bits_hex']]
        record = dict(symbol=row['symbol'], date=event['date_iso'],
                      decoded_fields=[text(v) for v in fields],
                      frozen_repr=baseline[(row['symbol'], event['date_iso'])]['candidates']['frozen_repr'])
        prior = row.get('prior_bar')
        if not prior:
            record['status'] = 'UNKNOWN_PREVIOUS_BAR'
            rows.append(record)
            continue
        date, o, h, low, close = struct.unpack('<5I', bytes.fromhex(prior['raw_hex'])[:20])
        assert date < event['date_raw_int'] and close > 0
        prices = [v / 100 for v in (o, h, low, close)]
        normalized, affine = injoyai(fields, prices)
        reference, proportional = quantaxis(fields, prices)
        record.update(status='SCALAR_DIAGNOSTIC_ONLY', prior_date=date,
                      prior_ohlc=[text(p) for p in prices],
                      injoyai_fields_2dp=[text(v) for v in normalized],
                      normalized_bits_preserved=[struct.pack('<f', a) == struct.pack('<f', b)
                                                 for a, b in zip(fields, normalized)],
                      injoyai_affine_ohlc=[text(p) for p in affine],
                      quantaxis_raw_input_reference=text(reference),
                      quantaxis_raw_input_proportional_ohlc=[text(p) for p in proportional])
        rows.append(record)
    return dict(schema='issue90.result.v1', plan_sha256=sha(plan_bytes), rows=rows,
                row_count=len(rows), official_semantics='NOT_VERIFIED',
                upstream_runtime_tests='NOT_RUN_SCALAR_ADAPTATION_ONLY',
                four_stock_full_chain='NOT_RUN', live_source='NOT_RUN',
                production_algorithm_changed=False)


def serialized():
    return (json.dumps(analyze(), indent=2, sort_keys=True) + '\n').encode()


if __name__ == '__main__':
    result = serialized()
    if sys.argv[1:] == ['--check']:
        assert result == (HERE / 'result.json').read_bytes(), 'diagnostic drift'
        print('PASS offline scalar determinism; official semantics NOT_VERIFIED')
    elif sys.argv[1:] == ['--write']:
        (HERE / 'result.json').write_bytes(result)
    else:
        sys.stdout.buffer.write(result)
