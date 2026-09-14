#!/usr/bin/env python3
"""Offline synthetic/tamper checks for the structure scanner identity gate."""
from __future__ import annotations

import tempfile
import json
from pathlib import Path

import scan_tdxhy_structure as scanner


def main() -> None:
    with tempfile.TemporaryDirectory() as directory:
        archive = Path(directory) / "tampered.zip"
        archive.write_bytes(b"synthetic-tamper")
        result = scanner.scan(
            archive,
            Path("evidence/issue-6-tdx-dbf-source/official/archive-20260913.json"),
        )
        assert result["classification"] == "INPUT_IDENTITY_NOT_VERIFIED"
        assert result["input_identity"]["reason"] == "ARCHIVE_IDENTITY_MISMATCH"

    with tempfile.TemporaryDirectory() as directory:
        retained = Path("evidence/issue-6-tdx-dbf-source/official/archive-20260913.json")
        tampered_index = json.loads(retained.read_text(encoding="utf-8"))
        next(item for item in tampered_index["members"] if item["name"] == "tdxhy.cfg")["sha256"] = "0" * 64
        index_path = Path(directory) / "tampered-index.json"
        index_path.write_text(json.dumps(tampered_index), encoding="utf-8")
        result = scanner.scan(Path("/tmp/official-base.zip"), index_path)
        assert result["classification"] == "INPUT_IDENTITY_NOT_VERIFIED"
        assert result["input_identity"]["reason"] == "TARGET_MEMBER_IDENTITY_MISMATCH"

    print("synthetic identity/tamper checks: PASS")


if __name__ == "__main__":
    main()
