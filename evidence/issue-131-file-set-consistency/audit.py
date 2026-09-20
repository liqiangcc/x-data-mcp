#!/usr/bin/env python3
"""Deterministic audit of the pinned official retained input set for
issue #131 file-set consistency.

Reads the pinned snapshot manifest, re-verifies each retained file's
sha256 from its bytes, observes the immutable publication bindings in the
preview database, and emits:

  result.json                  audit outcome and observed facts
  compatibility-evidence.json  the CompatibilityEvidence artifact
  input.json                   consistency_check input (FileFacts + evidence)

No network access. No wall-clock output: two runs over unchanged inputs
produce byte-identical artifacts.
"""
import hashlib
import json
import re
import sqlite3
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
PLAN = json.loads((HERE / 'plan.json').read_text())
MANIFEST_PATH = Path(PLAN['inputs']['snapshot_manifest']['path'])
MANIFEST_SHA = PLAN['inputs']['snapshot_manifest']['sha256']
DB_PATH = PLAN['inputs']['preview_database']['path']

REQUIRED_ROLES = ('base_industry', 'day', 'gbbq')
EVENT_RE = re.compile(r'\((\d+) events, (\d+) non-category-1')


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for chunk in iter(lambda: f.read(1 << 20), b''):
            h.update(chunk)
    return h.hexdigest()


def encode(obj):
    return json.dumps(obj, indent=2, sort_keys=True, ensure_ascii=False) + '\n'


def check(name, ok, detail=None):
    entry = {'check': name, 'ok': bool(ok)}
    if detail is not None:
        entry['detail'] = detail
    return entry


def main():
    checks = []

    manifest_bytes = MANIFEST_PATH.read_bytes()
    actual_manifest_sha = hashlib.sha256(manifest_bytes).hexdigest()
    checks.append(check('manifest_sha256_matches_pin', actual_manifest_sha == MANIFEST_SHA,
                        actual_manifest_sha))
    manifest = json.loads(manifest_bytes)

    by_role = {f['role']: f for f in manifest['files']}
    checks.append(check('manifest_covers_required_roles',
                        sorted(by_role) == sorted(REQUIRED_ROLES),
                        sorted(by_role)))

    # Re-verify each retained file's bytes against the manifest (never trust).
    file_facts = []
    for role in REQUIRED_ROLES:
        entry = by_role[role]
        path = MANIFEST_PATH.parent / entry['relative_path']
        actual = sha256_file(path)
        checks.append(check(f'file_bytes_rehashed:{role}', actual == entry['sha256'],
                            {'manifest': entry['sha256'], 'actual': actual,
                             'bytes': path.stat().st_size}))
        file_facts.append({
            'role': role,
            'bytes': entry['bytes'],
            'sha256': entry['sha256'],
            'source_url': entry['source_url'],
            'acquisition_started_at': entry['acquisition_started_at'],
            'acquisition_completed_at': entry['acquisition_completed_at'],
            'last_modified': entry.get('last_modified'),
            'etag': entry.get('etag'),
            'byte_integrity': 'verified',
            'source_revision': None,
            'source_identity': None,
        })

    day_sha = by_role['day']['sha256']
    gbbq_sha = by_role['gbbq']['sha256']

    con = sqlite3.connect(f"file:{DB_PATH}?mode=ro", uri=True)

    # compat_json embeds each version's full record list; stream rows so peak
    # memory stays at one parsed row instead of the whole 10k-row result set.
    day_versions = 0
    applied = excluded = parsed = 0
    binding_violations = []
    for vid, compat in con.execute(
            "select version_id, compat_json from data_versions where period='day'"):
        day_versions += 1
        j = json.loads(compat)
        inputs = (j.get('batch', {}).get('provenance', {}) or {}) \
            .get('snapshot_inputs') or []
        bound = {i['role']: i['archive_sha256'] for i in inputs}
        if bound.get('day') != day_sha or bound.get('gbbq') != gbbq_sha:
            binding_violations.append(vid)
        reason = (j.get('adjustment_evidence') or {}).get('reason') or ''
        m = EVENT_RE.search(reason)
        if m:
            parsed += 1
            applied += int(m.group(1))
            excluded += int(m.group(2))
    checks.append(check('day_versions_sole_input_binding',
                        not binding_violations,
                        {'day_versions': day_versions,
                         'violations': len(binding_violations)}))

    heads = con.execute(
        "select count(*) from effective_heads where period='day'").fetchone()[0]
    head_versions = con.execute(
        "select count(*) from effective_heads e join data_versions v"
        " on e.version_id = v.version_id"
        " where e.period='day' and v.version_id like 'range-version-%'").fetchone()[0]
    checks.append(check('all_day_heads_published_from_set',
                        heads > 0 and head_versions == heads,
                        {'heads': heads, 'range_version_heads': head_versions}))

    # Base exercised via the catalog publication: source instance id is the
    # manifest sha256 prefix, acquired_at equals base acquisition_completed_at.
    base_completed = by_role['base_industry']['acquisition_completed_at']
    expected_source = f"official-file:{MANIFEST_SHA[:16]}"
    catalog_match = []
    for vid, compat in con.execute(
            "select version_id, compat_json from catalog_versions"):
        j = json.loads(compat)
        if j.get('source') == expected_source and j.get('acquired_at') == base_completed:
            catalog_match.append(vid)
    checks.append(check('base_exercised_via_catalog_publication',
                        len(catalog_match) == 1,
                        {'expected_source': expected_source,
                         'matched_versions': catalog_match}))

    checks.append(check('adjustment_evidence_statistics', parsed > 0,
                        {'versions_with_stats': parsed,
                         'applied_category1_events': applied,
                         'excluded_non_category1_records': excluded}))
    con.close()

    all_ok = all(c['ok'] for c in checks)

    valid_from = min(f['acquisition_started_at'] for f in file_facts)
    valid_until = max(f['acquisition_completed_at'] for f in file_facts)

    core = {
        'manifest_sha256': MANIFEST_SHA,
        'member_sha256': {r: by_role[r]['sha256'] for r in REQUIRED_ROLES},
        'valid_window': {'valid_from': valid_from, 'valid_until': valid_until},
        'day_versions': day_versions,
        'day_heads': heads,
        'applied_category1_events': applied,
        'excluded_non_category1_records': excluded,
        'checks_ok': all_ok,
    }
    audit_digest = hashlib.sha256(encode(core).encode()).hexdigest()

    evidence = {
        'evidence_ref': f'evidence/issue-131-file-set-consistency/result.json@{audit_digest}',
        'rule_version': 'official-file-consistency-v1',
        'compatibility_set_revision': f'snapshot-manifest@{MANIFEST_SHA}',
        'compatibility_set_identity': 'tdx-official-retained-snapshot',
        'member_bindings': {
            role: {'sha256': by_role[role]['sha256'],
                   'source_revision': None,
                   'source_identity': None}
            for role in REQUIRED_ROLES
        },
        'valid_window': {'valid_from': valid_from, 'valid_until': valid_until},
    }

    result = {
        'schema': 'issue-131-file-set-consistency-result/v1',
        'audit_digest': audit_digest,
        'all_checks_ok': all_ok,
        'checks': checks,
        'exercised_domain': {
            'day_versions': day_versions,
            'day_effective_heads': heads,
            'range_version_heads': head_versions,
            'catalog_versions_matched': catalog_match,
            'applied_category1_events': applied,
            'excluded_non_category1_records': excluded,
        },
        'compatibility_evidence': evidence,
    }

    (HERE / 'result.json').write_text(encode(result))
    (HERE / 'compatibility-evidence.json').write_text(encode(evidence))
    (HERE / 'input.json').write_text(encode({
        'files': file_facts,
        'compatibility': evidence,
    }))
    print(encode({'all_checks_ok': all_ok, 'audit_digest': audit_digest}))
    return 0 if all_ok else 1


if __name__ == '__main__':
    sys.exit(main())
