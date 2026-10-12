#!/usr/bin/env python3
"""Public-repo hygiene gate: tracked text must not carry operator-private facts.

The repository is public. Once an email address, an internal host name or a
private network address is committed it cannot be recalled, so CI rejects them
in tracked files. Immutable historical evidence (`evidence/`, `fixtures/`,
`archive/`) and lock files are out of scope: they are captured artefacts, not
authored text.

Rules:
  1. e-mail addresses, except documented no-reply / transport addresses;
  2. internal host names (`ip-A-B-C-D`, `*.compute.internal`, `*.ts.net`);
  3. private / tailnet IPv4 addresses (10/8, 172.16/12, 192.168/16, 100.64/10).

Exit status is non-zero when any rule fires. Commit messages and PR bodies are
not scanned here; the same rules apply to them by convention.
"""
from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

SKIP_PREFIXES = ("evidence/", "fixtures/", "archive/")
SKIP_SUFFIXES = (".lock", "package-lock.json")

# Local part must not follow "/" (URL user-info such as `https://user@host`);
# the domain must end in an alphabetic TLD (so `tool@0.9.1` version pins pass).
EMAIL = re.compile(
    r"(?<![A-Za-z0-9._%+/-])[A-Za-z0-9._%+-]+@(?:[A-Za-z0-9-]+\.)+[A-Za-z]{2,}\b"
)
EMAIL_ALLOWED = (
    re.compile(r"^(?:noreply|no-reply)@(?:anthropic\.com|github\.com)$"),
    re.compile(r"^git@github\.com$"),
    re.compile(r"^[A-Za-z0-9._+-]+@users\.noreply\.github\.com$"),
    re.compile(r"@(?:example\.(?:com|org|net)|localhost)$"),
)
HOSTNAME = re.compile(
    r"\bip-\d{1,3}-\d{1,3}-\d{1,3}-\d{1,3}\b|\.compute\.internal\b|\.ts\.net\b"
)
PRIVATE_IP = re.compile(
    r"(?<![\d.])(?:10\.\d{1,3}\.\d{1,3}\.\d{1,3}"
    r"|172\.(?:1[6-9]|2\d|3[01])\.\d{1,3}\.\d{1,3}"
    r"|192\.168\.\d{1,3}\.\d{1,3}"
    r"|100\.(?:6[4-9]|[7-9]\d|1[01]\d|12[0-7])\.\d{1,3}\.\d{1,3})(?![\d.])"
)


def in_scope(path: str) -> bool:
    return not path.startswith(SKIP_PREFIXES) and not path.endswith(SKIP_SUFFIXES)


def scan_text(path: str, text: str) -> list[str]:
    errors: list[str] = []
    for number, line in enumerate(text.splitlines(), 1):
        for match in EMAIL.finditer(line):
            address = match.group(0)
            if not any(rule.search(address) for rule in EMAIL_ALLOWED):
                errors.append(f"{path}:{number}: e-mail address {address!r}")
        if HOSTNAME.search(line):
            errors.append(f"{path}:{number}: internal host name")
        if PRIVATE_IP.search(line):
            errors.append(f"{path}:{number}: private network address")
    return errors


def tracked_files(root: Path = ROOT) -> list[str]:
    out = subprocess.run(
        ["git", "ls-files", "-z"], cwd=root, check=True, capture_output=True
    ).stdout
    return [p for p in out.decode("utf-8").split("\0") if p]


def check(root: Path = ROOT) -> list[str]:
    errors: list[str] = []
    for rel in tracked_files(root):
        if not in_scope(rel):
            continue
        try:
            text = (root / rel).read_text(encoding="utf-8")
        except (UnicodeDecodeError, FileNotFoundError, IsADirectoryError):
            continue
        errors += scan_text(rel, text)
    return errors


def main() -> int:
    errors = check()
    if errors:
        print("public hygiene check failed:", file=sys.stderr)
        for error in errors:
            print(f"  {error}", file=sys.stderr)
        return 1
    print("public hygiene OK")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
