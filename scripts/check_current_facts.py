#!/usr/bin/env python3
import re
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

EXPECTED_PACKAGE_VERSION = "0.2.0-preview.37"
EXPECTED_LEGACY_SCHEMA = 18
# Fresh `init` output schema (code constant). The DEPLOYED schema is a
# deployment fact and is read from docs/STATUS.md (see DEPLOYED_SCHEMA_ROW):
# the code constant may be ahead of it, e.g. schema 24 built by M2 PR-2 while
# the deployed instance still runs schema 21.
EXPECTED_COMPACT_SCHEMA = 24
# Registry/code fact (v8 PR-2 adds list_catalog_facets: 23 -> 24).
EXPECTED_DATA_TOOLS = 24
# The deployed instance reports v7 / 23 tools after the v7 switch:
# documentation "current" regions describe deployment facts, so stale-claim
# checks compare against the deployed constants, not the code constants. A
# sentence that explicitly says it is stating the code fact (代码/未部署) is
# checked against the registry instead.
EXPECTED_DEPLOYED_DATA_TOOLS = 15
EXPECTED_ACQUIRE_TOOLS = 10
EXPECTED_DATA_CAPABILITY_REVISION = "generic-data-v8"
# The strict Skills describe the DEPLOYED contract (v8 after the minimal switch).
# The same deployed revision keys the strict-Skill checks and the
# stale-deployment-claim check below: the "current" regions describe the
# deployed instance, not the code on main. Flip this constant together with
# the Skills in the same commit at the next contract switch.
EXPECTED_SKILL_CAPABILITY_REVISION = "generic-data-v8"

CURRENT_DOCS = {
    "README.md": "## 当前状态",
    "docs/daily-sync.md": "## 当前状态",
    "docs/mcp-tools.md": "## 版本与阅读方式",
    "docs/generic-data-tools.md": "## 决策与适用范围",
    "docs/source-closure.md": None,
}

# Current-state regions added by P1-2 (#296): the regions P1-1 corrected. They
# do not restate every REQUIRED_FACT, so they get the stale-claim rules only
# (schema / capability revision written as deployed, tool counts). `None` is
# the same "block above the first '## ' heading" rule as CURRENT_DOCS; a tuple
# lists the '## ' headings that are current-state regions in that file.
CURRENT_REGIONS: dict[str, tuple[str | None, ...]] = {
    "docs/design.md": (
        "## 存储模型状态（版本链 vs 单版本）",
        "## 当前能力与目标差距",
    ),
    "docs/latest-only-storage.md": (
        None,
        # §10 states the deployed model directly ("已部署事实不因本文改变"),
        # §11 is the schema-20 comparison table; both were wrong before P1-1.
        "## 10. schema 21、部署与回滚",
        "## 11. 与 schema 20（历史）模型对照",
    ),
    "docs/sqlite-storage-design.md": (None,),
    "docs/verified-reads.md": (None,),
    "docs/yearly.md": (None,),
    "docs/publish-storage.md": (None,),
    # The data catalog states current dataset semantics and read limits;
    # deployment numbers stay in STATUS.md (linked only).
    "docs/data-catalog.md": (
        "## 1. 说明与权威入口",
        "## 2. 已提供的数据",
        "## 3. 缺口清单",
        "## 4. 按用途看现在能做什么",
        "## 5. 现在的访问方式和限制",
    ),
    "docs/decisions.md": (
        "## 1. 性能与门禁",
        "## 2. 部署",
        "## 3. 存储模型",
        "## 4. 工程与仓库",
    ),
}
# docs/contracts/*.md headers (including the generic-data-v7 status line) are
# covered by contract_snapshot_claims(), so they are not listed twice.

def required_facts(deployed_schema: int) -> list[tuple[str, re.Pattern]]:
    """Facts every CURRENT_DOCS region must state.

    The schema number is the DEPLOYED one: a current region describes the
    deployed instance, so it keeps "已部署：schema 21" while the code constant
    is 22. Tool counts come from the source registries.
    """
    return [
        (
            f"schema {deployed_schema}",
            re.compile(rf"schema[\s:=v]*{deployed_schema}(?!\d)", re.IGNORECASE),
        ),
        (
            f"{EXPECTED_DEPLOYED_DATA_TOOLS} data tools",
            re.compile(
                rf"\b{EXPECTED_DEPLOYED_DATA_TOOLS}\s+data\s+tools?\b",
                re.IGNORECASE,
            ),
        ),
        (
            "10 acquisition tools",
            re.compile(r"\b10\s+acquisition\s+tools?\b", re.IGNORECASE),
        ),
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
# Authoritative deployed schema row: "| 数据库 schema | 已部署：schema 21 ... |".
# The code constant is a build fact; this row is the deployment fact, so a
# current region may keep the deployed number while the code moves ahead.
DEPLOYED_SCHEMA_ROW = re.compile(
    r"\|\s*数据库\s*schema\s*\|[^\n]*?已部署\s*[:：]?\s*schema\s*(\d+)"
)

# --- P1-2 (#296): sentence-scoped current-state rules ---------------------
# The P1-1 texts legitimately mention old values in progression form
# ("schema 20 ... 已被 schema 21 取代", "自 preview.32 起部署", "v6 -> v7"),
# so these rules key on assertion words and exempt per SENTENCE, not per
# paragraph: a table row marked 历史 must not exempt the rows around it.
CURRENT_ASSERTION = re.compile(
    r"当前部署|已部署|线上|当前实现|当前生产|当前版本|当前运行|运行的是"
    r"|current(?:ly)?\s+(?:deployed|running)|in\s+production",
    re.IGNORECASE,
)
SCHEMA_CLAIM = re.compile(r"schema[\s:=v\-]*(\d{1,2})", re.IGNORECASE)
ACQUIRE_TOOLS_CLAIM = re.compile(
    r"\b(\d+)\s*(?:个\s*)?acquisition\s*(?:tools?|工具)", re.IGNORECASE
)
# A bare "N tools" / "N 个工具": the number must be one of the two registries.
BARE_TOOLS_CLAIM = re.compile(r"\b(\d+)\s*(?:个\s*)?(?:tools?\b|工具)", re.IGNORECASE)
# A sentence that explicitly labels a count as the code/build fact (rather than
# the deployed instance) is checked against the registry, not the deployed
# constant. This is the tool-count counterpart of the STATUS "已部署：schema N；
# 代码：schema M（未部署）" convention.
CODE_FACT_MARKERS = ("代码", "未部署", "not deployed", "code (main)", "code:")
# Sentence boundaries. The timeline arrow is handled separately (below): a
# transition chain is exempt as a whole, not split into claims.
SENTENCE_SPLIT = re.compile(r"[。；;！？]")
# A timeline transition ("6/6 → 5/6") describes how a value changed, not what it
# is now; only STATUS uses this marker.
STATUS_TRANSITION = "→"
HISTORY_IN_SENTENCE = (
    "historical",
    "Historical",
    "已取代",
    "已被",
    "及更早",
    "legacy",
    "当时",
    "写作时",
)
# 历史 as a marker of "this text is history", not as a data concept: "某个历史版本"
# (a historical data version) must not exempt the sentence from the rules.
HISTORY_WORD = re.compile(r"历史(?!版本|记录|数据|时点|区间|窗口|行情|序列)")


def is_historical(sentence: str) -> bool:
    return bool(HISTORY_WORD.search(sentence)) or any(
        marker in sentence for marker in HISTORY_IN_SENTENCE
    )

# docs/contracts/ holds historical snapshots; none may call itself current.
CONTRACTS_DIR = ROOT / "docs" / "contracts"
CONTRACT_STATUS_LINE = re.compile(r"\*\*状态\*\*\s*[:：]\s*(.+)")
# "当前契约见 X" points elsewhere and is fine; the status may not call THIS
# file the current contract.
CONTRACT_CURRENT_SELF_CLAIM = re.compile(
    r"^\s*(?:当前|current)\b|当前契约(?!见)|current\s+contract", re.IGNORECASE
)

# docs/decisions.md: the 状态 column is a current-state claim. Its allowed
# values are the ones the file itself declares.
DECISION_STATUS_DECL = re.compile(r"状态取值\s*[:：]\s*(.+)")

# docs/STATUS.md internal consistency (P1-1, item 7c): one authoritative
# "| 热 generation | N/M" row, one "| 部署版本 |" row, and one 服务 table whose
# 工具数 column must match the source registries.
HOT_GENERATION_ROW = re.compile(r"\|\s*热\s*generation\s*\|\s*\**\s*(\d+)\s*/\s*(\d+)")
HOT_FRACTION = re.compile(r"\b(\d{1,2})\s*/\s*(\d{1,2})\b")
# A projection ("activate 后回收到约 4/6") is not a current-state claim.
HOT_PROJECTION = ("预期", "预计", "约", "回收到", "将", "会")
HOT_ACTIVE = re.compile(r"(\d+)\s*active\b", re.IGNORECASE)
HOT_SUPERSEDED = re.compile(r"(\d+)\s*superseded\b", re.IGNORECASE)
STATUS_CURRENT_MARKER = re.compile(
    r"当前|现为|目前|下一次|\bnow\b|热\s*generation|\bhot\b", re.IGNORECASE
)
DEPLOYED_VERSION_MARKER = re.compile(
    r"部署版本|当前部署|已部署|serverInfo\.version|server_version"
)


def deployed_schema_number(status_text: str | None) -> int | None:
    """The deployed schema from STATUS.md's authoritative row, if present."""
    if not status_text:
        return None
    match = DEPLOYED_SCHEMA_ROW.search(status_text)
    return int(match.group(1)) if match else None


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


def _split_block(text: str, line_map: list[int]) -> list[tuple[str, list[int]]]:
    """Split a block into sentences, keeping each character's source line."""
    parts: list[tuple[str, list[int]]] = []
    start = 0
    for match in SENTENCE_SPLIT.finditer(text):
        parts.append((text[start : match.start()], line_map[start : match.start()]))
        start = match.end()
    parts.append((text[start:], line_map[start:]))
    return [(part, lines) for part, lines in parts if part.strip()]


def claim_sentences(region: str, offset: int = 0) -> list[tuple[str, list[int]]]:
    """(sentence, per-character source line) pairs for stale-claim scanning.

    A markdown table row or a list item is its own sentence, so a 历史 marker in
    one row cannot exempt the rest of the table. Wrapped prose lines are joined
    before splitting (Chinese text wraps mid-sentence), and the line map keeps
    every reported failure on the line that carries the claim.
    """
    sentences: list[tuple[str, list[int]]] = []
    buffer: list[str] = []
    buffer_lines: list[int] = []

    def flush() -> None:
        nonlocal buffer, buffer_lines
        if not buffer:
            return
        sentences.extend(_split_block("".join(buffer), buffer_lines))
        buffer = []
        buffer_lines = []

    for lineno, line in enumerate(region.splitlines(), start=1):
        number = lineno + offset
        if not line.strip():
            flush()
            continue
        if line.lstrip().startswith("|") or re.match(r"\s*(?:[-*+]|\d+\.)\s", line):
            flush()
            sentences.extend(_split_block(line, [number] * len(line)))
        else:
            buffer.append(line)
            buffer_lines.extend([number] * len(line))
    flush()
    return sentences


def claim_line(line_map: list[int], position: int) -> int:
    """Source line of a match inside a joined sentence."""
    if not line_map:
        return 0
    return line_map[min(position, len(line_map) - 1)]


def expected_data_tools(sentence: str, deployed: int, code: int | None) -> int:
    """Deployed count by default; a code-labelled sentence uses the registry."""
    if code is not None and any(marker in sentence for marker in CODE_FACT_MARKERS):
        return code
    return deployed


def stale_current_region_claims(
    region: str,
    schema: int,
    revision: str,
    data_tools: int,
    acquire_tools: int,
    offset: int = 0,
    code_data_tools: int | None = None,
) -> list[tuple[int, str, str]]:
    """(line, rule, detail) for each stale current-state claim in a region.

    * an old schema / capability revision written as the deployed one;
    * a tool count that disagrees with the source registries.
    """
    wanted_revision = revision.rsplit("-v", 1)[-1]
    found: list[tuple[int, str, str]] = []
    for sentence, line_map in claim_sentences(region, offset):
        if is_historical(sentence):
            continue
        if CURRENT_ASSERTION.search(sentence):
            for m in SCHEMA_CLAIM.finditer(sentence):
                if int(m.group(1)) != schema:
                    found.append(
                        (
                            claim_line(line_map, m.start()),
                            "stale-schema",
                            f"{m.group(0)!r} written as deployed (deployed: schema {schema})",
                        )
                    )
            for m in REVISION_CLAIM.finditer(sentence):
                if m.group(1) != wanted_revision:
                    found.append(
                        (
                            claim_line(line_map, m.start()),
                            "stale-revision",
                            f"{m.group(0)} written as deployed (deployed: {revision})",
                        )
                    )
        sentence_tools = expected_data_tools(sentence, data_tools, code_data_tools)
        for m in DATA_TOOLS_CLAIM.finditer(sentence):
            if int(m.group(1)) != sentence_tools:
                found.append(
                    (
                        claim_line(line_map, m.start()),
                        "data-tool-count",
                        f"{m.group(0)} (expected {sentence_tools} data tools)",
                    )
                )
        for m in ACQUIRE_TOOLS_CLAIM.finditer(sentence):
            if int(m.group(1)) != acquire_tools:
                found.append(
                    (
                        claim_line(line_map, m.start()),
                        "acquire-tool-count",
                        f"{m.group(0)} (registry: {acquire_tools} acquisition tools)",
                    )
                )
        for m in BARE_TOOLS_CLAIM.finditer(sentence):
            if int(m.group(1)) not in (sentence_tools, acquire_tools):
                found.append(
                    (
                        claim_line(line_map, m.start()),
                        "tool-count",
                        f"{m.group(0)} is neither {sentence_tools} data nor "
                        f"{acquire_tools} acquisition tools",
                    )
                )
    return found


def contract_name_claims(rel: str) -> list[tuple[int, str, str]]:
    """A snapshot file name may not call itself current (P1-1 renamed one)."""
    name = Path(rel).name
    if re.search(r"current|当前", name, re.IGNORECASE):
        return [(1, "snapshot-name-claims-current", name)]
    return []


def contract_snapshot_claims(
    text: str,
    schema: int,
    revision: str,
    data_tools: int,
    acquire_tools: int,
    code_data_tools: int | None = None,
) -> list[tuple[int, str, str]]:
    """Snapshot rules for a docs/contracts/*.md file (P1-2, #296).

    Its status line may not claim to be the current contract, and its header may
    not present an old schema / revision / tool count as the deployed one.
    """
    found: list[tuple[int, str, str]] = []
    header = top_current_region(text, None)
    status = CONTRACT_STATUS_LINE.search(header)
    if not status:
        found.append((1, "snapshot-status-missing", "header has no '**状态**：' line"))
        return found
    lineno = header[: status.start()].count("\n") + 1
    if CONTRACT_CURRENT_SELF_CLAIM.search(status.group(1)):
        found.append((lineno, "snapshot-status-claims-current", status.group(1).strip()))
    for line, rule, detail in stale_current_region_claims(
        header,
        schema,
        revision,
        data_tools,
        acquire_tools,
        code_data_tools=code_data_tools,
    ):
        found.append((line, f"snapshot-{rule}", detail))
    return found


def decision_status_claims(text: str) -> list[tuple[int, str, str]]:
    """The docs/decisions.md 状态 column must use the declared status values."""
    found: list[tuple[int, str, str]] = []
    decl = DECISION_STATUS_DECL.search(text)
    if not decl:
        return [(1, "decision-status-vocabulary-missing", "no '状态取值：' line")]
    allowed = re.findall(r"\*\*([^*]+)\*\*", decl.group(1))
    if not allowed:
        return [(1, "decision-status-vocabulary-missing", decl.group(1).strip())]
    status_index: int | None = None
    seen_table = False
    for lineno, line in enumerate(text.splitlines(), start=1):
        stripped = line.strip()
        if not stripped.startswith("|"):
            status_index = None
            continue
        cells = [cell.strip() for cell in stripped.strip("|").split("|")]
        if "状态" in cells:
            status_index = cells.index("状态")
            seen_table = True
            continue
        if status_index is None or status_index >= len(cells):
            continue
        if all(re.fullmatch(r":?-{2,}:?", cell) for cell in cells):
            continue
        cell = cells[status_index].strip("* ")
        if not any(cell.startswith(value) for value in allowed):
            found.append(
                (
                    lineno,
                    "decision-status-undeclared",
                    f"{cell[:40]!r} is not one of {allowed}",
                )
            )
    if not seen_table:
        found.append((1, "decision-status-column-missing", "no table with a 状态 column"))
    return found


def status_consistency_errors(
    text: str,
    data_tools: int,
    acquire_tools: int,
    code_data_tools: int | None = None,
) -> list[tuple[int, str, str]]:
    """docs/STATUS.md must not contradict itself (P1-1, item 7c).

    One authoritative row per fact: "| 热 generation | N/M", "| 部署版本 |", and
    the 服务 table's 工具数 column. Current-state restatements elsewhere must
    agree; historical segments (历史 / 当时 / timeline arrows) are exempt.
    """
    found: list[tuple[int, str, str]] = []
    hot = cap = None
    hot_line = 0
    for lineno, line in enumerate(text.splitlines(), start=1):
        m = HOT_GENERATION_ROW.search(line)
        if m:
            hot, cap, hot_line = int(m.group(1)), int(m.group(2)), lineno
            break
    if hot is None:
        found.append(
            (1, "status-hot-row-missing", "no authoritative '| 热 generation | N/M' row")
        )

    deployed = STATUS_DEPLOYED_VERSION.search(text)
    deployed_preview = deployed.group(1) if deployed else None

    for sentence, line_map in claim_sentences(text):
        if is_historical(sentence):
            continue
        if STATUS_TRANSITION in sentence:
            continue
        projecting = any(word in sentence for word in HOT_PROJECTION)
        if STATUS_CURRENT_MARKER.search(sentence) and hot is not None and not projecting:
            stated_hot = None
            for m in HOT_FRACTION.finditer(sentence):
                count, capacity = int(m.group(1)), int(m.group(2))
                if capacity == cap or re.search(
                    r"热|\bhot\b|generation", sentence, re.IGNORECASE
                ):
                    if count != hot:
                        found.append(
                            (
                                claim_line(line_map, m.start()),
                                "status-hot-count",
                                f"{m.group(0)} disagrees with the 热 generation row "
                                f"line {hot_line}: {hot}/{cap}",
                            )
                        )
                    if capacity != cap:
                        found.append(
                            (
                                claim_line(line_map, m.start()),
                                "status-hot-capacity",
                                f"{m.group(0)} disagrees with the 热 generation capacity "
                                f"{cap} (line {hot_line})",
                            )
                        )
                    stated_hot = count
            active = HOT_ACTIVE.search(sentence)
            superseded = HOT_SUPERSEDED.search(sentence)
            if active and superseded:
                total = int(active.group(1)) + int(superseded.group(1))
                expected_total = stated_hot if stated_hot is not None else hot
                if total != expected_total:
                    found.append(
                        (
                            claim_line(line_map, active.start()),
                            "status-hot-breakdown",
                            f"{active.group(0)} + {superseded.group(0)} = {total}, "
                            f"but this statement / the 热 generation row "
                            f"(line {hot_line}) says {expected_total}",
                        )
                    )
        if deployed_preview and DEPLOYED_VERSION_MARKER.search(sentence):
            for m in PREVIEW_CLAIM.finditer(sentence):
                if m.group(1) != deployed_preview:
                    found.append(
                        (
                            claim_line(line_map, m.start()),
                            "status-deployed-version",
                            f"{m.group(0)} disagrees with the 部署版本 row: "
                            f"preview.{deployed_preview}",
                        )
                    )
        sentence_tools = expected_data_tools(sentence, data_tools, code_data_tools)
        for m in DATA_TOOLS_CLAIM.finditer(sentence):
            if int(m.group(1)) != sentence_tools:
                found.append(
                    (
                        claim_line(line_map, m.start()),
                        "status-data-tool-count",
                        f"{m.group(0)} (expected {sentence_tools} data tools)",
                    )
                )
        for m in ACQUIRE_TOOLS_CLAIM.finditer(sentence):
            if int(m.group(1)) != acquire_tools:
                found.append(
                    (
                        claim_line(line_map, m.start()),
                        "status-acquire-tool-count",
                        f"{m.group(0)} (registry: {acquire_tools} acquisition tools)",
                    )
                )
        pairs = list(re.finditer(r"(\d{1,3})\s*/\s*(\d{1,3})\s*工具", sentence))
        for m in BARE_TOOLS_CLAIM.finditer(sentence):
            if any(pair.start() <= m.start() < pair.end() for pair in pairs):
                continue
            value = int(m.group(1))
            prefix = sentence[: m.start()].lower()
            acquire_at = prefix.rfind("acquire")
            data_at = prefix.rfind("data")
            expected = None
            if acquire_at > data_at:
                expected = acquire_tools
            elif data_at >= 0:
                expected = sentence_tools
            if expected is not None and value != expected:
                found.append(
                    (
                        claim_line(line_map, m.start()),
                        "status-tool-count",
                        f"{m.group(0)} (expected {expected} tools)",
                    )
                )
            elif expected is None and value not in (sentence_tools, acquire_tools):
                found.append(
                    (
                        claim_line(line_map, m.start()),
                        "status-tool-count",
                        f"{m.group(0)} is neither {sentence_tools} data nor "
                        f"{acquire_tools} acquisition tools",
                    )
                )
        for m in pairs:
            if (int(m.group(1)), int(m.group(2))) != (data_tools, acquire_tools):
                found.append(
                    (
                        claim_line(line_map, m.start()),
                        "status-tool-pair",
                        f"{m.group(0)} (registry: {data_tools}/{acquire_tools} tools)",
                    )
                )

    for lineno, line in enumerate(text.splitlines(), start=1):
        stripped = line.strip()
        if not stripped.startswith("|"):
            continue
        cells = [cell.strip().strip("`") for cell in stripped.strip("|").split("|")]
        unit = cells[0] if cells else ""
        expected = {
            "x-data-mcp-data": data_tools,
            "x-data-mcp-acquire": acquire_tools,
        }.get(unit)
        if expected is None:
            continue
        numbers = [cell for cell in cells[1:] if re.fullmatch(r"\d+", cell)]
        if not numbers:
            found.append(
                (lineno, "status-service-tool-count", f"no numeric 工具数 cell for {unit}")
            )
        elif int(numbers[0]) != expected:
            found.append(
                (
                    lineno,
                    "status-service-tool-count",
                    f"{unit} 工具数 {numbers[0]} (registry: {expected})",
                )
            )
    return found


def region_start_line(text: str, start_heading: str | None) -> int:
    """0-based line offset of the region, so failures can cite the file line."""
    if start_heading is None:
        return 0
    for i, line in enumerate(text.splitlines(keepends=True)):
        if line.rstrip() == start_heading:
            return i
    return 0


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
    status_text = (
        status_path.read_text(encoding="utf-8") if status_path.exists() else None
    )
    allowed_previews = deployed_preview_numbers(status_text, EXPECTED_PACKAGE_VERSION)
    deployed_schema = deployed_schema_number(status_text)
    check(
        "docs/STATUS.md declares the deployed schema",
        deployed_schema is not None,
        "no '| 数据库 schema | 已部署：schema N' row",
    )
    if deployed_schema is None:
        # Keep checking the rest against the code schema so the run reports
        # every failure instead of stopping at the missing row.
        deployed_schema = compact
    check(
        "deployed schema is not ahead of the code schema",
        deployed_schema <= compact,
        f"STATUS says {deployed_schema}, code builds {compact}",
    )

    for rel, heading in CURRENT_DOCS.items():
        region = top_current_region(
            (ROOT / rel).read_text(encoding="utf-8"), heading
        )
        check(f"{rel} current region located", bool(region), f"missing {heading}")
        for label, pattern in required_facts(deployed_schema):
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
            EXPECTED_DEPLOYED_DATA_TOOLS,
            EXPECTED_SKILL_CAPABILITY_REVISION,
        ):
            check(
                f"{rel} current region line {lineno} states a stale deployment fact",
                False,
                f"{kind}: {line.strip()}",
            )

    # P1-2 (#296): current-state regions P1-1 corrected, docs/contracts
    # snapshots, the decisions status column, and STATUS self-consistency.
    # Every failure carries file:line + rule name so it is locatable.
    registry_data_tools = len(data_names)
    registry_acquire_tools = len(acquire_names)

    def add_claims(rel: str, claims: list[tuple[int, str, str]]) -> None:
        for lineno, rule, detail in claims:
            failures.append(f"{rel}:{lineno}: {rule}: {detail}")

    for rel, headings in CURRENT_REGIONS.items():
        path = ROOT / rel
        check(f"{rel} exists", path.is_file())
        if not path.is_file():
            continue
        text = path.read_text(encoding="utf-8")
        for heading in headings:
            region = top_current_region(text, heading)
            check(f"{rel} current region located", bool(region), f"missing {heading}")
            offset = region_start_line(text, heading)
            add_claims(
                rel,
                stale_current_region_claims(
                    region,
                    deployed_schema,
                    EXPECTED_DATA_CAPABILITY_REVISION,
                    EXPECTED_DEPLOYED_DATA_TOOLS,
                    registry_acquire_tools,
                    offset,
                    code_data_tools=registry_data_tools,
                ),
            )
            for lineno, line in enumerate(region.splitlines(), start=1):
                if STALE_EIGHT_TOOLS.search(line):
                    add_claims(rel, [(lineno + offset, "stale-eight-tools", line.strip())])

    for path in sorted(CONTRACTS_DIR.iterdir()):
        if not path.is_file():
            continue
        rel = path.relative_to(ROOT).as_posix()
        add_claims(rel, contract_name_claims(rel))
        if path.suffix != ".md":
            continue
        add_claims(
            rel,
            contract_snapshot_claims(
                path.read_text(encoding="utf-8"),
                deployed_schema,
                EXPECTED_DATA_CAPABILITY_REVISION,
                EXPECTED_DEPLOYED_DATA_TOOLS,
                registry_acquire_tools,
                code_data_tools=registry_data_tools,
            ),
        )

    decisions_rel = "docs/decisions.md"
    decisions_path = ROOT / decisions_rel
    check(f"{decisions_rel} exists", decisions_path.is_file())
    if decisions_path.is_file():
        add_claims(
            decisions_rel,
            decision_status_claims(decisions_path.read_text(encoding="utf-8")),
        )

    if status_path.is_file():
        add_claims(
            "docs/STATUS.md",
            status_consistency_errors(
                status_path.read_text(encoding="utf-8"),
                EXPECTED_DEPLOYED_DATA_TOOLS,
                registry_acquire_tools,
                code_data_tools=registry_data_tools,
            ),
        )

    if failures:
        for failure in failures:
            print(f"FAIL {failure}", file=sys.stderr)
        return 1
    print(
        f"current-facts OK: {EXPECTED_PACKAGE_VERSION}, fresh compact schema "
        f"{EXPECTED_COMPACT_SCHEMA} (deployed {deployed_schema}; legacy upgrade "
        f"source schema {EXPECTED_LEGACY_SCHEMA}), {EXPECTED_DATA_TOOLS} data tools, "
        f"{EXPECTED_ACQUIRE_TOOLS} acquisition tools, "
        f"{len(CURRENT_DOCS)} current docs + {len(CURRENT_REGIONS)} current "
        f"regions + "
        f"{len([p for p in CONTRACTS_DIR.iterdir() if p.is_file()])} contract "
        f"snapshots consistent"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
