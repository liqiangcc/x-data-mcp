#!/usr/bin/env python3
"""Field-level inventory of pinned official base.zip members for issue #46.

Evidence-only, offline. Never a semantic contract. Reads exactly the six
members pinned in plan.json from the gen-2 official archive, verifies every
byte identity before analysis, and emits deterministic JSON.

Layers kept separate:
- official bytes: archive/member sizes, hashes, raw line/record hex;
- mechanical observations: field shapes, charset counts, token positions,
  code-set joins (byte facts only);
- semantic questions: always emitted under the fixed UNPROVEN label set.
"""
import argparse
import hashlib
import json
import re
import struct
import zipfile
from collections import Counter
from pathlib import Path

HERE = Path(__file__).resolve().parent
RETAINED_INDEX = HERE.parent / 'issue-6-tdx-dbf-source' / 'official' / 'archive-20260913.json'

PLAN_SHA256 = 'fc08cba8c78ecf1e618a521b783d998925f9bb5010f498dfb3d32ea64b7478ff'
ARCHIVE_BYTES = 16411232
ARCHIVE_SHA256 = 'dcdf6b49898926675698263535021aed7ad92b34be82a1a9e8a2a8982bd68bc6'
RETAINED_INDEX_SHA256 = '15210910cf8d648a85e58409558be2284b1aa2bdf8464ae2e0d2d0228048325c'
MEMBER_PINS = {
    'tdxhy.cfg': {'bytes': 150332, 'sha256': 'df7cb7051d0f73a0d1381e9b36eafa33c8f5081d28ddef87cad406dd28ed8ea6'},
    'tdxhy2.cfg': {'bytes': 249158, 'sha256': 'f496dcd9af9c6681682cc25d621b2ada195f555e9a9834ef75878dfeb0a0be4a'},
    'infoharbor_ex.code': {'bytes': 182145, 'sha256': '79eed0f6857ffde3d1d3cc8966ceff6e400f45e6e2387224446e453850fb5281'},
    'infoharbor_ex.name': {'bytes': 81609, 'sha256': 'f120745d760cea1f730639a8701f043ebffc31d7dccd70c8376a09ab9e7a3fb4'},
    'base.dbf': {'bytes': 4270189, 'sha256': '6e6480bbefcfa4145f1214e586457862f144dc5a563f14a285021e5e3ebcc770'},
    'base.map': {'bytes': 123740, 'sha256': '8441db92ec60046f5951e067eeaa50f5cf2e797a4551353250681bc4126f93cf'},
}
CODE_FIELD_INDEX = {
    'tdxhy.cfg': 1,
    'tdxhy2.cfg': 1,
    'infoharbor_ex.code': 0,
    'infoharbor_ex.name': 1,
}
SAMPLES = ('688318', '600000', '000001', '600004', '600006', '000002', '000004')
TOKEN_RE = re.compile(rb'(?<![0-9])([0-9]{6})(?![0-9])')
SIX_DIGIT_RE = re.compile(rb'[0-9]{6}')
PATTERNS = {
    'single_digit': re.compile(rb'[0-9]'),
    'six_digit': re.compile(rb'[0-9]{6}'),
    'T_code': re.compile(rb'T[0-9]{4,6}'),
    'X_code': re.compile(rb'X[0-9]{4,6}'),
    'ZX_code': re.compile(rb'ZX[0-9]{6}'),
    'CJ_code': re.compile(rb'CJ[0-9]{6}'),
    'ZZ_code': re.compile(rb'ZZ[0-9]{8}'),
}
UNPROVEN = [
    'MARKET_FIELD_SEMANTICS',
    'INDUSTRY_CODE_SEMANTICS',
    'INDUSTRY_HIERARCHY',
    'NAME_CORRECTNESS',
    'ENCODING_CONFIRMED',
    'CODE_TO_NAME_CONTRACT',
    'SECURITY_RANGE_COMPLETENESS',
    'MEMBER_RELATION_SEMANTICS',
]
GEN1_TDXHY2_RECORDS = 6753


def sha(data):
    return hashlib.sha256(data).hexdigest()


def encode(value):
    return (json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + '\n').encode()


def optional_ascii(value):
    return value.decode('ascii') if all(32 <= b < 127 for b in value) else None


def gb18030_ok(value):
    try:
        value.decode('gb18030')
        return True
    except UnicodeDecodeError:
        return False


def field_repr(value):
    item = {'hex': value.hex()}
    ascii_form = optional_ascii(value)
    if ascii_form is not None:
        item['ascii'] = ascii_form
    return item


def parse_lines(data):
    lines = []
    start = i = number = 0
    while i < len(data):
        byte = data[i]
        if byte == 13:
            if data[i:i + 3] == b'\r\r\n':
                eol, advance = 'CRCRLF', 3
            elif data[i:i + 2] == b'\r\n':
                eol, advance = 'CRLF', 2
            else:
                eol, advance = 'CR', 1
        elif byte == 10:
            eol, advance = 'LF', 1
        else:
            i += 1
            continue
        lines.append({'line_number': number, 'line_start_offset': start,
                      'content': data[start:i], 'eol': eol})
        number += 1
        i += advance
        start = i
    if start < len(data):
        lines.append({'line_number': number, 'line_start_offset': start,
                      'content': data[start:], 'eol': None})
    return lines


def field_inventory(lines):
    by_index = {}
    for line in lines:
        for index, value in enumerate(line['content'].split(b'|')):
            by_index.setdefault(index, []).append(value)
    stats = {}
    for index in sorted(by_index):
        values = by_index[index]
        distinct = sorted(set(values))
        counter = Counter(values)
        stats[str(index)] = {
            'record_count': len(values),
            'byte_width_histogram': {str(k): v for k, v in sorted(Counter(map(len, values)).items())},
            'empty_count': sum(1 for v in values if v == b''),
            'distinct_count': len(distinct),
            'ascii_only_count': sum(1 for v in values if optional_ascii(v) is not None),
            'high_byte_count': sum(1 for v in values if any(b >= 128 for b in v)),
            'gb18030_strict_decode_ok_count': sum(1 for v in values if gb18030_ok(v)),
            'gbk_name_trial_count': sum(1 for v in values if any(b >= 128 for b in v) and gb18030_ok(v)),
            'pattern_conformance': {name: sum(1 for v in values if p.fullmatch(v)) for name, p in PATTERNS.items()},
            'top_values': [[v.hex(), optional_ascii(v), c] for v, c in counter.most_common(50)],
            'distinct_values': [field_repr(v) for v in distinct] if len(distinct) <= 4096 else None,
        }
    return stats


def token_scan(lines):
    occurrences = {}
    for line in lines:
        content = line['content']
        for match in TOKEN_RE.finditer(content):
            field_index = content[:match.start(1)].count(b'|')
            occurrences.setdefault(match.group(1).decode(), []).append(
                {'line_number': line['line_number'], 'field_index': field_index})
    return occurrences


def text_member_report(name, data, code_field_index):
    lines = parse_lines(data)
    nonblank = [l for l in lines if l['content'] != b'']
    eol_counts = Counter(str(l['eol']) if l['eol'] is not None else 'NO_TERMINATOR' for l in lines)
    field_counts = Counter(len(l['content'].split(b'|')) for l in nonblank)
    tokens = token_scan(nonblank)
    profile = {}
    code_tokens = []
    other_tokens = []
    for token, hits in sorted(tokens.items()):
        fields = sorted({h['field_index'] for h in hits})
        line_numbers = sorted({h['line_number'] for h in hits})
        profile[token] = {
            'occurrence_count': len(hits),
            'line_count': len(line_numbers),
            'field_indices': fields,
        }
        if fields == [code_field_index]:
            code_tokens.append(token)
        else:
            other_tokens.append(token)
    multi_line = [
        {'token': t, 'line_count': p['line_count'], 'occurrence_count': p['occurrence_count'],
         'field_indices': p['field_indices']}
        for t, p in profile.items() if p['line_count'] > 1
    ]
    code_values = [l['content'].split(b'|')[code_field_index]
                   for l in nonblank if len(l['content'].split(b'|')) > code_field_index]
    code_six_digit = [v.decode() for v in code_values if SIX_DIGIT_RE.fullmatch(v)]
    return {
        'bytes': len(data),
        'sha256': sha(data),
        'line_count': len(lines),
        'record_count': len(nonblank),
        'blank_line_count': len(lines) - len(nonblank),
        'line_ending_counts': dict(sorted(eol_counts.items())),
        'field_count_distribution': {str(k): v for k, v in sorted(field_counts.items())},
        'field_inventory': field_inventory(nonblank),
        'code_field_index': code_field_index,
        'code_field_six_digit_count': len(code_six_digit),
        'code_field_six_digit_distinct': len(set(code_six_digit)),
        'code_field_set': sorted(set(code_six_digit)),
        'token_field_profile': profile,
        'code_position_tokens': code_tokens,
        'other_field_tokens': other_tokens,
        'multi_line_tokens': multi_line,
        'distinct_token_count': len(profile),
    }


def dbf_report(data):
    version = data[0]
    nrec = struct.unpack_from('<I', data, 4)[0]
    hlen = struct.unpack_from('<H', data, 8)[0]
    rlen = struct.unpack_from('<H', data, 10)[0]
    assert hlen + nrec * rlen <= len(data), 'dbf record area exceeds member bytes'
    fields = []
    offset = 32
    while data[offset] != 0x0D:
        descriptor = data[offset:offset + 32]
        fields.append({
            'name': descriptor[:11].split(b'\x00')[0].decode('ascii', 'replace'),
            'type': chr(descriptor[11]),
            'length': descriptor[16],
            'decimal_count': descriptor[17],
            'raw_hex': descriptor.hex(),
        })
        offset += 32
    position = 1
    for field in fields:
        field['record_offset'] = position
        position += field['length']
    assert position == rlen, 'dbf field lengths do not sum to record length'
    records = [data[hlen + i * rlen:hlen + (i + 1) * rlen] for i in range(nrec)]
    deleted_count = sum(1 for r in records if r[0] == 0x2A)
    field_stats = []
    for field in fields:
        start = field['record_offset']
        column = [r[start:start + field['length']] for r in records]
        field_stats.append({
            'name': field['name'],
            'type': field['type'],
            'length': field['length'],
            'non_blank_count': sum(1 for v in column if v.strip(b' \x00') != b''),
            'all_space_or_zero_count': sum(1 for v in column if v.strip(b' \x00') == b''),
            'ascii_only_count': sum(1 for v in column if optional_ascii(v) is not None),
            'high_byte_count': sum(1 for v in column if any(b >= 128 for b in v)),
            'distinct_count': len(set(column)),
        })
    gpdm_field = next((f for f in fields if f['name'] == 'GPDM'), None)
    gpdm_values = None
    gpdm_counter = None
    if gpdm_field is not None:
        start = gpdm_field['record_offset']
        gpdm_values = [r[start:start + gpdm_field['length']] for r in records]
        gpdm_counter = Counter(gpdm_values)
    return {
        'bytes': len(data),
        'sha256': sha(data),
        'version_byte': f'0x{version:02x}',
        'header_length': hlen,
        'record_length': rlen,
        'record_count': nrec,
        'trailing_bytes_after_records': len(data) - (hlen + nrec * rlen),
        'deleted_flag_count': deleted_count,
        'field_descriptors': fields,
        'field_stats': field_stats,
        'gpdm': None if gpdm_values is None else {
            'field_name': gpdm_field['name'],
            'six_digit_count': sum(1 for v in gpdm_values if SIX_DIGIT_RE.fullmatch(v)),
            'distinct_count': len(gpdm_counter),
            'duplicate_value_count': sum(1 for c in gpdm_counter.values() if c > 1),
            'code_set': sorted(v.decode() for v in gpdm_counter if SIX_DIGIT_RE.fullmatch(v)),
        },
        'records': records,
    }


def run(archive_path):
    plan_bytes = (HERE / 'plan.json').read_bytes()
    assert sha(plan_bytes) == PLAN_SHA256, 'plan.json hash drifted from frozen value'

    archive_data = archive_path.read_bytes()
    assert len(archive_data) == ARCHIVE_BYTES, 'archive byte count mismatch vs plan pin'
    assert sha(archive_data) == ARCHIVE_SHA256, 'archive sha256 mismatch vs plan pin'

    index_bytes = RETAINED_INDEX.read_bytes()
    assert sha(index_bytes) == RETAINED_INDEX_SHA256, 'retained index sha256 mismatch'
    retained = {m['name']: m for m in json.loads(index_bytes)['members']}

    findings = []
    with zipfile.ZipFile(archive_path) as archive:
        infos = {i.filename: i for i in archive.infolist()}
        assert len(infos) == len(retained) == 542, 'archive member table size differs from retained index'
        assert set(infos) == set(retained), 'archive member names differ from retained index'
        member_diffs = []
        for name in sorted(retained):
            data = archive.read(infos[name])
            actual = sha(data)
            if actual != retained[name]['sha256'] or len(data) != retained[name]['bytes']:
                member_diffs.append({'member': name, 'gen1_bytes': retained[name]['bytes'],
                                     'gen1_sha256': retained[name]['sha256'],
                                     'gen2_bytes': len(data), 'gen2_sha256': actual})
        if member_diffs:
            findings.append({'kind': 'MEMBER_CONTENT_DIFFERS_FROM_GEN1', 'count': len(member_diffs),
                             'members': member_diffs})
        for expected in ('infoharbor_ex.dat',):
            if expected not in infos:
                findings.append({'kind': 'EXPECTED_MEMBER_ABSENT', 'member': expected})
        member_data = {}
        for name, pin in MEMBER_PINS.items():
            data = archive.read(infos[name])
            assert len(data) == pin['bytes'], f'{name} byte count mismatch vs plan pin'
            assert sha(data) == pin['sha256'], f'{name} sha256 mismatch vs plan pin'
            member_data[name] = data

    reports = {}
    for name in CODE_FIELD_INDEX:
        reports[name] = text_member_report(name, member_data[name], CODE_FIELD_INDEX[name])
    reports['base.map'] = text_member_report('base.map', member_data['base.map'], 0)
    # base.map records are a single fixed-width field: first 6 bytes vs tail
    map_lines = [l for l in parse_lines(member_data['base.map']) if l['content'] != b'']
    map_head6 = [l['content'][:6] for l in map_lines]
    map_tail = [l['content'][6:] for l in map_lines]
    head6_six = [v for v in map_head6 if SIX_DIGIT_RE.fullmatch(v)]
    reports['base.map']['code_field_six_digit_count'] = len(head6_six)
    reports['base.map']['code_field_six_digit_distinct'] = len(set(head6_six))
    reports['base.map']['fixed_width_split'] = {
        'head6_six_digit_count': len(head6_six),
        'head6_distinct': len(set(map_head6)),
        'tail_numeric_count': sum(1 for v in map_tail if v.isdigit()),
        'tail_width_histogram': {str(k): v for k, v in sorted(Counter(map(len, map_tail)).items())},
        'tail_min': min(int(v) for v in map_tail if v.isdigit()),
        'tail_max': max(int(v) for v in map_tail if v.isdigit()),
    }
    reports['base.map']['code_field_set'] = sorted(v.decode() for v in set(head6_six))
    if reports['tdxhy2.cfg']['record_count'] != GEN1_TDXHY2_RECORDS:
        findings.append({'kind': 'RECORD_COUNT_DIFFERS_FROM_GEN1',
                         'member': 'tdxhy2.cfg',
                         'gen1_record_count': GEN1_TDXHY2_RECORDS,
                         'gen2_record_count': reports['tdxhy2.cfg']['record_count']})
    dbf = dbf_report(member_data['base.dbf'])
    dbf_records = dbf.pop('records')
    reports['base.dbf'] = dbf

    code_sets = {
        'tdxhy.cfg': set(reports['tdxhy.cfg']['code_field_set']),
        'tdxhy2.cfg': set(reports['tdxhy2.cfg']['code_field_set']),
        'infoharbor_ex.code': set(reports['infoharbor_ex.code']['code_field_set']),
        'infoharbor_ex.name': set(reports['infoharbor_ex.name']['code_field_set']),
        'base.map': set(reports['base.map']['code_field_set']),
        'base.dbf': set(reports['base.dbf']['gpdm']['code_set']) if reports['base.dbf']['gpdm'] else set(),
    }
    joins = {}
    names = sorted(code_sets)
    for left in names:
        for right in names:
            if left >= right:
                continue
            joins[f'{left} vs {right}'] = {
                'left_cardinality': len(code_sets[left]),
                'right_cardinality': len(code_sets[right]),
                'intersection': len(code_sets[left] & code_sets[right]),
                'only_left': len(code_sets[left] - code_sets[right]),
                'only_right': len(code_sets[right] - code_sets[left]),
            }

    # field-0 byte agreement on shared codes between the two tdxhy members
    def field0_by_code(name):
        out = {}
        for line in parse_lines(member_data[name]):
            if line['content'] == b'':
                continue
            parts = line['content'].split(b'|')
            if len(parts) > 1 and SIX_DIGIT_RE.fullmatch(parts[1]):
                out[parts[1].decode()] = parts[0].hex()
        return out
    hy_f0 = field0_by_code('tdxhy.cfg')
    hy2_f0 = field0_by_code('tdxhy2.cfg')
    shared_hy = sorted(set(hy_f0) & set(hy2_f0))
    field0_agree = sum(1 for c in shared_hy if hy_f0[c] == hy2_f0[c])
    field0_disagree = [{'code': c, 'tdxhy': hy_f0[c], 'tdxhy2': hy2_f0[c]}
                       for c in shared_hy if hy_f0[c] != hy2_f0[c]]

    # ordered pairing: base.map line i head6 vs base.dbf record i GPDM
    gpdm_field = next(f for f in reports['base.dbf']['field_descriptors'] if f['name'] == 'GPDM')
    gstart = gpdm_field['record_offset']
    ordered_match = sum(
        1 for i, line in enumerate(map_lines)
        if i < len(dbf_records) and line['content'][:6] == dbf_records[i][gstart:gstart + 6])
    ordered_pairing = {
        'map_line_count': len(map_lines),
        'dbf_record_count': len(dbf_records),
        'count_equal': len(map_lines) == len(dbf_records),
        'ordered_head6_equals_gpdm_count': ordered_match,
    }

    # ambiguity attribution between the two tdxhy members
    def restricted(name):
        profile = reports[name]['token_field_profile']
        code_field = CODE_FIELD_INDEX[name]
        code_pos = {t for t, p in profile.items() if p['field_indices'] == [code_field]}
        all_tokens = set(profile)
        multi = {t: p for t, p in profile.items() if p['line_count'] > 1}
        return code_pos, all_tokens, multi
    left_code, left_all, left_multi = restricted('tdxhy.cfg')
    right_code, right_all, right_multi = restricted('tdxhy2.cfg')
    shared_code_tokens = sorted(left_code & right_code)
    nonunivalent = [c for c in shared_code_tokens
                    if reports['tdxhy.cfg']['token_field_profile'][c]['line_count'] > 1
                    or reports['tdxhy2.cfg']['token_field_profile'][c]['line_count'] > 1]
    ambiguity_causes = []
    for token in sorted(left_multi.keys() | right_multi.keys()):
        ambiguity_causes.append({
            'token': token,
            'tdxhy_cfg': None if token not in left_multi else {
                'line_count': left_multi[token]['line_count'],
                'field_indices': left_multi[token]['field_indices']},
            'tdxhy2_cfg': None if token not in right_multi else {
                'line_count': right_multi[token]['line_count'],
                'field_indices': right_multi[token]['field_indices']},
        })
    ambiguity = {
        'unrestricted_overlap': {
            'left_cardinality': len(left_all),
            'right_cardinality': len(right_all),
            'intersection': len(left_all & right_all),
            'only_left': len(left_all - right_all),
            'only_right': len(right_all - left_all),
        },
        'code_position_overlap': {
            'left_cardinality': len(left_code),
            'right_cardinality': len(right_code),
            'intersection': len(shared_code_tokens),
            'only_left': len(left_code - right_code),
            'only_right': len(right_code - left_code),
            'nonunivalent_shared_tokens': nonunivalent,
        },
        'multi_line_token_attribution': ambiguity_causes,
        'verdict': 'CODE_POSITION_TOKENS_UNIVALENT' if not nonunivalent else 'CODE_POSITION_TOKENS_NONUNIVALENT',
        'boundary': 'field-1 byte-token correspondence is a byte fact only; it does not prove the two members denote the same security records semantically',
    }
    if not nonunivalent:
        findings.append({'kind': 'STRUCTURAL_AMBIGUITY_ATTRIBUTED',
                         'detail': 'all #135 ambiguity-causing tokens occur outside the code-position field; code-position tokens are univalent'})
    # token collisions: six-digit value used both as code-position token and inside other fields
    for name in ('tdxhy.cfg', 'tdxhy2.cfg'):
        profile = reports[name]['token_field_profile']
        collisions = sorted(t for t, p in profile.items() if len(p['field_indices']) > 1)
        if collisions:
            findings.append({'kind': 'TOKEN_FIELD_COLLISION', 'member': name,
                             'count': len(collisions), 'tokens': collisions[:200]})
    cross_collision = sorted((code_sets['tdxhy.cfg'] | code_sets['tdxhy2.cfg']) &
                             (set(reports['tdxhy.cfg']['other_field_tokens']) |
                              set(reports['tdxhy2.cfg']['other_field_tokens'])))
    if cross_collision:
        findings.append({'kind': 'CROSS_POSITION_TOKEN_COLLISION', 'count': len(cross_collision),
                         'detail': 'six-digit values that occur both at a code position and inside a non-code field',
                         'tokens': cross_collision[:200]})

    # fixed sample record comparison
    samples = {}
    for code in SAMPLES:
        per_member = {}
        for name in CODE_FIELD_INDEX:
            index = CODE_FIELD_INDEX[name]
            hits = []
            for line in parse_lines(member_data[name]):
                parts = line['content'].split(b'|')
                if len(parts) > index and parts[index].decode('ascii', 'replace') == code:
                    hits.append({'line_number': line['line_number'],
                                 'raw_hex': line['content'].hex(),
                                 'fields': [field_repr(p) for p in parts],
                                 'fields_gb18030_trial': [
                                     p.decode('gb18030') if gb18030_ok(p) else None for p in parts]})
            per_member[name] = {
                'code_position': f'field_{index}',
                'record_count': len(hits),
                'records': hits,
            }
        map_hits = [{'line_number': l['line_number'], 'raw_hex': l['content'].hex(),
                     'head6': l['content'][:6].decode('ascii', 'replace'),
                     'tail': l['content'][6:].decode('ascii', 'replace')}
                    for l in map_lines if l['content'][:6].decode('ascii', 'replace') == code]
        per_member['base.map'] = {'code_position': 'bytes_0_5', 'record_count': len(map_hits), 'records': map_hits}
        dbf_hits = [{'record_index': i, 'raw_hex': r.hex()} for i, r in enumerate(dbf_records)
                    if r[gstart:gstart + 6].decode('ascii', 'replace') == code]
        per_member['base.dbf'] = {'code_position': 'GPDM_field', 'record_count': len(dbf_hits), 'records': dbf_hits}
        checklist = {
            'CODE_TOKEN_PRESENT_TDXHY_CFG': 'PROVEN' if per_member['tdxhy.cfg']['record_count'] else 'ABSENT',
            'CODE_TOKEN_PRESENT_TDXHY2_CFG': 'PROVEN' if per_member['tdxhy2.cfg']['record_count'] else 'ABSENT',
            'CODE_TOKEN_PRESENT_EX_CODE': 'PROVEN' if per_member['infoharbor_ex.code']['record_count'] else 'ABSENT',
            'CODE_TOKEN_PRESENT_EX_NAME': 'PROVEN' if per_member['infoharbor_ex.name']['record_count'] else 'ABSENT',
            'CODE_TOKEN_PRESENT_BASE_MAP': 'PROVEN' if per_member['base.map']['record_count'] else 'ABSENT',
            'CODE_TOKEN_PRESENT_BASE_DBF': 'PROVEN' if per_member['base.dbf']['record_count'] else 'ABSENT',
            'CROSS_MEMBER_CODE_TOKEN_BYTE_EQUAL': 'PROVEN' if all(
                per_member[m]['record_count'] for m in per_member) else 'NOT_ALL_MEMBERS',
        }
        for label in UNPROVEN:
            checklist[label] = 'UNPROVEN'
        samples[code] = {'members': per_member, 'checklist': checklist}

    result = {
        'schema': 'issue46.tdxhy-inventory.v1',
        'status': 'FIELD_SEMANTICS_NOT_VERIFIED',
        'plan_sha256': PLAN_SHA256,
        'archive': {'bytes': len(archive_data), 'sha256': sha(archive_data),
                    'member_count': len(infos), 'member_names_match_retained_index': True},
        'findings': findings,
        'members': reports,
        'code_set_joins': joins,
        'tdxhy_pair': {
            'field0_agreement_on_shared_codes': {
                'shared_code_count': len(shared_hy),
                'agree_count': field0_agree,
                'disagree_count': len(field0_disagree),
                'disagreements': field0_disagree[:200],
            },
            'ambiguity_attribution': ambiguity,
        },
        'base_map_dbf_pairing': ordered_pairing,
        'samples': samples,
        'unproven_labels': UNPROVEN,
        'limits': [
            'Field shapes, charset counts, decode trials, token positions and set joins are mechanical byte facts.',
            'GB18030 decode success is a measurement, not a confirmed encoding.',
            'Code-position token byte equality across members is not proof the members denote the same security records.',
            'No industry level, hierarchy, name, market or range semantics are established.',
            'This slice does not close #46/#6 and does not change production readiness.',
        ],
    }
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('archive', type=Path, help='pinned gen-2 official base.zip')
    parser.add_argument('--output', type=Path)
    parser.add_argument('--check', action='store_true', help='compare regenerated result with committed result.json')
    args = parser.parse_args()
    result = encode(run(args.archive))
    if args.check:
        assert result == (HERE / 'result.json').read_bytes(), 'result differs'
        print('Offline inventory replay PASS; field semantics NOT_VERIFIED')
    elif args.output:
        args.output.write_bytes(result)
    else:
        print(result.decode(), end='')


if __name__ == '__main__':
    main()
