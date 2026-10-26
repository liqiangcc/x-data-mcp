"""Unit tests for the stale-deployment-claim rule of check_current_facts.py."""
import importlib.util
import subprocess
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location(
    "check_current_facts", SCRIPTS / "check_current_facts.py"
)
cf = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cf)

ALLOWED = {"31"}


def claims(text: str, deployed: str = "generic-data-v6") -> list[str]:
    # Default mirrors this branch's deployed-revision constant (post-switch).
    return [
        kind
        for _, kind, _ in cf.stale_deployment_claims(
            text, ALLOWED, 21, deployed
        )
    ]


class StaleDeploymentClaims(unittest.TestCase):
    def test_current_facts_pass(self):
        v6_text = (
            "本机已部署 v0.2.0-preview.31；data server 21 data tools，"
            "capability revision `generic-data-v6`，10 acquisition tools。\n"
        )
        self.assertEqual(claims(v6_text), [])
        v5_text = (
            "本机已部署 v0.2.0-preview.31；data server 21 data tools，"
            "capability revision `generic-data-v5`，10 acquisition tools。\n"
        )
        self.assertEqual(claims(v5_text, "generic-data-v5"), [])

    def test_v5_claim_is_flagged_while_deployed_revision_is_v6(self):
        # Post-switch the deployed instance declares v6; a current region
        # naming the previous deployed revision (v5) is stale.
        text = "当前部署注册 21 data tools 并宣告 `generic-data-v5`。\n"
        found = claims(text)
        self.assertEqual(len(found), 1, found)
        self.assertIn("generic-data-v5", found[0])
        self.assertEqual(
            claims("当前部署注册 21 data tools 并宣告 `generic-data-v6`。\n"), []
        )

    def test_the_old_sentences_are_flagged(self):
        # The wording that main@9189b68 still carried after the preview.31 deploy.
        old = (
            "当前部署 preview.29（`4493d45`）：data socket 注册 20 data tools，\n"
            "当前开发分支（XP-014，未部署）注册 21 data tools 并宣告 `generic-data-v5`；\n"
            "preview.29 部署实例仍为 20 data tools / `generic-data-v3`。\n"
        )
        found = claims(old)
        self.assertEqual(sum("preview.29" in k for k in found), 2, found)
        self.assertEqual(sum("20 data tools" in k for k in found), 2, found)
        self.assertEqual(sum("generic-data-v3" in k for k in found), 1, found)

    def test_chinese_count_form_is_flagged(self):
        self.assertEqual(len(claims("共 20 个 data tools。\n")), 1)
        self.assertEqual(claims("共 21 个 data 工具。\n"), [])

    def test_historical_paragraph_is_exempt(self):
        text = "历史记录（preview.29 时期）：20 data tools / generic-data-v3。\n"
        self.assertEqual(claims(text), [])
        text = "Historical note: preview.29 had 20 data tools.\n"
        self.assertEqual(claims(text), [])

    def test_marker_exempts_the_whole_paragraph_including_list_items(self):
        text = (
            "- 历史实测：preview.27 的数字\n"
            "- preview.28 的另一条（同一段落）\n"
            "\n"
            "preview.28 单独的当前段落。\n"
        )
        self.assertEqual(len(claims(text)), 1)

    def test_historical_heading_exempts_until_same_level_heading(self):
        text = (
            "## 当前\n\npreview.28 在当前段落。\n\n"
            "### 历史沿革\n\npreview.27 / 20 data tools\n\n#### 子节\n\npreview.26\n\n"
            "### 另一个小节\n\npreview.25\n"
        )
        found = claims(text)
        self.assertEqual(len(found), 2, found)  # preview.28 and preview.25

    def test_deployed_version_comes_from_status_row(self):
        status = "| 部署版本 | v0.2.0-preview.32 @ `abc` | x |\n"
        allowed = cf.deployed_preview_numbers(status, "0.2.0-preview.31")
        self.assertEqual(allowed, {"31", "32"})
        self.assertEqual(
            cf.deployed_preview_numbers(None, "0.2.0-preview.31"), {"31"}
        )

    def test_limitations_documented(self):
        # Known false negative: a stale claim in a paragraph that also holds
        # a historical marker word is not reported. Documented, not hidden.
        text = "历史上有 20 data tools，现在仍为 20 data tools。\n"
        self.assertEqual(claims(text), [])


class RealDocs(unittest.TestCase):
    def test_script_passes_on_the_repository(self):
        result = subprocess.run(
            ["python3", str(SCRIPTS / "check_current_facts.py")],
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)


REGION_SCHEMA = 21
REGION_REVISION = "generic-data-v7"
REGION_DATA_TOOLS = 23
REGION_ACQUIRE_TOOLS = 10


def region_claims(text: str, offset: int = 0) -> list[tuple[int, str, str]]:
    return cf.stale_current_region_claims(
        text,
        REGION_SCHEMA,
        REGION_REVISION,
        REGION_DATA_TOOLS,
        REGION_ACQUIRE_TOOLS,
        offset,
    )


def rules(claims: list[tuple[int, str, str]]) -> list[str]:
    return [rule for _, rule, _ in claims]


class StaleCurrentRegions(unittest.TestCase):
    """P1-2 (#296): the regions P1-1 corrected (its 7d class)."""

    def test_deployed_schema_is_accepted(self):
        self.assertEqual(
            region_claims("当前部署的是 schema 21 的单版本存储。\n"), []
        )

    def test_old_schema_written_as_deployed_is_flagged(self):
        # design.md before the P1-1 correction (item 7d).
        old = (
            "本文中「版本」等表述描述的是**版本链模型**：它是已部署的\n"
            "schema 20（v0.2.0-preview.31）的行为，但**不再被视为目标模型**。"
            "两者并列：线上仍是\n"
            "schema 20，schema 21 合并并部署之前不能当作已部署事实。\n"
        )
        found = region_claims(old)
        self.assertEqual(rules(found), ["stale-schema", "stale-schema"], found)
        self.assertEqual([line for line, _, _ in found], [2, 3])

    def test_superseded_schema_is_not_flagged(self):
        # The P1-1 wording: schema 20 only appears as superseded history.
        text = (
            "它是\n"
            "schema 20（v0.2.0-preview.31 及更早）的行为，**已被 schema 21 取代**（历史，不再被视为目标模型）。\n"
            "当前部署的是 schema 21 的单版本存储：每 `(实体, 周期)` 只存当前序列。\n"
            "schema 21 自 preview.32 起部署（旧 v20 库归档）。\n"
        )
        self.assertEqual(region_claims(text), [])

    def test_old_revision_written_as_deployed_is_flagged(self):
        # generic-data-v7.md before the P1-1 correction (item 7d #9).
        old = "**状态**：尚未部署；当前部署仍为 `generic-data-v6` / compact schema 21。\n"
        found = region_claims(old)
        self.assertEqual(rules(found), ["stale-revision"], found)
        self.assertIn("generic-data-v6", found[0][2])

    def test_revision_progression_is_not_flagged(self):
        text = (
            "`capability_revision`：`generic-data-v6` → `generic-data-v7`。\n"
            "`related_tasks` 有界；capability 升到 `generic-data-v5`，Skill 同步更新。\n"
            "当前部署已是 `generic-data-v7`（自 preview.33 起）。\n"
        )
        self.assertEqual(region_claims(text), [])

    def test_tool_counts_must_match_the_registries(self):
        # design.md:69 before the P1-1 correction said 21 data tools (item 7d #2).
        old = "| MCP | data socket 21 data tools、acquisition socket 10 acquisition tools | x |\n"
        self.assertEqual(rules(region_claims(old)), ["data-tool-count"])
        good = "| MCP | data socket 23 data tools、acquisition socket 10 acquisition tools | x |\n"
        self.assertEqual(region_claims(good), [])
        bad_acquire = "| MCP | data socket 23 data tools、acquisition socket 8 acquisition tools | x |\n"
        self.assertEqual(rules(region_claims(bad_acquire)), ["acquire-tool-count"])

    def test_bare_tool_count_must_be_one_of_the_registries(self):
        # The v6 switch checklist header said "21 tools" (stale current fact).
        self.assertEqual(rules(region_claims("对齐到 preview.31 / 21 tools / v5。\n")), ["tool-count"])
        self.assertEqual(region_claims("双 server 共 23 tools。\n"), [])

    def test_data_history_word_is_not_a_history_marker(self):
        # verified-reads.md before the P1-1 correction (item 7d #5): "某个历史版本" is a data
        # concept, not a marker that the sentence is historical.
        old = (
            "下文把「按 `version_id` 读取某个历史版本」作为完整性读取的前提，"
            "这是版本链模型（已部署的 schema 20 沿用其语义）。\n"
        )
        found = region_claims(old)
        self.assertEqual(rules(found), ["stale-schema"], found)

    def test_history_marked_sentence_is_exempt(self):
        text = "历史实测（preview.27/schema20）：5,222 items / 616s。\n"
        self.assertEqual(region_claims(text), [])

    def test_history_marker_exempts_one_sentence_not_the_table(self):
        text = (
            "| 性能 | 历史实测（preview.27/schema20）：5,222 items | — |\n"
            "| MCP | data socket 21 data tools | x |\n"
        )
        found = region_claims(text)
        self.assertEqual(rules(found), ["data-tool-count"], found)
        self.assertEqual(found[0][0], 2)

    def test_wrapped_prose_lines_are_joined(self):
        # The assertion and the schema number sit on different source lines.
        text = "两者并列：线上仍是\nschema 20，schema 21 不能当作已部署事实。\n"
        found = region_claims(text)
        self.assertEqual(rules(found), ["stale-schema"], found)

    def test_offset_reports_file_lines(self):
        found = region_claims("intro\n\n线上仍是 schema 20。\n", offset=10)
        self.assertEqual([line for line, _, _ in found], [13])


class DeployedSchemaRow(unittest.TestCase):
    """The deployed schema is a STATUS fact; the code schema may be ahead."""

    def test_row_is_parsed(self):
        text = (
            "| 数据库 schema | 已部署：schema 21（`pragma user_version=21`）；"
            "代码：schema 22（未部署） | src |\n"
        )
        self.assertEqual(cf.deployed_schema_number(text), 21)

    def test_absent_row_is_none(self):
        self.assertIsNone(
            cf.deployed_schema_number("| 部署版本 | v0.2.0-preview.35 |\n")
        )
        self.assertIsNone(cf.deployed_schema_number(None))

    def test_code_schema_ahead_keeps_deployed_claim_valid(self):
        # M1 PR-1 builds schema 22 while the deployed instance stays on 21:
        # the current region states the deployed number, and the code number
        # is explicitly marked as not deployed.
        text = "已部署实例为 compact schema 21；代码（main）为 schema 22（M1 PR-1，未部署）。\n"
        self.assertEqual(region_claims(text), [])

    def test_deployed_claim_ahead_of_status_is_flagged(self):
        text = "当前部署的是 schema 22 的单版本存储。\n"
        found = region_claims(text)
        self.assertEqual(rules(found), ["stale-schema"], found)

    def test_required_facts_follow_the_deployed_schema(self):
        facts = cf.required_facts(21)
        labels = [label for label, _ in facts]
        self.assertIn("schema 21", labels)
        self.assertNotIn("schema 22", labels)
        schema = dict(facts)["schema 21"]
        self.assertTrue(schema.search("已部署：schema 21"))
        self.assertFalse(schema.search("代码：schema 22"))


CONTRACT_OK = """# 某历史快照

**状态**：历史快照。当前契约见 [generic-data-v7](generic-data-v7.md)。

正文。

## 1. 细节

正文。
"""


def contract_claims(text: str) -> list[tuple[int, str, str]]:
    return cf.contract_snapshot_claims(
        text,
        REGION_SCHEMA,
        REGION_REVISION,
        REGION_DATA_TOOLS,
        REGION_ACQUIRE_TOOLS,
    )


class ContractSnapshots(unittest.TestCase):
    """P1-2 (#296) requirement 4: snapshots must not call themselves current."""

    def test_historical_snapshot_passes(self):
        self.assertEqual(contract_claims(CONTRACT_OK), [])

    def test_file_name_claiming_current_is_rejected(self):
        self.assertEqual(cf.contract_name_claims("docs/contracts/mcp-tools-current.json"),
                         [(1, "snapshot-name-claims-current", "mcp-tools-current.json")])
        self.assertEqual(
            rules(cf.contract_name_claims("docs/contracts/当前契约.md")),
            ["snapshot-name-claims-current"],
        )
        self.assertEqual(
            cf.contract_name_claims("docs/contracts/mcp-tools-b3fe2b3.json"), []
        )

    def test_missing_status_line_is_rejected(self):
        text = "# 快照\n\n正文。\n"
        self.assertEqual(rules(contract_claims(text)), ["snapshot-status-missing"])

    def test_status_claiming_to_be_current_is_rejected(self):
        text = "# 快照\n\n**状态**：当前契约。\n"
        self.assertEqual(
            rules(contract_claims(text)), ["snapshot-status-claims-current"]
        )

    def test_header_old_deployment_is_rejected(self):
        # v6-switch-checklist.md before P1-2.
        text = (
            "# generic-data-v6 / schema 21 部署日切换清单\n\n"
            "**状态**：历史快照。\n\n"
            "**现在不要改这些文件**——已部署实例仍是\n"
            "preview.31 / schema 20 / `generic-data-v5`（STATUS），\n"
            "提前改会让检查失败。\n"
        )
        found = contract_claims(text)
        self.assertEqual(
            sorted(rules(found)),
            ["snapshot-stale-revision", "snapshot-stale-schema"],
            found,
        )
        self.assertEqual({line for line, _, _ in found}, {6})

    def test_repository_contracts_pass(self):
        for path in sorted(cf.CONTRACTS_DIR.iterdir()):
            if not path.is_file():
                continue
            rel = path.relative_to(cf.ROOT).as_posix()
            self.assertEqual(cf.contract_name_claims(rel), [], rel)
            if path.suffix == ".md":
                self.assertEqual(contract_claims(path.read_text(encoding="utf-8")), [], rel)


DECISIONS_OK = """# 决策记录

状态取值：**生效**、**实施中**（已决定，未部署）、**已取代**。

## 1. 一节

| # | 决策 | 状态 | 来源 |
|---|---|---|---|
| D-01 | 某决定 | 生效 | PR #1 |
| D-02 | 某决定 | 已取代（D-01） | PR #2 |
"""


class DecisionsStatus(unittest.TestCase):
    """P1-2 (#296): the decisions 状态 column must be machine-checkable."""

    def test_declared_status_values_pass(self):
        self.assertEqual(cf.decision_status_claims(DECISIONS_OK), [])

    def test_undeclared_status_value_is_rejected(self):
        text = DECISIONS_OK.replace("| D-02 | 某决定 | 已取代（D-01） |", "| D-02 | 某决定 | 待定 |")
        found = cf.decision_status_claims(text)
        self.assertEqual(rules(found), ["decision-status-undeclared"], found)
        self.assertEqual(found[0][0], 10)

    def test_missing_vocabulary_is_rejected(self):
        text = DECISIONS_OK.replace("状态取值：", "状态：")
        self.assertEqual(
            rules(cf.decision_status_claims(text)),
            ["decision-status-vocabulary-missing"],
        )

    def test_missing_status_column_is_rejected(self):
        text = "# 决策记录\n\n状态取值：**生效**。\n\n| # | 决策 |\n|---|---|\n| D-01 | x |\n"
        self.assertEqual(
            rules(cf.decision_status_claims(text)),
            ["decision-status-column-missing"],
        )

    def test_repository_decisions_pass(self):
        text = (cf.ROOT / "docs/decisions.md").read_text(encoding="utf-8")
        self.assertEqual(cf.decision_status_claims(text), [])


STATUS_OK = """# 当前状态（STATUS）

最后核实：2026-10-24。

## 部署

| 项 | 事实 | 来源 |
|---|---|---|
| 部署版本 | v0.2.0-preview.35 @ `23edeaf` | manifest |

## 服务

| systemd 单元 | 进程 | 入口 | 工具数 | 备注 |
|---|---|---|---|---|
| `x-data-mcp-data` | `data_service` | `run/mcp.sock` | 23 | capability `generic-data-v7` |
| `x-data-mcp-acquire` | `acquire_service` | `run/acquire.sock` | 10 | — |

## 数据

| 项 | 事实 | 来源 |
|---|---|---|
| 热 generation | **5/6**：1 active + 4 superseded；下一次 acquire 可直接受理 | acquire.sqlite |

preview.35 部署后 capability 仍为 `generic-data-v7`，23/10 工具不变。

工具数与能力版本为 live 只读复核：data socket `tools/list` 返回 23 个工具、
acquire socket `tools/list` 返回 10 个工具。两个 socket 的 `serverInfo.version` 均为 `0.2.0-preview.35`。
"""


def status_claims(text: str) -> list[tuple[int, str, str]]:
    return cf.status_consistency_errors(text, REGION_DATA_TOOLS, REGION_ACQUIRE_TOOLS)


class StatusConsistency(unittest.TestCase):
    """P1-2 (#296) item 3: STATUS must not contradict itself (P1-1, item 7c)."""

    def test_consistent_status_passes(self):
        self.assertEqual(status_claims(STATUS_OK), [])

    def test_hot_count_contradiction_is_flagged(self):
        # P1-1, item 7c: the 容量 row said hot 6/6 while the 数据 table said 5/6.
        text = STATUS_OK + (
            "\n## 新鲜度\n\n| 项 | 事实 | 来源 |\n|---|---|---|\n"
            "| 容量 | hot **6/6**（实测：1 active + 4 superseded） | acquire.sqlite |\n"
        )
        found = status_claims(text)
        self.assertEqual(
            sorted(set(rules(found))),
            ["status-hot-breakdown", "status-hot-count"],
            found,
        )
        self.assertTrue(all(line == 33 for line, _, _ in found), found)

    def test_hot_timeline_is_exempt(self):
        # A transition chain describes how the count changed, not its value.
        text = STATUS_OK + (
            "\n10-24 实测路径：16:56Z 为 6/6（1 active + 5 superseded）→ "
            "17:52Z 再退役后当前 5/6。\n"
        )
        self.assertEqual(status_claims(text), [])

    def test_hot_capacity_contradiction_is_flagged(self):
        text = STATUS_OK + "\n当前 热 generation 上限 5/8，超限后拒绝受理。\n"
        self.assertEqual(rules(status_claims(text)), ["status-hot-capacity"])

    def test_hot_projection_is_exempt(self):
        text = STATUS_OK + "\n当前 5/6，activate 后由自动保留回收到约 4/6。\n"
        self.assertEqual(status_claims(text), [])

    def test_missing_hot_row_is_flagged(self):
        text = STATUS_OK.replace("| 热 generation | **5/6**", "| 代 | **5/6**")
        self.assertEqual(rules(status_claims(text)), ["status-hot-row-missing"])

    def test_deployed_version_contradiction_is_flagged(self):
        text = STATUS_OK.replace(
            "`serverInfo.version` 均为 `0.2.0-preview.35`",
            "`serverInfo.version` 均为 `0.2.0-preview.34`",
        )
        self.assertEqual(rules(status_claims(text)), ["status-deployed-version"])

    def test_service_table_tool_count_is_checked(self):
        text = STATUS_OK.replace(
            "| `run/mcp.sock` | 23 |", "| `run/mcp.sock` | 21 |"
        )
        self.assertEqual(rules(status_claims(text)), ["status-service-tool-count"])

    def test_prose_tool_count_is_checked(self):
        text = STATUS_OK.replace(
            "data socket `tools/list` 返回 23 个工具",
            "data socket `tools/list` 返回 21 个工具",
        )
        self.assertEqual(rules(status_claims(text)), ["status-tool-count"])

    def test_tool_pair_is_checked(self):
        text = STATUS_OK + "\npreview.34 与 preview.35 都是二进制热切换，21/10 工具不变。\n"
        self.assertEqual(rules(status_claims(text)), ["status-tool-pair"])

    def test_repository_status_passes(self):
        text = (cf.ROOT / "docs/STATUS.md").read_text(encoding="utf-8")
        self.assertEqual(status_claims(text), [])


class CurrentRegionsCoverage(unittest.TestCase):
    """Every configured region must exist, so a renamed heading fails loudly."""

    def test_every_configured_region_is_found(self):
        for rel, headings in cf.CURRENT_REGIONS.items():
            text = (cf.ROOT / rel).read_text(encoding="utf-8")
            for heading in headings:
                self.assertTrue(cf.top_current_region(text, heading), f"{rel}: {heading}")

    def test_design_md_has_both_p1_1_regions(self):
        self.assertEqual(
            cf.CURRENT_REGIONS["docs/design.md"],
            ("## 存储模型状态（版本链 vs 单版本）", "## 当前能力与目标差距"),
        )

    def test_current_regions_and_docs_do_not_overlap(self):
        self.assertEqual(set(cf.CURRENT_REGIONS) & set(cf.CURRENT_DOCS), set())


if __name__ == "__main__":
    unittest.main()
