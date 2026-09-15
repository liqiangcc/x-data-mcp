#!/usr/bin/env python3
"""Full-domain offline diagnostic scan of the retained GBBQ member.

Never a production normalization API. Decodes every record of the pinned
retained gbbq member with the pinned community decoder source, exhausts all
category=1 records for C1..C4 raw-bits/float32 distribution histograms, and
evaluates the frozen candidate decimal interpretations on every category=1
event of the #53 four selected securities against the retained official .day
members. No network access; all input bytes are hash-pinned before use.
"""
import argparse
import hashlib
import importlib.util
import json
import math
from collections import Counter
from decimal import Decimal as D, localcontext, ROUND_HALF_UP
from pathlib import Path
import struct
import zipfile

HERE = Path(__file__).resolve().parent
CROSS = HERE.parent / 'issue-3-cross-stock-qfq'
COMMUNITY = HERE.parent / 'issue-3-tdx-gbbq-community'

GBBQ_MEMBER_SHA256 = '24f46cecc9b085ee6c0ef126fab33b405b8057747793eb5d25d44e624769ff60'
GBBQ_MEMBER_BYTES = 5966087
GBBQ_COUNT = 205727
RECORD_SIZE = 29
HSJDAY_ZIP_SHA256 = '611ea3ccc1e64717aebc9a14ce9c8b1ed06526b5fd69bd14275fc376f4f44272'
DECODER_RAW_SHA256 = 'e28fae76760b1247a960cab285aa357c2d263b3212e7cabfb3f1ebfcc127d6ba'
DECODER_GIT_BLOB_SHA1 = '0eda4935027ffc79b5c425eff0a70a6d492b21d1'
PLAN_SHA256 = 'df436841a2a11bbae32deaf1b0c166d5c4863065eb99fce13fe8d291d89df40c'
SELECTED = ('sh.600004', 'sh.600006', 'sz.000002', 'sz.000004')
EXPECTED_CATEGORIES = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]
PRECISION = 60


def sha(data):
    return hashlib.sha256(data).hexdigest()


def encode(value):
    return (json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + '\n').encode()


def git_blob_sha1(data):
    return hashlib.sha1(f'blob {len(data)}\0'.encode() + data).hexdigest()


def load_decoder_module():
    path = COMMUNITY / 'decode_gbbq.py'
    spec = importlib.util.spec_from_file_location('merged_gbbq_decoder', path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def shortest(value):
    for digits in range(1, 10):
        candidate = D(format(value, f'.{digits}g'))
        if struct.pack('<f', float(candidate)) == struct.pack('<f', value):
            return candidate
    raise ValueError('float32 roundtrip unavailable')


def candidate_decimals(values):
    variants = {
        'frozen_repr': [D(repr(v)) for v in values],
        'exact_binary': [D.from_float(v) for v in values],
        'shortest_roundtrip': [shortest(v) for v in values],
    }
    for digits in (2, 3, 4, 6):
        variants[f'fixed_{digits}dp'] = [D.from_float(v).quantize(D(10) ** -digits, rounding=ROUND_HALF_UP) for v in values]
    return variants


def field_stats(records):
    """Per-field C1..C4 distribution over category=1 records."""
    stats = []
    for index in range(4):
        counter = Counter()
        info = {}
        zero = nonzero = negative = non_finite = 0
        for record in records:
            bits = record['float32_raw_bits_hex'][index]
            counter[bits] += 1
            value = record['float32_repr_diagnostic'][index]
            if bits not in info:
                short = shortest(value)
                info[bits] = {
                    'float32_repr_diagnostic': repr(value),
                    'exact_binary_decimal': str(D.from_float(value)),
                    'shortest_roundtrip_decimal': str(short),
                    'tail_exact_binary_differs_from_shortest': D.from_float(value) != short,
                }
            if value == 0:
                zero += 1
            else:
                nonzero += 1
            if value < 0:
                negative += 1
            if not math.isfinite(value):
                non_finite += 1
        values = [
            dict(bits=bits, count=counter[bits], **info[bits])
            for bits in sorted(counter)
        ]
        stats.append({
            'field': f'C{index + 1}',
            'record_count': len(records),
            'zero_count': zero,
            'nonzero_count': nonzero,
            'negative_count': negative,
            'non_finite_count': non_finite,
            'distinct_raw_bits_count': len(counter),
            'tail_bits_count': sum(1 for item in values if item['tail_exact_binary_differs_from_shortest']),
            'distinct_values': values,
        })
    return stats


def combination_patterns(records):
    masks = Counter()
    for record in records:
        mask = sum((1 << i) for i in range(4) if record['float32_repr_diagnostic'][i] != 0)
        masks[mask] += 1
    return [
        {
            'mask': mask,
            'nonzero_fields': [f'C{i + 1}' for i in range(4) if mask & (1 << i)],
            'count': masks.get(mask, 0),
        }
        for mask in range(16)
    ]


def symbol_coverage(records):
    coverage = {}
    for record in records:
        key = f"{record['market']}.{record['code']}"
        entry = coverage.setdefault(key, {'count': 0, 'first_date': None, 'last_date': None})
        entry['count'] += 1
        date = record['date_iso']
        if date is not None:
            if entry['first_date'] is None or date < entry['first_date']:
                entry['first_date'] = date
            if entry['last_date'] is None or date > entry['last_date']:
                entry['last_date'] = date
    return {key: coverage[key] for key in sorted(coverage)}


def parse_day_member(data):
    if len(data) % 32:
        raise ValueError('.day length is not divisible by 32')
    rows = []
    for offset in range(0, len(data), 32):
        record = data[offset:offset + 32]
        date_raw, open_raw, high_raw, low_raw, close_raw = struct.unpack_from('<5I', record)
        rows.append({
            'index': offset // 32,
            'date_raw_int': date_raw,
            'date_iso': f'{date_raw // 10000:04d}-{date_raw // 100 % 100:02d}-{date_raw % 100:02d}',
            'raw_hex': record.hex(),
            'open': D(open_raw) / 100,
            'close': D(close_raw) / 100,
        })
    return rows


def event_matrix(events, day_rows):
    """Frozen candidate evaluation for every category=1 event of one security."""
    raw_dates = [row['date_raw_int'] for row in day_rows]
    assert raw_dates == sorted(set(raw_dates)), '.day dates not strictly increasing'
    rows = []
    with localcontext() as context:
        context.prec = PRECISION
        for event in events:
            prior = [row for row in day_rows if row['date_raw_int'] < event['date_raw_int']]
            on_day = [row for row in day_rows if row['date_raw_int'] == event['date_raw_int']]
            previous = prior[-1] if prior else None
            usable = previous is not None and previous['close'] > 0
            variants = candidate_decimals(event['float32_repr_diagnostic'])
            results = {}
            for name, cs in variants.items():
                result = {
                    'C1_C2_C3_C4': [str(c) for c in cs],
                    'roundtrip_to_original_bits': [
                        struct.pack('<f', float(c)) == struct.pack('<f', v)
                        for c, v in zip(cs, event['float32_repr_diagnostic'])
                    ],
                }
                if usable:
                    close = previous['close']
                    c1, c2, c3, c4 = cs
                    reference = (close * 10 - c1 + c4 * c2) / (10 + c4 + c3)
                    cent = reference.quantize(D('0.01'), rounding=ROUND_HALF_UP)
                    result.update(
                        reference=str(reference),
                        reference_cent=str(cent),
                        reference_cent_equals_previous_close=cent == close,
                        single_event_ratio_6dp=str((cent / close).quantize(D('0.000001'), rounding=ROUND_HALF_UP)),
                    )
                results[name] = result
            row = {
                'record_index': event['record_index'],
                'date': event['date_iso'],
                'date_raw_int': event['date_raw_int'],
                'clear_hex': event['clear_hex'],
                'float32_raw_bits_hex': event['float32_raw_bits_hex'],
                'float32_repr_diagnostic': event['float32_repr_diagnostic'],
                'previous_date': previous['date_iso'] if previous else None,
                'previous_close': str(previous['close']) if previous else None,
                'previous_bar_raw_hex': previous['raw_hex'] if previous else None,
                'event_date_in_day': bool(on_day),
                'calculation_status': 'DIAGNOSTIC_ONLY' if usable else ('PREVIOUS_CLOSE_UNRESOLVED' if previous is None else 'PREVIOUS_CLOSE_NONPOSITIVE'),
                'candidates': results,
            }
            if on_day:
                row['event_day_bar'] = {'open': str(on_day[0]['open']), 'close': str(on_day[0]['close']), 'raw_hex': on_day[0]['raw_hex']}
            rows.append(row)
    rows.sort(key=lambda r: r['record_index'])
    return rows


def matrix_summary(rows):
    """Per-candidate machine comparison vs .day observed previous close."""
    candidate_ids = list(next(iter(rows))['candidates']) if rows else []
    summary = {}
    for name in candidate_ids:
        resolved = [row for row in rows if 'reference_cent' in row['candidates'][name]]
        equal = [row for row in resolved if row['candidates'][name]['reference_cent_equals_previous_close']]
        summary[name] = {
            'resolved_count': len(resolved),
            'unresolved_count': len(rows) - len(resolved),
            'reference_cent_equals_previous_close_count': len(equal),
            'reference_cent_differs_count': len(resolved) - len(equal),
        }
    disagreements = []
    for row in rows:
        cents = {row['candidates'][name].get('reference_cent') for name in candidate_ids}
        cents.discard(None)
        if len(cents) > 1:
            disagreements.append({'record_index': row['record_index'], 'date': row['date'], 'distinct_reference_cents': sorted(cents)})
    return {'per_candidate': summary, 'candidate_disagreement_events': disagreements, 'candidate_disagreement_count': len(disagreements)}


def run(gbbq_path, decoder_source_path, hsjday_path):
    plan_bytes = (HERE / 'plan.json').read_bytes()
    assert sha(plan_bytes) == PLAN_SHA256, 'plan.json hash drifted from frozen value'
    plan = json.loads(plan_bytes)
    pinned = plan['inputs']['committed_evidence']
    selected_events_bytes = (CROSS / 'selected-events.json').read_bytes()
    selection_bytes = (CROSS / 'selection.json').read_bytes()
    assert sha(selected_events_bytes) == pinned['selected_events_sha256'], 'committed selected-events.json hash mismatch'
    assert sha(selection_bytes) == pinned['selection_sha256'], 'committed selection.json hash mismatch'
    selection = json.loads(selection_bytes)
    committed_events = json.loads(selected_events_bytes)['selected_events']

    decoder_source = decoder_source_path.read_bytes()
    assert sha(decoder_source) == DECODER_RAW_SHA256, 'community decoder source hash mismatch'
    assert git_blob_sha1(decoder_source) == DECODER_GIT_BLOB_SHA1, 'community decoder blob sha1 mismatch'

    gbbq = gbbq_path.read_bytes()
    assert len(gbbq) == GBBQ_MEMBER_BYTES, 'retained gbbq member length mismatch'
    assert sha(gbbq) == GBBQ_MEMBER_SHA256, 'retained gbbq member hash mismatch'

    decoder = load_decoder_module()
    key = decoder.key_from_source(decoder_source)
    count = struct.unpack_from('<I', gbbq, 0)[0]
    assert count == GBBQ_COUNT, 'gbbq record count header mismatch'
    assert len(gbbq) == 4 + count * RECORD_SIZE, 'gbbq structural length mismatch'

    category_counts = Counter()
    category1_records = []
    six_digit_cat1 = 0
    target_records = {symbol: [] for symbol in SELECTED}
    for index in range(count):
        encrypted = gbbq[4 + index * RECORD_SIZE:4 + (index + 1) * RECORD_SIZE]
        record = decoder.parse_clear_record(decoder.decode_encrypted_record(encrypted, key), index)
        category_counts[record['category']] += 1
        if record['category'] != 1:
            continue
        category1_records.append(record)
        if len(record['code']) == 6 and record['code'].isdigit():
            six_digit_cat1 += 1
        symbol = f"{record['market']}.{record['code']}"
        if symbol in target_records:
            target_records[symbol].append(record)

    assert sorted(category_counts) == EXPECTED_CATEGORIES, 'decoded category set differs from committed structure summary'
    assert six_digit_cat1 == selection['category1_decoded_total'] == pinned['category1_six_digit_decoded_total'], 'category=1 six-digit total differs from committed #53 selection count'

    for symbol in SELECTED:
        fresh = {e['record_index']: e['clear_hex'] for e in target_records[symbol]}
        committed = {e['record_index']: e['clear_hex'] for e in committed_events[symbol] if e['category'] == 1}
        assert fresh == committed, f'fresh decode differs from committed #53 events for {symbol}'

    with open(hsjday_path, 'rb') as stream:
        hsjday_sha = hashlib.file_digest(stream, 'sha256').hexdigest()
    assert hsjday_sha == HSJDAY_ZIP_SHA256, 'retained hsjday.zip hash mismatch'
    day = {}
    day_member_meta = {}
    with zipfile.ZipFile(hsjday_path) as archive:
        for symbol in SELECTED:
            item = next(s for s in selection['selected'] if f"{s['market']}.{s['code']}" == symbol)
            member = item['member']
            data = archive.read(member)
            assert sha(data) == item['day']['sha256'], f'{member} sha256 mismatch'
            day[symbol] = parse_day_member(data)
            day_member_meta[member] = {'bytes': len(data), 'sha256': sha(data), 'record_count': len(day[symbol])}
    matrices = {symbol: event_matrix(target_records[symbol], day[symbol]) for symbol in SELECTED}

    result = {
        'schema': 'issue54.full-events-scan.v1',
        'status': 'GBBQ_FLOAT32_BUSINESS_DECIMAL_NORMALIZATION_NOT_VERIFIED',
        'official_qfq_contract': 'TDX_OFFICIAL_QFQ_CONTRACT_NOT_PROVEN',
        'evidence_levels': {
            'official_bytes': 'retained gbbq member and hsjday.zip hashes verified before use',
            'decoder': 'community reverse-engineering (pytdx pinned commit); not an official TDX contract',
            'field_mapping': 'community/research assumption C1..C4 = fenhong/peigujia/songzhuangu/peigu; NOT_VERIFIED',
            'candidates_and_calculations': 'frozen diagnostic research rules, not official semantics',
        },
        'plan_sha256': PLAN_SHA256,
        'inputs': {
            'gbbq_member_sha256': sha(gbbq),
            'gbbq_member_bytes': len(gbbq),
            'gbbq_count_u32_le': count,
            'decoder_source_sha256': sha(decoder_source),
            'hsjday_zip_sha256': hsjday_sha,
            'day_members': day_member_meta,
            'selected_events_sha256': sha(selected_events_bytes),
        },
        'decode_summary': {
            'decoded_count': count,
            'category_counts': {str(k): category_counts[k] for k in sorted(category_counts)},
            'category1_record_count': len(category1_records),
            'category1_six_digit_code_count': six_digit_cat1,
        },
        'histograms': {
            'per_field': field_stats(category1_records),
            'field_combination_patterns': combination_patterns(category1_records),
            'symbol_coverage': symbol_coverage(category1_records),
        },
        'four_security_matrices': {
            symbol: {
                'event_count': len(matrices[symbol]),
                'summary': matrix_summary(matrices[symbol]),
                'rows': matrices[symbol],
            }
            for symbol in SELECTED
        },
        'full_four_stock_revalidation': 'NOT_RUN_NO_OFFICIAL_NORMALIZATION_CONTRACT',
        'live_source': 'NOT_RUN',
    }
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--gbbq', type=Path, help='retained gbbq member (5966087 bytes)')
    parser.add_argument('--decoder-source', type=Path, help='pinned pytdx gbbq_reader.py')
    parser.add_argument('--hsjday', type=Path, help='retained hsjday.zip')
    parser.add_argument('--output', type=Path)
    parser.add_argument('--check', action='store_true', help='compare regenerated result with committed result.json')
    args = parser.parse_args()
    for flag in ('gbbq', 'decoder_source', 'hsjday'):
        if getattr(args, flag) is None:
            parser.error(f'--{flag.replace("_", "-")} is required')
    result = encode(run(args.gbbq, args.decoder_source, args.hsjday))
    if args.check:
        assert result == (HERE / 'result.json').read_bytes(), 'result differs'
        print('Offline full-domain replay PASS; official normalization NOT_VERIFIED; live source NOT_RUN')
    elif args.output:
        args.output.write_bytes(result)
    else:
        print(result.decode(), end='')


if __name__ == '__main__':
    main()
