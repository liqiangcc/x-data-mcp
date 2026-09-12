#!/usr/bin/env python3
"""Bounded, offline diagnostic experiment. Never a production normalization API."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import struct
from decimal import Decimal as D, localcontext, ROUND_HALF_UP
import zipfile

HERE = Path(__file__).resolve().parent
OLD = HERE.parent / 'issue-3-cross-stock-qfq'
PLAN_SHA = 'fe0e4a0855f3f4dd0c85a60cd74de41e67e6f5eb80b39d7dd9d484fea75f6106'


def sha(data):
    return hashlib.sha256(data).hexdigest()


def encode(value):
    return (json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + '\n').encode()


def floats(event):
    raw = bytes.fromhex(event['clear_hex'])
    values = struct.unpack('<ffff', raw[13:])
    assert [f'0x{x:08x}' for x in struct.unpack('<IIII', raw[13:])] == event['float32_raw_bits_hex']
    assert list(values) == event['float32_repr_diagnostic']
    return values


def shortest(value):
    for digits in range(1, 10):
        candidate = D(format(value, f'.{digits}g'))
        if struct.pack('<f', float(candidate)) == struct.pack('<f', value):
            return candidate
    raise ValueError('float32 roundtrip unavailable')


def selected():
    assert sha((HERE / 'plan.json').read_bytes()) == PLAN_SHA
    events = [(symbol, event) for symbol, rows in json.loads((OLD / 'selected-events.json').read_bytes())['selected_events'].items() for event in rows if event['category'] == 1]
    events.sort(key=lambda x: (x[1]['market'], x[1]['code'], x[1]['date_iso'], x[1]['record_index']))
    chosen = {}

    def add(pair, reason):
        symbol, event = pair
        key = (symbol, event['record_index'])
        chosen.setdefault(key, {'symbol': symbol, 'event': event, 'selection_reasons': []})['selection_reasons'].append(reason)

    for pair in events:
        if pair[0] == 'sh.600006' and pair[1]['date_iso'] == '2025-07-17':
            add(pair, 'mandatory_half_cent')
    for index in (0, 2, 3):
        matches = [p for p in events if floats(p[1])[index] != 0 and D.from_float(floats(p[1])[index]) != shortest(floats(p[1])[index])]
        for pair in matches[:3]:
            add(pair, f'C{index + 1}_tail')
    for pair in [p for p in events if floats(p[1])[1] != 0 and floats(p[1])[3] != 0][:2]:
        add(pair, 'C2_C4_combination')
    return list(chosen.values())


def extract(archive):
    provenance = json.loads((OLD / 'source-provenance.json').read_bytes())
    with open(archive, 'rb') as stream:
        digest = hashlib.file_digest(stream, 'sha256').hexdigest()
    assert digest == provenance['official_hsjday_zip_sha256']
    rows = selected()
    with zipfile.ZipFile(archive) as z:
        for row in rows:
            symbol, event = row['symbol'], row['event']
            market, code = symbol.split('.')
            member = f'{market}\\lday\\{market}{code}.day'
            data = z.read(member)
            assert len(data) % 32 == 0
            dates = [struct.unpack_from('<I', data, i)[0] for i in range(0, len(data), 32)]
            assert dates == sorted(set(dates))
            prior = [i for i, date in enumerate(dates) if date < event['date_raw_int']]
            row['day_member'] = member
            row['day_member_sha256'] = sha(data)
            row['prior_bar'] = None
            if prior:
                index = prior[-1]
                row['prior_bar'] = {'index': index, 'raw_hex': data[index*32:(index+1)*32].hex(), 'next_raw_hex': data[(index+1)*32:(index+2)*32].hex()}
    return {'schema': 'issue54.bounded-input.v1', 'plan_sha256': PLAN_SHA, 'selected_events_sha256': sha((OLD / 'selected-events.json').read_bytes()), 'source_provenance': provenance, 'rows': rows}


def analyze(inputs):
    expected = selected()
    assert inputs['plan_sha256'] == PLAN_SHA
    assert inputs['source_provenance'] == json.loads((OLD / 'source-provenance.json').read_bytes())
    members = {item['member']: item['day']['sha256'] for item in json.loads((OLD / 'selection.json').read_bytes())['selected']}
    for row in inputs['rows']:
        assert members[row['day_member']] == row['day_member_sha256']
    assert inputs['selected_events_sha256'] == sha((OLD / 'selected-events.json').read_bytes())
    assert [{k: row[k] for k in ('symbol', 'event', 'selection_reasons')} for row in inputs['rows']] == expected
    plan = json.loads((HERE / 'plan.json').read_bytes())
    rows = []
    with localcontext() as context:
        context.prec = plan['calculation']['precision']
        for row in inputs['rows']:
            values = floats(row['event'])
            variants = {'frozen_repr': [D(repr(v)) for v in values], 'exact_binary': [D.from_float(v) for v in values], 'shortest_roundtrip': [shortest(v) for v in values]}
            for digits in (2, 3, 4, 6):
                variants[f'fixed_{digits}dp'] = [D.from_float(v).quantize(D(10) ** -digits, rounding=ROUND_HALF_UP) for v in values]
            assert list(variants) == [c['id'] for c in plan['candidates']]
            prior = row['prior_bar']
            close = None
            date = None
            if prior is not None:
                bar = bytes.fromhex(prior['raw_hex'])
                assert len(bar) == 32
                date, _, _, _, close_int = struct.unpack_from('<IIIII', bar)
                assert date < row['event']['date_raw_int']
                if prior['next_raw_hex']:
                    assert struct.unpack_from('<I', bytes.fromhex(prior['next_raw_hex']))[0] >= row['event']['date_raw_int']
                close = D(close_int) / 100
                assert close > 0
            results = {}
            for name, cs in variants.items():
                result = {'C1_C2_C3_C4': [str(c) for c in cs], 'roundtrip_to_original_bits': [struct.pack('<f', float(c)) == struct.pack('<f', v) for c, v in zip(cs, values)]}
                if close is not None:
                    c1, c2, c3, c4 = cs
                    reference = (close * 10 - c1 + c4 * c2) / (10 + c4 + c3)
                    cent = reference.quantize(D('0.01'), rounding=ROUND_HALF_UP)
                    result.update(reference=str(reference), reference_cent=str(cent), single_event_ratio_6dp=str((cent / close).quantize(D('0.000001'), rounding=ROUND_HALF_UP)))
                results[name] = result
            rows.append({'symbol': row['symbol'], 'date': row['event']['date_iso'], 'record_index': row['event']['record_index'], 'selection_reasons': row['selection_reasons'], 'previous_date': date, 'previous_close': str(close) if close is not None else None, 'calculation_status': 'DIAGNOSTIC_ONLY' if close else 'PREVIOUS_CLOSE_UNRESOLVED', 'candidates': results})
    return {'schema': 'issue54.event-matrix.v1', 'status': 'GBBQ_FLOAT32_BUSINESS_DECIMAL_NORMALIZATION_NOT_VERIFIED', 'official_qfq_contract': 'TDX_OFFICIAL_QFQ_CONTRACT_NOT_PROVEN', 'inputs_sha256': sha(encode(inputs)), 'plan_sha256': PLAN_SHA, 'full_four_stock_revalidation': 'NOT_RUN_NO_OFFICIAL_NORMALIZATION_CONTRACT', 'live_source': 'NOT_RUN', 'row_count': len(rows), 'rows': rows}


def verify_artifacts(hsjday, gbbq, decoder):
    inputs = json.loads((HERE / 'inputs.json').read_bytes())
    assert encode(extract(hsjday)) == encode(inputs), 'bounded .day extraction differs'
    raw = gbbq.read_bytes()
    provenance = inputs['source_provenance']
    assert sha(raw) == provenance['official_gbbq_member_sha256']
    source = decoder.read_bytes()
    assert sha(source) == provenance['community_decoder']['raw_sha256']
    path = HERE.parent / 'issue-3-tdx-gbbq-community' / 'decode_gbbq.py'
    spec = importlib.util.spec_from_file_location('historical_decoder', path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    key = module.key_from_source(source)
    for row in inputs['rows']:
        event = row['event']
        offset = 4 + 29 * event['record_index']
        assert module.decode_encrypted_record(raw[offset:offset+29], key).hex() == event['clear_hex']
    print('Retained official artifact verification PASS: 9 bounded records; decoder remains COMMUNITY evidence')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--extract-hsjday', type=Path)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--verify-artifacts', nargs=3, type=Path, metavar=('HSJDAY', 'GBBQ', 'COMMUNITY_DECODER'))
    args = parser.parse_args()
    if args.verify_artifacts:
        verify_artifacts(*args.verify_artifacts)
        return
    if args.extract_hsjday:
        assert args.output and not args.check
        args.output.write_bytes(encode(extract(args.extract_hsjday)))
        return
    result = encode(analyze(json.loads((HERE / 'inputs.json').read_bytes())))
    if args.check:
        assert result == (HERE / 'matrix.json').read_bytes(), 'matrix differs'
        print('Offline diagnostic replay PASS; official normalization NOT_VERIFIED; live source NOT_RUN')
    elif args.output:
        args.output.write_bytes(result)
    else:
        print(result.decode(), end='')


if __name__ == '__main__':
    main()
