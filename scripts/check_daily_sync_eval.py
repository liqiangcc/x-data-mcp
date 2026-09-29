#!/usr/bin/env python3
import hashlib
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PROMPT = ROOT / ".devin/evals/daily-ai-sync-v2.txt"
CONFIG = ROOT / ".devin/evals/daily-sync-config.json"
SKILL = ROOT / ".devin/skills/x-data-daily-sync/SKILL.md"
EXPECTED_PROMPT_SHA256 = (
    "fa9e2360780e5cd1274862da450da4067c267c8c0a3d9e8ae668cf4d23ec73c9"
)
EXPECTED_DENY = {"read", "edit", "grep", "glob", "exec"}


def skill_tools(text: str) -> list[str]:
    tools = []
    in_allowlist = False
    for line in text.splitlines():
        if line == "allowed-tools:":
            in_allowlist = True
            continue
        if in_allowlist and line == "---":
            break
        if in_allowlist and line.startswith("  - mcp__"):
            tools.append(line.removeprefix("  - "))
    return tools


def main() -> int:
    failures = []

    def check(label: str, ok: bool, detail: str = "") -> None:
        if not ok:
            failures.append(f"{label}: {detail}" if detail else label)

    prompt = PROMPT.read_bytes()
    prompt_text = prompt.decode("utf-8")
    prompt_sha256 = hashlib.sha256(prompt).hexdigest()
    check(
        "prompt digest",
        prompt_sha256 == EXPECTED_PROMPT_SHA256,
        f"expected {EXPECTED_PROMPT_SHA256}, found {prompt_sha256}",
    )
    check(
        "prompt invokes the Skill slash entrypoint",
        prompt_text.startswith("/x-data-daily-sync\n"),
    )
    for required in [
        "禁止使用 shell、exec、SSH、curl、SQLite",
        "不执行 fresh DB rebuild",
        "没有创建任何 generation/data task",
    ]:
        check(f"prompt contains {required!r}", required in prompt_text)

    config = json.loads(CONFIG.read_text(encoding="utf-8"))
    permissions = config.get("permissions", {})
    allowed = permissions.get("allow", [])
    denied = set(permissions.get("deny", []))
    expected_tools = skill_tools(SKILL.read_text(encoding="utf-8"))
    check("Skill MCP allowlist is non-empty", bool(expected_tools))
    check(
        "eval MCP allowlist equals Skill allowlist",
        set(allowed) == set(expected_tools) and len(allowed) == len(expected_tools),
        f"expected {expected_tools}, found {allowed}",
    )
    check(
        "eval MCP allowlist has no wildcard",
        all("*" not in tool for tool in allowed),
        f"found wildcard in {allowed}",
    )
    check(
        "eval denies non-MCP fallback tools",
        denied == EXPECTED_DENY,
        f"expected {sorted(EXPECTED_DENY)}, found {sorted(denied)}",
    )
    check(
        "eval config skips interactive setup",
        config.get("shell", {}).get("setup_complete") is True,
    )
    skill_text = SKILL.read_text(encoding="utf-8")
    for required in [
        "不得读取 overflow 文件",
        "也不要求枚举全部工具",
        "get_acquisition_capabilities({\"contract_version\":\"v1\"})",
        "get_data_capabilities({})",
        "catalog selection 只能用于\n`dataset:\"securities.catalog\"`",
        "按 ≤100 实体分批调用 `get_data_availability`",
        "selection:{\"kind\":\"batch\",\"entities\":[…]}",
    ]:
        check(f"Skill contains {required!r}", required in skill_text)
    for lineno, line in enumerate(skill_text.splitlines(), start=1):
        check(
            f"Skill line {lineno} does not apply catalog selection to market data",
            not (
                '"dataset":"market.' in line
                and '"kind":"catalog"' in line
            ),
            line.strip(),
        )

    if failures:
        for failure in failures:
            print(f"FAIL {failure}", file=sys.stderr)
        return 1
    print(
        f"daily-sync-eval OK: prompt v2 sha256={prompt_sha256}, "
        f"{len(expected_tools)} exact MCP tools, fallback tools denied"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
