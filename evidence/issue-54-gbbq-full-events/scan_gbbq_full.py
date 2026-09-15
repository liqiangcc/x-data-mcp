#!/usr/bin/env python3
"""Full-domain offline diagnostic scan of a pinned GBBQ member generation.

Never a production normalization API. Decodes every record of the pinned
gbbq member with the pinned community decoder source, exhausts all
category=1 records for C1..C4 raw-bits/float32 distribution histograms, and
evaluates the frozen candidate decimal interpretations on every category=1
event of the #53 four selected securities against the pinned official .day
members. No network access; all input bytes are hash-pinned before use.

Two frozen plans are registered in KNOWN_PLANS:
- plan.json  : gen-1 retained snapshot pins (2026-09-13); cross_checks demand
               committed #53 events reproduce at identical record_index.
- plan-v2.json: gen-2 re-downloaded snapshot (2026-09-14 archive generation,
               authorized by issue #71 comment-5676627357); consistency is
               presence-based -- every committed clear_hex must exist in the
               member, index shifts and divergences are recorded as findings.
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
BOUNDED_INPUTS = HERE.parent / 'issue-54-gbbq-decimal' / 'inputs.json'

KNOWN_PLANS = {
    'plan.json': 'df436841a2a11bbae32deaf1b0c166d5c4863065eb99fce13fe8d291d89df40c',
    'plan-v2.json': '23ce0e91352d85e6832d16b6b1060caa1e435a62d2d6c3caaaace12698f2721a',
}
RECORD_SIZE = 29
SELECTED = ('sh.600004', 'sh.600006', 'sz.000002', 'sz.000004')
MARKET_PREFIX = {0: 'sz', 1: 'sh'}
EXPECTED_CATEGORIES = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]
PRECISION = 60


def symbol_key(record):
    prefix = MARKET_PREFIX.get(record['market'])
    return None if prefix is None else f"{prefix}.{record['code']}"


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


def event_matrix(events, day_rows, committed_index_by_hex=None):
    """Frozen candidate evaluation for every category=1 event of one security.

    committed_index_by_hex maps clear_hex -> committed gen-1 record_index so
    gen-2 runs can record the index shift of each committed event.
    """
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
            if committed_index_by_hex is not None:
                committed_index = committed_index_by_hex.get(event['clear_hex'])
                row['committed_record_index'] = committed_index
                row['record_index_shift'] = None if committed_index is None else event['record_index'] - committed_index
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


def run(gbbq_path, decoder_source_path, hsjday_path, plan_name='plan-v2.json'):
    plan_bytes = (HERE / plan_name).read_bytes()
    plan_sha = sha(plan_bytes)
    assert KNOWN_PLANS.get(plan_name) == plan_sha, f'{plan_name} hash drifted from frozen value'
    plan = json.loads(plan_bytes)
    pins = plan['inputs']
    pinned = pins['committed_evidence']
    presence_mode = plan.get('cross_check', {}).get('event_reproduction') == 'presence'

    selected_events_bytes = (CROSS / 'selected-events.json').read_bytes()
    selection_bytes = (CROSS / 'selection.json').read_bytes()
    assert sha(selected_events_bytes) == pinned['selected_events_sha256'], 'committed selected-events.json hash mismatch'
    assert sha(selection_bytes) == pinned['selection_sha256'], 'committed selection.json hash mismatch'
    selection = json.loads(selection_bytes)
    committed_events = json.loads(selected_events_bytes)['selected_events']

    bounded_events = None
    if pinned.get('bounded_inputs_sha256'):
        bounded_bytes = BOUNDED_INPUTS.read_bytes()
        assert sha(bounded_bytes) == pinned['bounded_inputs_sha256'], 'bounded inputs.json hash mismatch'
        bounded_events = [row['event'] for row in json.loads(bounded_bytes)['rows']]

    decoder_source = decoder_source_path.read_bytes()
    decoder_pin = pins['community_decoder_source']
    assert sha(decoder_source) == decoder_pin['raw_sha256'], 'community decoder source hash mismatch'
    assert git_blob_sha1(decoder_source) == decoder_pin['git_blob_sha1'], 'community decoder blob sha1 mismatch'

    gbbq = gbbq_path.read_bytes()
    member_pin = pins['official_gbbq_member']
    assert len(gbbq) == member_pin['bytes'], 'retained gbbq member length mismatch'
    assert sha(gbbq) == member_pin['sha256'], 'retained gbbq member hash mismatch'

    decoder = load_decoder_module()
    key = decoder.key_from_source(decoder_source)
    count = struct.unpack_from('<I', gbbq, 0)[0]
    assert count == member_pin['count_u32_le'], 'gbbq record count header mismatch'
    assert len(gbbq) == 4 + count * RECORD_SIZE, 'gbbq structural length mismatch'

    category_counts = Counter()
    category1_records = []
    cat1_index_by_hex = {}
    six_digit_cat1 = 0
    target_records = {symbol: [] for symbol in SELECTED}
    for index in range(count):
        encrypted = gbbq[4 + index * RECORD_SIZE:4 + (index + 1) * RECORD_SIZE]
        record = decoder.parse_clear_record(decoder.decode_encrypted_record(encrypted, key), index)
        category_counts[record['category']] += 1
        if record['category'] != 1:
            continue
        category1_records.append(record)
        cat1_index_by_hex[record['clear_hex']] = index
        if len(record['code']) == 6 and record['code'].isdigit():
            six_digit_cat1 += 1
        symbol = symbol_key(record)
        if symbol in target_records:
            target_records[symbol].append(record)

    findings = []
    category_set_matches = sorted(category_counts) == EXPECTED_CATEGORIES
    if not category_set_matches:
        findings.append({'kind': 'CATEGORY_SET_DIFFERS_FROM_COMMITTED', 'observed_categories': sorted(category_counts)})
        assert presence_mode, 'decoded category set differs from committed structure summary'

    cat1_total_check = {
        'observed': six_digit_cat1,
        'committed_total': pinned['category1_six_digit_decoded_total'],
        'selection_total': selection['category1_decoded_total'],
        'delta_vs_committed': six_digit_cat1 - pinned['category1_six_digit_decoded_total'],
    }
    if cat1_total_check['delta_vs_committed'] != 0:
        findings.append({'kind': 'CATEGORY1_SIX_DIGIT_TOTAL_DIFFERS', **cat1_total_check})
        assert presence_mode, 'category=1 six-digit total differs from committed #53 selection count'

    reproduction = []
    for symbol in SELECTED:
        for event in committed_events[symbol]:
            if event['category'] != 1:
                continue
            observed_index = cat1_index_by_hex.get(event['clear_hex'])
            item = {
                'symbol': symbol,
                'date': event['date_iso'],
                'committed_record_index': event['record_index'],
                'observed_record_index': observed_index,
                'present': observed_index is not None,
                'same_index': observed_index == event['record_index'],
                'index_shift': None if observed_index is None else observed_index - event['record_index'],
            }
            reproduction.append(item)
            if observed_index is None:
                findings.append({'kind': 'COMMITTED_EVENT_ABSENT_IN_MEMBER', 'symbol': symbol,
                                 'date': event['date_iso'], 'record_index': event['record_index']})

    if not presence_mode:
        for symbol in SELECTED:
            fresh = {e['record_index']: e['clear_hex'] for e in target_records[symbol]}
            committed = {e['record_index']: e['clear_hex'] for e in committed_events[symbol] if e['category'] == 1}
            assert fresh == committed, f'fresh decode differs from committed #53 events for {symbol}'

    bounded_check = None
    if bounded_events is not None:
        detail = [
            {'date': e['date_iso'], 'committed_record_index': e['record_index'],
             'observed_record_index': cat1_index_by_hex.get(e['clear_hex']),
             'present': e['clear_hex'] in cat1_index_by_hex}
            for e in bounded_events
        ]
        bounded_check = {'event_count': len(detail), 'present_count': sum(d['present'] for d in detail), 'events': detail}

    hsjday_pin = pins.get('official_hsjday_zip_sha256') or pins['official_hsjday_zip']['sha256']
    with open(hsjday_path, 'rb') as stream:
        hsjday_sha = hashlib.file_digest(stream, 'sha256').hexdigest()
    assert hsjday_sha == hsjday_pin, 'hsjday.zip hash mismatch vs plan pin'
    day = {}
    day_member_meta = {}
    gen1_day_pins = {item['member']: item['day']['sha256'] for item in selection['selected']}
    with zipfile.ZipFile(hsjday_path) as archive:
        for symbol in SELECTED:
            item = next(s for s in selection['selected'] if f"{s['market']}.{s['code']}" == symbol)
            member = item['member']
            data = archive.read(member)
            member_sha = sha(data)
            assert member_sha == pins['day_members'][member], f'{member} sha256 mismatch vs plan pin'
            matches_gen1 = member_sha == gen1_day_pins[member]
            if not matches_gen1:
                findings.append({'kind': 'DAY_MEMBER_DIFFERS_FROM_GEN1_PIN', 'member': member,
                                 'gen1_sha256': gen1_day_pins[member],
                                 'observed_sha256': member_sha, 'observed_bytes': len(data)})
            day[symbol] = parse_day_member(data)
            day_member_meta[member] = {'bytes': len(data), 'sha256': member_sha,
                                       'record_count': len(day[symbol]),
                                       'matches_gen1_selection_pin': matches_gen1}
    committed_index_by_hex = {
        symbol: {e['clear_hex']: e['record_index'] for e in committed_events[symbol] if e['category'] == 1}
        for symbol in SELECTED
    }
    matrices = {symbol: event_matrix(target_records[symbol], day[symbol], committed_index_by_hex[symbol]) for symbol in SELECTED}

    shifted = [r['index_shift'] for r in reproduction if r['present'] and not r['same_index']]
    consistency = {
        'event_reproduction_mode': 'presence' if presence_mode else 'same_index',
        'per_symbol_decoded_counts': {
            symbol: {
                'decoded_category1': len(target_records[symbol]),
                'committed_category1': sum(1 for e in committed_events[symbol] if e['category'] == 1),
            }
            for symbol in SELECTED
        },
        'committed_events': {
            'committed_count': len(reproduction),
            'present_count': sum(r['present'] for r in reproduction),
            'same_index_count': sum(r['same_index'] for r in reproduction),
            'shifted_count': len(shifted),
            'absent_count': sum(1 for r in reproduction if not r['present']),
            'shift_distribution': {str(k): v for k, v in sorted(Counter(shifted).items())},
            'events': reproduction,
        },
        'bounded_nine_events': bounded_check,
        'category_set_matches_committed': category_set_matches,
        'category1_six_digit_total': cat1_total_check,
        'day_members': day_member_meta,
        'findings': findings,
    }

    result = {
        'schema': 'issue54.full-events-scan.v1',
        'status': 'GBBQ_FLOAT32_BUSINESS_DECIMAL_NORMALIZATION_NOT_VERIFIED',
        'official_qfq_contract': 'TDX_OFFICIAL_QFQ_CONTRACT_NOT_PROVEN',
        'evidence_levels': {
            'official_bytes': 'plan-pinned gbbq member and hsjday.zip hashes verified before use',
            'decoder': 'community reverse-engineering (pytdx pinned commit); not an official TDX contract',
            'field_mapping': 'community/research assumption C1..C4 = fenhong/peigujia/songzhuangu/peigu; NOT_VERIFIED',
            'candidates_and_calculations': 'frozen diagnostic research rules, not official semantics',
        },
        'plan': plan_name,
        'plan_sha256': plan_sha,
        'generation': plan.get('generation', 'gen-1 retained snapshot'),
        'inputs': {
            'gbbq_member_sha256': sha(gbbq),
            'gbbq_member_bytes': len(gbbq),
            'gbbq_count_u32_le': count,
            'decoder_source_sha256': sha(decoder_source),
            'hsjday_zip_sha256': hsjday_sha,
            'day_members': day_member_meta,
            'selected_events_sha256': sha(selected_events_bytes),
        },
        'consistency': consistency,
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
    parser.add_argument('--gbbq', type=Path, help='pinned gbbq member for the selected plan')
    parser.add_argument('--decoder-source', type=Path, help='pinned pytdx gbbq_reader.py')
    parser.add_argument('--hsjday', type=Path, help='pinned hsjday.zip for the selected plan')
    parser.add_argument('--plan', default='plan-v2.json', choices=sorted(KNOWN_PLANS),
                        help='frozen plan file; default plan-v2.json (gen-2 snapshot)')
    parser.add_argument('--output', type=Path)
    parser.add_argument('--check', action='store_true', help='compare regenerated result with committed result.json')
    args = parser.parse_args()
    for flag in ('gbbq', 'decoder_source', 'hsjday'):
        if getattr(args, flag) is None:
            parser.error(f'--{flag.replace("_", "-")} is required')
    result = encode(run(args.gbbq, args.decoder_source, args.hsjday, plan_name=args.plan))
    if args.check:
        assert result == (HERE / 'result.json').read_bytes(), 'result differs'
        print('Offline full-domain replay PASS; official normalization NOT_VERIFIED; live source NOT_RUN')
    elif args.output:
        args.output.write_bytes(result)
    else:
        print(result.decode(), end='')


if __name__ == '__main__':
    main()
