#!/usr/bin/env python3
"""Offline Rust inspector cross-check against retained #53 four-member facts."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[2]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary', required=True, type=Path)
parser.add_argument('--members', required=True, type=Path)
parser.add_argument('--output', required=True, type=Path)
args = parser.parse_args()
selection_path = ROOT / 'evidence/issue-3-cross-stock-qfq/selection.json'
selection = json.loads(selection_path.read_bytes())
rows = []
for item in selection['selected']:
    symbol = item['market'] + '.' + item['code']
    path = args.members / (item['market'] + item['code'] + '.day')
    observation = json.loads(subprocess.check_output([str(args.binary.resolve()), str(path.resolve())]))
    expected = item['day']
    for field in ('sha256', 'bytes', 'record_count', 'first_date', 'last_date'):
        assert observation[field] == expected[field], (symbol, field, observation[field], expected[field])
    assert observation['business_semantics'] == 'NOT_VERIFIED'
    assert observation['production_ready'] is False
    rows.append({'symbol': symbol, 'member': item['member'], 'observation': observation})
result = {
    'schema': 'issue64.retained-day-check.v1',
    'status': 'STRUCTURAL_MATCH_ONLY',
    'selection_sha256': hashlib.sha256(selection_path.read_bytes()).hexdigest(),
    'parser_sha256': hashlib.sha256((ROOT / 'src/day_file.rs').read_bytes()).hexdigest(),
    'rows': rows,
    'live_source': 'NOT_RUN',
    'qfq_normalization': 'NOT_VERIFIED',
    'network_requests': 0,
}
args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + '\n')
print('4 retained members: structural/hash PASS; QFQ NOT_VERIFIED; live source NOT_RUN')
