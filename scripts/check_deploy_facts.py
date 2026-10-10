#!/usr/bin/env python3
"""Deployment-facts linter (TASK-XP-026 S5).

Companion to check_current_facts.py (which is owned by the mainline and
must not be modified). Checks:

1. docs/STATUS.md carries a "last verified" (最后核实) date; warns when it
   is older than 14 days. A missing STATUS.md is a warning, not a failure:
   the doc may not exist yet on every branch — but a present file without
   a date, or with a malformed one, is an error.
2. Deployment version strings in STATUS.md must be legal
   (v?X.Y.Z-preview.N / preview.N forms).
3. Inside the CURRENT_DOCS "current" regions (same doc set as
   check_current_facts.py), ecs-node must not be written as the current
   deployment host. Mentions of ecs-node in other roles (e.g. the
   measurement environment of benchmark notes) are allowed.

Exit 0 with only warnings; exit 1 on errors.
"""
import datetime
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

CURRENT_DOCS = {
    "README.md": "## 当前状态",
    "docs/daily-sync.md": "## 当前状态",
    "docs/mcp-tools.md": "## 版本与阅读方式",
    "docs/generic-data-tools.md": "## 决策与适用范围",
    "docs/source-closure.md": None,
}

STATUS_MD = ROOT / "docs" / "STATUS.md"
STALE_DAYS = 14

VERIFIED_DATE = re.compile(
    r"(?:最后核实|last\s+verified|verified)[^\n]{0,30}?"
    r"(20\d\d[-/.](?:0?[1-9]|1[0-2])[-/.](?:0?[1-9]|[12]\d|3[01]))",
    re.IGNORECASE,
)
# Any version-looking token after a deployment label is a candidate; each
# candidate must match VERSION_OK. Labels may chain ("deployment version:").
VERSION_FIELD = re.compile(
    r"(?:(?:version|版本|deployment|部署)\s*)+[:：=]?\s*`?(\S+)",
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
    candidates = [m.group(1).strip("` ") for m in VERSION_FIELD.finditer(text)]
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


def main() -> int:
    errors: list[str] = []
    warnings: list[str] = []
    e, w = check_status_md()
    errors += e
    warnings += w
    errors += check_current_host()
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
