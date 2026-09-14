#!/usr/bin/env python3
"""Offline synthetic/tamper checks for the structure scanner identity gate."""
from __future__ import annotations

import tempfile
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

    try:
        scanner.member_report(
            "tdxhy.cfg",
            b"1|000001|x|y|z|w\r\n",
            {"bytes": 20, "sha256": "0" * 64},
        )
    except scanner.InputIdentityError as error:
        assert error.reason == "TARGET_MEMBER_IDENTITY_MISMATCH"
    else:
        raise AssertionError("tampered target member must fail identity gate")

    print("synthetic identity/tamper checks: PASS")


if __name__ == "__main__":
    main()
