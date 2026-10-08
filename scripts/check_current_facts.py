#!/usr/bin/env python3
import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

EXPECTED_PACKAGE_VERSION = "0.2.0-preview.30"
EXPECTED_LEGACY_SCHEMA = 18
EXPECTED_COMPACT_SCHEMA = 20
EXPECTED_DATA_TOOLS = 20
EXPECTED_ACQUIRE_TOOLS = 10
EXPECTED_DATA_CAPABILITY_REVISION = "generic-data-v3"

CURRENT_DOCS = {
    "README.md": "## 当前状态",
    "docs/daily-sync.md": "## 当前状态",
    "docs/mcp-tools.md": "## 版本与阅读方式",
    "docs/generic-data-tools.md": "## 决策与适用范围",
    "docs/source-closure.md": None,
}

REQUIRED_FACTS = [
    ("schema 20", re.compile(r"schema[\s:=v]*20(?!\d)", re.IGNORECASE)),
    ("20 data tools", re.compile(r"\b20\s+data\s+tools?\b", re.IGNORECASE)),
    ("10 acquisition tools", re.compile(r"\b10\s+acquisition\s+tools?\b", re.IGNORECASE)),
]

STALE_SCHEMA_AS_CURRENT = re.compile(r"SCHEMA_VERSION\s*=\s*18")
STALE_BATCH_LIMIT_BLOCK = re.compile(
    r"100[-\s]?item[^\n。；;]{0,25}(budget|预算|阻塞|上限|不可运行|不能运行|block)",
    re.IGNORECASE,
)
STALE_EIGHT_TOOLS = re.compile(r"当前八工具")
LEGACY_CONTEXT_MARKERS = ("legacy", "升级来源", "历史", "旧库")


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
        ("daily Skill capability revision", EXPECTED_DATA_CAPABILITY_REVISION),
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
        EXPECTED_DATA_CAPABILITY_REVISION in status_skill,
        f"missing {EXPECTED_DATA_CAPABILITY_REVISION!r}",
    )
    check(
        "data capability calls carry no contract_version",
        '`get_data_capabilities`（`contract_version' not in daily_skill
        and '`get_data_capabilities`（`contract_version' not in status_skill,
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
