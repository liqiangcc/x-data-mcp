#!/usr/bin/env python3
import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

EXPECTED_PACKAGE_VERSION = "0.2.0-preview.31"
EXPECTED_LEGACY_SCHEMA = 18
# Fresh `init`/`init20` output schema (code constant). The deployed preview
# instance runs schema 21 (fresh rebuild at the v6 switch).
EXPECTED_COMPACT_SCHEMA = 21
EXPECTED_DATA_TOOLS = 21
EXPECTED_ACQUIRE_TOOLS = 10
EXPECTED_DATA_CAPABILITY_REVISION = "generic-data-v6"
# The strict Skills describe the DEPLOYED contract (v6 after the switch).
# The same deployed revision keys the strict-Skill checks and the
# stale-deployment-claim check below: the "current" regions describe the
# deployed instance, not the code on main. Flip this constant together with
# the Skills in the same commit at the next contract switch.
EXPECTED_SKILL_CAPABILITY_REVISION = "generic-data-v6"

CURRENT_DOCS = {
    "README.md": "## 当前状态",
    "docs/daily-sync.md": "## 当前状态",
    "docs/mcp-tools.md": "## 版本与阅读方式",
    "docs/generic-data-tools.md": "## 决策与适用范围",
    "docs/source-closure.md": None,
}

REQUIRED_FACTS = [
    ("schema 21", re.compile(r"schema[\s:=v]*21(?!\d)", re.IGNORECASE)),
    ("21 data tools", re.compile(r"\b21\s+data\s+tools?\b", re.IGNORECASE)),
    ("10 acquisition tools", re.compile(r"\b10\s+acquisition\s+tools?\b", re.IGNORECASE)),
]

STALE_SCHEMA_AS_CURRENT = re.compile(r"SCHEMA_VERSION\s*=\s*18")
STALE_BATCH_LIMIT_BLOCK = re.compile(
    r"100[-\s]?item[^\n。；;]{0,25}(budget|预算|阻塞|上限|不可运行|不能运行|block)",
    re.IGNORECASE,
)
STALE_EIGHT_TOOLS = re.compile(r"当前八工具")
LEGACY_CONTEXT_MARKERS = ("legacy", "升级来源", "历史", "旧库")

# Stale deployment claims inside the "current" regions (TASK-GOV-DOCS-FIX-1).
# A current region may only state the deployed machine facts that match the
# code constants / STATUS.md: `N data tools` with N == EXPECTED_DATA_TOOLS,
# `generic-data-vN` with the expected revision, and `preview.N` that is the
# deployed version (docs/STATUS.md "部署版本" row) or the package version.
#
# Historical exemption (so real history can stay): a *paragraph* (blank-line
# separated block, list items included) is exempt when any of its lines contains
# a HISTORICAL_MARKERS word, and everything under a heading whose text contains
# one is exempt until the next heading of the same or a higher level. Mark old
# measurements with "历史" / "historical" instead of deleting them.
HISTORICAL_MARKERS = ("历史", "historical", "Historical")
DATA_TOOLS_CLAIM = re.compile(
    r"\b(\d+)\s*(?:个\s*)?data\s*(?:tools?|工具)", re.IGNORECASE
)
REVISION_CLAIM = re.compile(r"generic-data-v(\d+)")
PREVIEW_CLAIM = re.compile(r"preview\.(\d+)")
STATUS_DEPLOYED_VERSION = re.compile(
    r"\|\s*部署版本\s*\|\s*`?v?\d+\.\d+\.\d+-preview\.(\d+)"
)


def deployed_preview_numbers(status_text: str | None, package_version: str) -> set[str]:
    """Preview numbers a current region may mention: STATUS.md deployed + package."""
    allowed = set()
    m = re.search(r"preview\.(\d+)", package_version)
    if m:
        allowed.add(m.group(1))
    if status_text:
        m = STATUS_DEPLOYED_VERSION.search(status_text)
        if m:
            allowed.add(m.group(1))
    return allowed


def stale_deployment_claims(
    region: str, allowed_previews: set[str], data_tools: int, revision: str
) -> list[tuple[int, str, str]]:
    """Returns (line number in region, kind, line) for each stale claim."""
    lines = region.splitlines()
    wanted_revision = revision.rsplit("-v", 1)[-1]
    found: list[tuple[int, str, str]] = []
    paragraph: list[int] = []
    hist_level: int | None = None  # heading level of the open historical heading

    def flush() -> None:
        if not paragraph:
            return
        if not any(
            marker in lines[i] for i in paragraph for marker in HISTORICAL_MARKERS
        ):
            for i in paragraph:
                line = lines[i]
                for m in DATA_TOOLS_CLAIM.finditer(line):
                    if int(m.group(1)) != data_tools:
                        found.append((i + 1, f"{m.group(0)} (expected {data_tools})", line))
                for m in REVISION_CLAIM.finditer(line):
                    if m.group(1) != wanted_revision:
                        found.append((i + 1, f"{m.group(0)} (expected {revision})", line))
                for m in PREVIEW_CLAIM.finditer(line):
                    if m.group(1) not in allowed_previews:
                        found.append(
                            (i + 1, f"{m.group(0)} (deployed: {sorted(allowed_previews)})", line)
                        )
        paragraph.clear()

    for i, line in enumerate(lines):
        stripped = line.lstrip()
        if stripped.startswith("#"):
            flush()
            level = len(stripped) - len(stripped.lstrip("#"))
            if hist_level is not None and level <= hist_level:
                hist_level = None
            if hist_level is None and any(mk in line for mk in HISTORICAL_MARKERS):
                hist_level = level
            continue
        if not line.strip():
            flush()
            continue
        if hist_level is None:
            paragraph.append(i)
    flush()
    return found


def top_current_region(text: str, start_heading: str | None) -> str:
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


def rust_u32_const(text: str, name: str) -> int | None:
    m = re.search(rf"pub\s+const\s+{name}\s*:\s*\w+\s*=\s*(\d+)\s*;", text)
    return int(m.group(1)) if m else None


def data_tool_names(text: str) -> list[str]:
    start = text.index("pub fn entries()")
    end = text.index("\n}\n", start)
    body = text[start:end]
    return re.findall(r'entry::<[^;]*?>\s*\(\s*"([^"]+)"', body)


def acquire_tool_names(text: str) -> list[str]:
    start = text.index("const ENTRIES")
    end = text.index("\n];", start)
    return re.findall(r'name:\s*"([^"]+)"', text[start:end])


def declared_scope_value(text: str) -> str:
    start = text.index("pub const DECLARED_SCOPE")
    end = text.index('";', start) + 1
    raw = "".join(re.findall(r'"([^"]*)"', text[start:end]))
    return re.sub(r"\\\n\s*", "", raw)


def main() -> int:
    failures: list[str] = []

    def check(label: str, ok: bool, detail: str = "") -> None:
        if not ok:
            failures.append(f"{label}: {detail}" if detail else label)

    cargo = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    version = cargo["package"]["version"]
    check(
        "Cargo.toml package version",
        version == EXPECTED_PACKAGE_VERSION,
        f"expected {EXPECTED_PACKAGE_VERSION}, found {version}",
    )

    sqlite_tasks = (ROOT / "src/sqlite_tasks.rs").read_text(encoding="utf-8")
    legacy = rust_u32_const(sqlite_tasks, "SCHEMA_VERSION")
    check(
        "SCHEMA_VERSION (legacy upgrade source schema)",
        legacy == EXPECTED_LEGACY_SCHEMA,
        f"expected {EXPECTED_LEGACY_SCHEMA}, found {legacy}",
    )
    compact = rust_u32_const(sqlite_tasks, "COMPACT_SCHEMA_VERSION")
    check(
        "COMPACT_SCHEMA_VERSION (fresh compact schema)",
        compact == EXPECTED_COMPACT_SCHEMA,
        f"expected {EXPECTED_COMPACT_SCHEMA}, found {compact}",
    )

    registry = (ROOT / "src/data_supply/registry.rs").read_text(encoding="utf-8")
    data_names = data_tool_names(registry)
    check(
        "data_supply::registry::entries() tool count",
        len(data_names) == EXPECTED_DATA_TOOLS,
        f"expected {EXPECTED_DATA_TOOLS}, found {len(data_names)}: {data_names}",
    )
    check(
        "data_supply::registry::entries() duplicate names",
        len(set(data_names)) == len(data_names),
        f"duplicates: {sorted({n for n in data_names if data_names.count(n) > 1})}",
    )

    acquire = (ROOT / "src/acquire/mcp.rs").read_text(encoding="utf-8")
    acquire_names = acquire_tool_names(acquire)
    check(
        "acquire::mcp::ENTRIES tool count",
        len(acquire_names) == EXPECTED_ACQUIRE_TOOLS,
        f"expected {EXPECTED_ACQUIRE_TOOLS}, found {len(acquire_names)}: {acquire_names}",
    )
    check(
        "acquire::mcp::ENTRIES duplicate names",
        len(set(acquire_names)) == len(acquire_names),
        f"duplicates: {sorted({n for n in acquire_names if acquire_names.count(n) > 1})}",
    )

    official = (ROOT / "src/official_market.rs").read_text(encoding="utf-8")
    scope = declared_scope_value(official)
    check(
        "DECLARED_SCOPE names the authorization boundary",
        "external distribution is not authorized" in scope,
        f"scope is {scope!r}",
    )
    check(
        "DECLARED_SCOPE cites no retired #225 gate",
        "#225" not in scope,
        f"scope is {scope!r}",
    )

    data_mcp = (ROOT / "src/data_supply/mcp.rs").read_text(encoding="utf-8")
    check(
        "data capability revision",
        f'"{EXPECTED_DATA_CAPABILITY_REVISION}"' in data_mcp,
        f"expected {EXPECTED_DATA_CAPABILITY_REVISION}",
    )

    daily_skill = (
        ROOT / ".devin/skills/x-data-daily-sync/SKILL.md"
    ).read_text(encoding="utf-8")
    status_skill = (
        ROOT / ".devin/skills/x-data-status/SKILL.md"
    ).read_text(encoding="utf-8")
    for label, fact in [
        ("daily Skill capability revision", EXPECTED_SKILL_CAPABILITY_REVISION),
        ("daily Skill day pending gate", "`pending_roles` 包含 `day`"),
        ("daily Skill day pending stop", "`day` 不在 `pending_roles`"),
        (
            "daily Skill explicit fresh rebuild gate",
            "FRESH_DB_REBUILD_REQUIRES_EXPLICIT_REQUEST",
        ),
        ("daily Skill inflight gate", "`sync_status.inflight_tasks`"),
    ]:
        check(label, fact in daily_skill, f"missing {fact!r}")
    check(
        "status Skill capability revision",
        EXPECTED_SKILL_CAPABILITY_REVISION in status_skill,
        f"missing {EXPECTED_SKILL_CAPABILITY_REVISION!r}",
    )
    check(
        "data capability calls carry no contract_version",
        '`get_data_capabilities`（`contract_version' not in daily_skill
        and '`get_data_capabilities`（`contract_version' not in status_skill,
    )

    status_path = ROOT / "docs/STATUS.md"
    allowed_previews = deployed_preview_numbers(
        status_path.read_text(encoding="utf-8") if status_path.exists() else None,
        EXPECTED_PACKAGE_VERSION,
    )

    for rel, heading in CURRENT_DOCS.items():
        region = top_current_region(
            (ROOT / rel).read_text(encoding="utf-8"), heading
        )
        check(f"{rel} current region located", bool(region), f"missing {heading}")
        for label, pattern in REQUIRED_FACTS:
            check(
                f"{rel} current region states {label}",
                bool(pattern.search(region)),
            )
        for lineno, line in enumerate(region.splitlines(), start=1):
            if STALE_SCHEMA_AS_CURRENT.search(line) and not any(
                marker in line for marker in LEGACY_CONTEXT_MARKERS
            ):
                check(
                    f"{rel} line {lineno} states SCHEMA_VERSION=18 as current fact",
                    False,
                    line.strip(),
                )
        check(
            f"{rel} current region keeps no 100-item budget block",
            not STALE_BATCH_LIMIT_BLOCK.search(region),
        )
        check(
            f"{rel} current region keeps no 当前八工具 claim",
            not STALE_EIGHT_TOOLS.search(region),
        )
        for lineno, kind, line in stale_deployment_claims(
            region,
            allowed_previews,
            EXPECTED_DATA_TOOLS,
            EXPECTED_SKILL_CAPABILITY_REVISION,
        ):
            check(
                f"{rel} current region line {lineno} states a stale deployment fact",
                False,
                f"{kind}: {line.strip()}",
            )

    if failures:
        for failure in failures:
            print(f"FAIL {failure}", file=sys.stderr)
        return 1
    print(
        f"current-facts OK: {EXPECTED_PACKAGE_VERSION}, fresh compact schema "
        f"{EXPECTED_COMPACT_SCHEMA} (legacy upgrade source schema "
        f"{EXPECTED_LEGACY_SCHEMA}), {EXPECTED_DATA_TOOLS} data tools, "
        f"{EXPECTED_ACQUIRE_TOOLS} acquisition tools, "
        f"{len(CURRENT_DOCS)} current docs consistent"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
