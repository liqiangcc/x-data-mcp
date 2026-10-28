"""Unittest coverage for scripts/check_deploy_facts.py (TASK-XP-026 S5)."""
import datetime
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from check_current_facts import CURRENT_DOCS as FACTS_CURRENT_DOCS
from check_deploy_facts import (
    CURRENT_DOCS,
    check_current_host,
    check_deployed_entries,
    check_status_md,
    top_current_region,
)

TODAY = datetime.date(2026, 10, 10)


REAL_STATUS_MD = r"""# 当前状态（STATUS）

本文件是**部署与运行事实**的唯一入口。README、AGENTS.md、`docs/daily-sync.md`
等文档只链接本文件，不再复述版本、主机、generation 或数据量。性能与容量目标见
[SLO](SLO.md)。

每次部署、迁移或 generation 激活后，执行者在同一个 PR 内更新本文件。

最后核实：2026-10-10。标注「来源」的数值来自所列报告或命令，本次未重新读取 live 服务。

## 部署

| 项 | 事实 | 来源 |
|---|---|---|
| 主机 | aws-node | 运维记录 |
| 系统 | Ubuntu 26.04 LTS，kernel 7.0.0-1014-aws | `/etc/os-release`、`uname -r` |
| 资源 | 2 vCPU（1 个物理核加 2 个超线程，没有 SHA-NI），1.9GiB RAM + 8GiB swap，根盘 77G（已用 34%） | `lscpu`、`/proc/cpuinfo`、`free -h`、`df -h` |
| 本机工具链 | rustc / cargo 1.99.0 | `rustc --version` |
| 部署版本 | v0.2.0-preview.30 @ `4ff45d748caef5fffda7360c2c4bef070a7c3189` | `/opt/x-data-mcp-preview/current/manifest.json` |
| 构建 | rustc 1.99.0，ubuntu-24.04，最低 glibc 2.39 | 同上 |
| 部署目录 | `/opt/x-data-mcp-preview/current` → `releases/4ff45d7…` | `ls -l` |
| 迁移 | 2026-10-09 从 ecs-node 迁到 aws-node；全新部署后重新采集，未迁移 DB | `/opt/x-data-mcp-preview/reports/migration-to-aws-20261009.json` |
| ecs-node | 服务已 stop 并 disable，数据保留为冷备（含 generation `a49c314b…` 的字节） | 同上 |
| production_ready | `false`：owner 个人研究、本地使用，未授权对外分发（#71） | `get_data_capabilities` |

## 服务

| systemd 单元 | 进程 | 入口 | 工具数 | 备注 |
|---|---|---|---|---|
| `x-data-mcp-data` | `data_service` | `run/mcp.sock` | 20 | capability `generic-data-v3`；drop-in 设置 `X_DATA_MCP_GENERATION_ROOT` |
| `x-data-mcp-acquire` | `acquire_service` | `run/acquire.sock` | 10 | 设置了 `X_DATA_MCP_TDX_FETCH=current/scripts/tdx_fetch.py` |
| `x-data-mcp-tunnel` | OpenAI `tunnel-client`（profile `x-data-mcp`） | 转发到 `run/mcp.sock` | — | 只转发 data server，不转发 acquire |

访问边界与认证细节属于运维信息，不记录在公开仓库。当前边界是否可接受，尚待 owner 明确决定。

## 数据

| 项 | 事实 | 来源 |
|---|---|---|
| 当前 generation | `87322997b21ee492224b42984c7fa5df865840d26f04bee628e54ce74a0e0de6`，2026-10-08T09:06:02Z 激活 | `generations/active-generation.json` |
| 前一 generation | `a49c314b8f910aa68d0a66b9f3b3d08bf0d57f5b215a8b77a973786391e512a1`（字节只在 ecs 冷备中） | 同上、迁移报告 |
| 数据库 | `state/preview.sqlite`，fresh compact schema 20，810,266,624 B（2026-10-10 04:34Z） | `ls -l` |
| 目录 | 5,228 个实体 | 迁移报告 |
| 日线 | 已发布 5,224 个，head 为 2026-10-08 | 迁移报告 |
| 年线 | 5,223 个 succeeded | 迁移报告 |
| 缺失 | `sz.001381`、`sz.301569`、`sz.301660`、`sz.301718`；`sz.001246` 为空 | 迁移报告 |

2026-10-09 之后是否执行过日更：**未核实**。

## 进行中

- **GOAL-008 性能**：分支 `perf/g8-daily-update-perf`，PR #323，**未合并**。
  - 进度查询小门已通过：已完成 100 条任务的 `get_task_progress` 中位 0.108s / 0.091s。
  - 新只读工具 `get_yearly_base_years`（capability `generic-data-v4`）的实现提交 `ef7ea71` CI 已通过，待审查。
  - 排期：构建与 SQLite 调优 → 受理体检有界化 → 年线服务端提速 → 全市场验收。门禁见 [SLO](SLO.md)。
  - 生产 preview.30 **不包含**上述改动。
- **#324**：stopped-write 核实会重写 `outcome_json`，破坏 compact result 的完整性链接。合并 #323 之前必须解决。
- **#304**：连续 10 个交易日仅用 Skill 完成同步的验收。已记录的 PASS 运行（Run 3、Run 5、2026-10-08）全部是 `day=unchanged`，有更新那天的路径尚未覆盖。有效计数以 #304 的评论为准；2026-10-08 那次使用的是提示词 `daily-ai-sync-v2`。

## 已知问题

| 问题 | 影响 | 来源 |
|---|---|---|
| #324 | 写路径的数据一致性缺陷 | Issue |
| 受理体检每次扫描全部历史 task、item、request，还会重算所有历史请求的 SHA-256（`src/sqlite_tasks.rs` 中的 `accept_tx`） | 受理耗时随历史线性增长；目前每次部署都全新 init 库，把增长掩盖了 | 源码；私有栈实测受理从 0.05s 增长到 1.8s/chunk |
| aws `acquire.sqlite` 中有 4 个没有字节的 generation，normalized-retire 待处理 | 采集台账有残留 | 迁移报告 |
| `/opt/x-data-mcp-preview/mcp.json` 的 `x-data-mcp-demo` 指向一个不存在的 sqlite | 配置过时 | `ls` |
| `src/acquire/fetch.rs` 在未设置 `X_DATA_MCP_TDX_FETCH` 时，回退到编译期的 `CARGO_MANIFEST_DIR` | 不带该变量启动的实例无法下载 | 源码 |
| 生产服务、私有性能栈和多个 agent 会话共用 1 个物理核和 1.9GiB 内存 | 2026-10-10 约 04:34Z 过载重启；测量结果有噪声 | 运维观察 |
| AGENTS.md 的环境段、README「当前状态」、`docs/daily-sync.md`「当前状态」仍写 ecs-node / preview.29 / generation `2e9183…` | 读取这些文档的 agent 会被误导 | 本次核对；这三处已加指向本文件的说明 |

## 维护规则

1. **谁更新**：部署或迁移的执行者，在记录本次部署的同一个 PR 内更新。
2. **其他文档**：README 只保留「项目是什么」并链接本文件；AGENTS.md 的环境段只写开发约束，主机事实链接本文件。旧段落的清理由后续 PR 完成。
3. **待办：扩展 CI 检查。** `scripts/check_current_facts.py` 目前只检查包版本、schema、工具数量这类构建事实，所以文档写错主机或部署版本时也能通过。建议增加部署事实的检查：
   - 本文件中的部署版本必须与最近一份已提交的部署记录一致；
   - `CURRENT_DOCS` 的当前区域不得把已退役主机写成当前主机；
   - 「最后核实」日期距今超过 N 天时给出警告。
"""


def status_file(tmp: Path, text: str) -> Path:
    path = tmp / "STATUS.md"
    path.write_text(text, encoding="utf-8")
    return path


class StatusMd(unittest.TestCase):
    def test_missing_file_warns_not_fails(self):
        errors, warnings = check_status_md(Path("/nonexistent/STATUS.md"), TODAY)
        self.assertEqual(errors, [])
        self.assertEqual(len(warnings), 1)

    def test_present_without_date_fails(self):
        with tempfile.TemporaryDirectory() as td:
            path = status_file(
                Path(td), "# status\ndeployment version: preview.29\n"
            )
            errors, _ = check_status_md(path, TODAY)
            self.assertTrue(any("no '最后核实'" in e or "no '" in e for e in errors))

    def test_fresh_date_passes(self):
        with tempfile.TemporaryDirectory() as td:
            path = status_file(
                Path(td),
                "# status\n最后核实: 2026-10-09\ndeployment version: preview.29\n",
            )
            errors, warnings = check_status_md(path, TODAY)
            self.assertEqual(errors, [])
            self.assertEqual(warnings, [])

    def test_stale_date_warns(self):
        with tempfile.TemporaryDirectory() as td:
            path = status_file(
                Path(td),
                "last verified 2026-09-01\ndeployment version: v0.2.0-preview.29\n",
            )
            errors, warnings = check_status_md(path, TODAY)
            self.assertEqual(errors, [])
            self.assertTrue(any("days old" in w for w in warnings))

    def test_future_date_fails(self):
        with tempfile.TemporaryDirectory() as td:
            path = status_file(
                Path(td), "最后核实 2027-01-01\nversion: preview.29\n"
            )
            errors, _ = check_status_md(path, TODAY)
            self.assertTrue(any("future" in e for e in errors))

    def test_malformed_version_fails(self):
        with tempfile.TemporaryDirectory() as td:
            path = status_file(
                Path(td),
                "最后核实 2026-10-09\ndeployment version: preview.29-rc!!\n",
            )
            errors, _ = check_status_md(path, TODAY)
            self.assertTrue(any("malformed" in e for e in errors))

    def test_no_version_fails(self):
        with tempfile.TemporaryDirectory() as td:
            path = status_file(Path(td), "最后核实 2026-10-09\n")
            errors, _ = check_status_md(path, TODAY)
            self.assertTrue(any("no deployment version" in e for e in errors))

    def test_real_status_md_from_main_passes(self):
        """origin/main docs/STATUS.md (PR #325) must yield zero errors —
        guards the trailing-punctuation date and markdown-table version."""
        with tempfile.TemporaryDirectory() as td:
            path = status_file(Path(td), REAL_STATUS_MD)
            errors, _ = check_status_md(path, datetime.date(2026, 10, 10))
            self.assertEqual(errors, [])

    def test_table_cell_version_detected(self):
        with tempfile.TemporaryDirectory() as td:
            path = status_file(
                Path(td),
                "最后核实：2026-10-10。\n"
                "| 项 | 事实 | 来源 |\n"
                "| 部署版本 | v0.2.0-preview.30 @ `4ff45d7` | manifest |\n",
            )
            errors, _ = check_status_md(path, datetime.date(2026, 10, 10))
            self.assertEqual(errors, [])


SERVICE_TABLE = """## 服务

| systemd 单元 | 进程 | 入口 | 工具数 | 备注 |
|---|---|---|---|---|
| `x-data-mcp-data` | `data_service` | `run/mcp.sock`（`--entry ops`） | 15 | capability `generic-data-v8` |
| `x-data-mcp-acquire` | `acquire_service` | `run/acquire.sock` | 10 | — |
"""

COUNTS = {"research": 5, "ops": 15, "maint": 9}


class DeployedEntries(unittest.TestCase):
    """v8 phase 2: the 服务 table states the deployed entries and their counts."""

    def test_single_ops_row_passes(self):
        self.assertEqual(check_deployed_entries(SERVICE_TABLE, COUNTS), [])

    def test_row_without_entry_means_the_implicit_ops_socket(self):
        text = SERVICE_TABLE.replace("`run/mcp.sock`（`--entry ops`）", "`run/mcp.sock`")
        self.assertEqual(check_deployed_entries(text, COUNTS), [])

    def test_count_must_match_that_entrys_allowlist(self):
        text = SERVICE_TABLE.replace("| 15 |", "| 21 |")
        errors = check_deployed_entries(text, COUNTS)
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("allowlist: 15", errors[0])

    def test_entry_outside_the_deployed_set_is_flagged(self):
        text = SERVICE_TABLE.replace(
            "`run/mcp.sock`（`--entry ops`）",
            "`run/mcp.sock`（`--entry ops`）、`run/research.sock`"
            "（`--entry research`）",
        ).replace("| 15 |", "| 15 / 5 |")
        errors = check_deployed_entries(text, COUNTS)
        self.assertTrue(any("not deployed" in e and "research" in e for e in errors), errors)

    def test_deployed_entry_missing_from_the_table_is_flagged(self):
        # The only data row declares research: the deployed ops entry is gone.
        text = SERVICE_TABLE.replace(
            "`run/mcp.sock`（`--entry ops`）", "`run/research.sock`（`--entry research`）"
        )
        errors = check_deployed_entries(text, COUNTS)
        self.assertTrue(any("omits deployed data entry ops" in e for e in errors), errors)
        self.assertTrue(any("not deployed" in e for e in errors), errors)

    def test_no_data_row_at_all_is_an_error(self):
        text = SERVICE_TABLE.replace("`x-data-mcp-data`", "`x-data-mcp-legacy`")
        errors = check_deployed_entries(text, COUNTS)
        self.assertTrue(any("no x-data-mcp-data row" in e for e in errors), errors)

    def test_multi_entry_row_passes_once_the_deploy_day_constant_flips(self):
        text = SERVICE_TABLE.replace(
            "`run/mcp.sock`（`--entry ops`）",
            "`run/mcp.sock`（`--entry ops`）、`run/research.sock`"
            "（`--entry research`）、`run/maint.sock`（`--entry maint`）",
        ).replace("| 15 |", "| 15 / 5 / 9 |")
        # Still the pre-Ops-B deployment: the extra entries are not deployed.
        self.assertTrue(check_deployed_entries(text, COUNTS))
        with mock.patch(
            "check_deploy_facts.DEPLOYED_DATA_ENTRIES",
            ("ops", "research", "maint"),
        ):
            self.assertEqual(check_deployed_entries(text, COUNTS), [])

    def test_repository_status_passes(self):
        text = (Path(__file__).resolve().parent.parent / "docs/STATUS.md").read_text(
            encoding="utf-8"
        )
        self.assertEqual(check_deployed_entries(text), [])


class CurrentHost(unittest.TestCase):
    def test_ecs_node_as_current_host_rejected(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            (root / "README.md").write_text(
                "intro\n## 当前状态\ncurrent host: ecs-node\n## next\n", encoding="utf-8"
            )
            errors = check_current_host(root)
            self.assertTrue(any("README.md" in e for e in errors))

    def test_ecs_node_as_measurement_env_allowed(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            (root / "README.md").write_text(
                "## 当前状态\n测量环境 ecs-node x86_64、2 vCPU\n", encoding="utf-8"
            )
            self.assertEqual(check_current_host(root), [])

    def test_ecs_node_outside_current_region_allowed(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            (root / "README.md").write_text(
                "## 当前状态\nhost: web\n## 历史\ncurrent host: ecs-node\n",
                encoding="utf-8",
            )
            self.assertEqual(check_current_host(root), [])


class Regions(unittest.TestCase):
    def test_region_stops_at_next_heading(self):
        region = top_current_region("a\n## 当前状态\nmid\n## end\nafter", "## 当前状态")
        self.assertIn("mid", region)
        self.assertNotIn("after", region)

    def test_none_heading_uses_prefix(self):
        self.assertEqual(top_current_region("pre\n## h\nrest", None), "pre\n")

    def test_current_docs_come_from_check_current_facts(self):
        # P1-2 (#296): one region set for both linters, so they cannot drift.
        self.assertIs(CURRENT_DOCS, FACTS_CURRENT_DOCS)


if __name__ == "__main__":
    unittest.main()
