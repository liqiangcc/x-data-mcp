#!/usr/bin/env python3
"""Offline integrity/text-presence audit, never a legal/production approval."""
import argparse
import hashlib
import json
from pathlib import Path


def replay(root):
    manifest = json.loads(Path(__file__).with_name('manifest.json').read_text())
    rows = []
    for item in manifest['documents']:
        data = (root / item['retained_name']).read_bytes()
        digest = hashlib.sha256(data).hexdigest()
        if digest != item['sha256'] or len(data) != item['bytes']:
            raise ValueError('retained artifact mismatch: ' + item['retained_name'])
        text = data.decode('utf-8')
        markers = {marker: marker in text for marker in item['markers']}
        if not all(markers.values()):
            raise ValueError('document marker missing')
        rows.append({'url': item['url'], 'sha256': digest, 'markers_present': markers})
    return {'schema': 'x-data-mcp.file-use-evidence.v1', 'documents': rows,
            'evidence_level': 'OFFICIAL_PUBLIC_DOCUMENTS_SCOPE_UNRESOLVED',
            'file_access_use': 'NOT_VERIFIED', 'production_ready': False,
            'normalization_contract': 'NOT_VERIFIED',
            'live_market_source': 'NOT_RUN', 'official_document_get': 'RUN'}


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('--retained-dir', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    args.output.write_text(json.dumps(replay(args.retained_dir), ensure_ascii=False, indent=2) + '\n')
