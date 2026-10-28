#!/usr/bin/env python3
"""Deployment-facts linter (TASK-XP-026 S5, extended by P1-2/#296).

Companion to check_current_facts.py, which owns the current-facts constants,
the "current region" rules and the STATUS self-consistency rules. This file
checks:

1. docs/STATUS.md carries a "last verified" (最后核实) date; warns when it
   is older than 14 days. A missing STATUS.md is a warning, not a failure:
   the doc may not exist yet on every branch — but a present file without
   a date, or with a malformed one, is an error.
2. Deployment version strings in STATUS.md must be legal
   (v?X.Y.Z-preview.N / preview.N forms).
3. Inside the CURRENT_DOCS "current" regions, ecs-node must not be written
   as the current deployment host. Mentions of ecs-node in other roles (e.g.
   the measurement environment of benchmark notes) are allowed. The region
   set is imported from check_current_facts.py so the two linters cannot
   drift apart.
4. The STATUS 服务 table must declare the deployed data entry set and each
   entry's tool count. The deployed set is the deployment fact from
   check_current_facts.DEPLOYED_DATA_ENTRIES (`ops` until the Ops-B deploy-day
   commit, v8 phase 2 §9.6); the per-entry counts come from the source
   allowlists, so one unit with several sockets/entries can be stated without
   duplicating numbers in this linter.

Exit 0 with only warnings; exit 1 on errors.
"""
import datetime
import re
import sys
from pathlib import Path

from check_current_facts import (
    CURRENT_DOCS,
    DEPLOYED_DATA_ENTRIES,
    SERVICE_ENTRY_SPEC,
    entry_tool_counts,
)

ROOT = Path(__file__).resolve().parent.parent

STATUS_MD = ROOT / "docs" / "STATUS.md"
STALE_DAYS = 14

VERIFIED_DATE = re.compile(
    r"(?:最后核实|last\s+verified|verified)[^\n]{0,30}?"
    r"\b(\d{4}-\d{2}-\d{2})\b",
    re.IGNORECASE,
)
# Any version-looking token after a deployment label is a candidate; each
# candidate must match VERSION_OK. Labels may chain ("deployment version:").
VERSION_FIELD = re.compile(
    r"(?:(?:version|版本|deployment|部署)\s*)+[:：=]?\s*`?(\S+)",
    re.IGNORECASE,
)
# Markdown-table form: the label sits in one cell, the value in the next
# ("| 部署版本 | v0.2.0-preview.30 @ `sha` |"). Capture the value cell's
# first token only.
VERSION_CELL = re.compile(
    r"\|\s*(?:(?:deployment|deployed|version|版本|部署版本|部署)\s*)+\|\s*`?([^\s|`]+)",
    re.IGNORECASE,
)
VERSION_OK = re.compile(r"^(v?\d+\.\d+\.\d+-preview\.\d+|preview\.\d+)$")
# A host assertion: ecs-node written as the value/side of a host label
# inside a "current" region.
HOST_CLAIM = re.compile(
    r"(?:当前主机|current\s+host|deployment\s+host|deploy(?:ed)?\s+(?:on|to)|主机)"
    r"\s*[:：=是为]?\s*`?ecs-node\b"
    r"|\becs-node\b[^\n]{0,20}?(?:当前主机|current\s+host|deployment\s+host)",
    re.IGNORECASE,
)


def top_current_region(text: str, start_heading: str | None) -> str:
    """Same region rule as check_current_facts.py: the 'current' section."""
    lines = text.splitlines(keepends=True)
    heads = [i for i, l in enumerate(lines) if l.startswith("## ")]
    if start_heading is None:
        end = heads[0] if heads else len(lines)
        return "".join(lines[:end])
    for pos, h in enumerate(heads):
        if lines[h].rstrip() == start_heading:
            end = heads[pos + 1] if pos + 1 < len(heads) else len(lines)
            return "".join(lines[h:end])
    return ""


def check_status_md(
    path: Path = STATUS_MD, today: datetime.date | None = None
) -> tuple[list[str], list[str]]:
    """Returns (errors, warnings) for the docs/STATUS.md freshness check."""
    errors: list[str] = []
    warnings: list[str] = []
    if not path.is_file():
        warnings.append("docs/STATUS.md absent: last-verified freshness unknown")
        return errors, warnings
    text = path.read_text(encoding="utf-8")
    match = VERIFIED_DATE.search(text)
    if not match:
        errors.append("docs/STATUS.md has no '最后核实'/'last verified' date")
    else:
        try:
            verified = datetime.date.fromisoformat(match.group(1).replace("/", "-"))
        except ValueError:
            errors.append(
                f"docs/STATUS.md last-verified date is malformed: {match.group(1)}"
            )
            verified = None
        if verified is not None:
            age = ((today or datetime.date.today()) - verified).days
            if age > STALE_DAYS:
                warnings.append(
                    f"docs/STATUS.md last-verified date is {age} days old (> {STALE_DAYS})"
                )
            if age < 0:
                errors.append("docs/STATUS.md last-verified date is in the future")
    candidates = [
        m.group(1).strip("` ")
        for pattern in (VERSION_FIELD, VERSION_CELL)
        for m in pattern.finditer(text)
    ]
    versions = [v for v in candidates if re.search(r"preview|\d+\.\d+", v)]
    if not versions:
        errors.append("docs/STATUS.md declares no deployment version")
    for version in versions:
        if not VERSION_OK.fullmatch(version):
            errors.append(f"docs/STATUS.md deployment version malformed: {version}")
    return errors, warnings


def check_current_host(root: Path = ROOT) -> list[str]:
    errors: list[str] = []
    for rel, heading in CURRENT_DOCS.items():
        path = root / rel
        if not path.is_file():
            continue
        region = top_current_region(path.read_text(encoding="utf-8"), heading)
        if HOST_CLAIM.search(region):
            errors.append(f"{rel}: current region writes ecs-node as the current host")
    return errors


DATA_UNIT = "x-data-mcp-data"


def check_deployed_entries(
    text: str, counts: dict[str, int] | None = None
) -> list[str]:
    """The STATUS 服务 table must state the deployed entries and their counts.

    One data unit may serve several `<socket> --entry <kind>` endpoints (v8
    phase 2), so a row can list several entries; a row without `--entry` means
    the pre-phase-2 single socket and therefore the implicit `ops` entry.
    """
    errors: list[str] = []
    counts = counts if counts is not None else entry_tool_counts()
    rows: list[tuple[int, list[str], list[int]]] = []
    for lineno, line in enumerate(text.splitlines(), start=1):
        stripped = line.strip()
        if not stripped.startswith("|"):
            continue
        cells = [cell.strip().strip("`") for cell in stripped.strip("|").split("|")]
        if not cells or cells[0] != DATA_UNIT:
            continue
        entries = (
            SERVICE_ENTRY_SPEC.findall(cells[2]) if len(cells) >= 4 else []
        ) or ["ops"]
        declared = [int(n) for n in re.findall(r"\d+", cells[3])] if len(cells) >= 4 else []
        rows.append((lineno, entries, declared))
    if not rows:
        errors.append(f"docs/STATUS.md has no {DATA_UNIT} row in the 服务 table")
        return errors
    seen: set[str] = set()
    for lineno, entries, declared in rows:
        seen.update(entries)
        if len(declared) != len(entries):
            errors.append(
                f"docs/STATUS.md:{lineno}: {DATA_UNIT} declares "
                f"{len(entries)} entr{'y' if len(entries) == 1 else 'ies'} "
                f"but {len(declared)} 工具数 value(s)"
            )
        for entry, count in zip(entries, declared):
            if entry not in counts:
                errors.append(
                    f"docs/STATUS.md:{lineno}: {DATA_UNIT} declares unknown "
                    f"--entry {entry}"
                )
            elif count != counts[entry]:
                errors.append(
                    f"docs/STATUS.md:{lineno}: {DATA_UNIT} --entry {entry} "
                    f"工具数 {count} (allowlist: {counts[entry]})"
                )
        for entry in entries:
            if entry not in DEPLOYED_DATA_ENTRIES and entry in counts:
                errors.append(
                    f"docs/STATUS.md:{lineno}: {DATA_UNIT} declares entry "
                    f"{entry}, not deployed (deployed: "
                    f"{', '.join(DEPLOYED_DATA_ENTRIES)})"
                )
    for entry in DEPLOYED_DATA_ENTRIES:
        if entry not in seen:
            errors.append(
                f"docs/STATUS.md: the 服务 table omits deployed data entry {entry}"
            )
    return errors


def main() -> int:
    errors: list[str] = []
    warnings: list[str] = []
    e, w = check_status_md()
    errors += e
    warnings += w
    errors += check_current_host()
    if STATUS_MD.is_file():
        errors += check_deployed_entries(STATUS_MD.read_text(encoding="utf-8"))
    for warning in warnings:
        print(f"WARN: {warning}")
    for error in errors:
        print(f"FAIL: {error}")
    if errors:
        return 1
    print("deploy facts checks passed" + (f" ({len(warnings)} warning(s))" if warnings else ""))
    return 0


if __name__ == "__main__":
    sys.exit(main())
