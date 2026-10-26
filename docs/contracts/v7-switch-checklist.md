# generic-data-v7 部署日切换清单

**状态**：历史快照。v7 切换已于 2026-10-16 随 preview.33 执行完毕；当前部署事实见
[STATUS](../STATUS.md)。

本文是当时**部署日**要做的机械切换：把各文档「当前状态」与两个 Skill 从
`generic-data-v6` / 21 data tools 改成 `generic-data-v7` / 23 data tools。
当时要求**不要提前改 Skill 与「当前状态」区域**——写作时的已部署实例
是 preview.32 / schema 21 / `generic-data-v6`（历史，[STATUS](../STATUS.md)），
提前改会让 `scripts/check_current_facts.py` 失败，且两个 Skill 被生产日更使用。

与 v6 切换的关键差别：**v7 不升 schema**（仍 21）、不加表，部署只需
**替换二进制**——不用 `deploy_phase_g.sh` 的归档+全新 init 流程，用
`scripts/switch_binary.sh <v0.2.0-preview.N> <S:40-hex>`（无 DB 迁移；
`--execute` 前确认 PLAN-H2 P1–P4 前置事实）。

位置一律用**锚点**（原文句子、区域标题、表行名）标注，不用行号。

## 0. 顺序约束

1. **实现阶段已完成（本 PR 系列，部署日之前）**：
   - `src/data_supply/mcp.rs` `capability_revision` 已改
     `"generic-data-v7"`；`EXPECTED_DATA_CAPABILITY_REVISION`（代码常量）
     已为 v7、`EXPECTED_DATA_TOOLS` 已为 23；`scripts/smoke_release.py`
     断言已为 23 tools / v7——这些**同 capability bump 同提交**（同
     OWNER-RULING-PAR-1A R4 的模式）。
   - 仍为已部署事实的 `EXPECTED_SKILL_CAPABILITY_REVISION`（v6）与
     `EXPECTED_DEPLOYED_DATA_TOOLS`（21）保持不动，**本清单部署日翻
     转**（见 §2）。
2. **本清单（部署日）**：Skill 修订断言翻转 + 文档「当前状态」区域 +
   STATUS.md + `EXPECTED_SKILL_CAPABILITY_REVISION` /
   `EXPECTED_DEPLOYED_DATA_TOOLS` 常量翻转——同一提交。
3. **release 版本号**：`EXPECTED_PACKAGE_VERSION` 随部署 tag bump。

## 1. 部署步骤（二进制热切换，无归档重建）

`scripts/switch_binary.sh`（dry-run 默认，`--execute` 实跑）已兼容
v7：schema 不变，无 `init`/`init20` 步骤，不碰 DB/WAL/generations，
只换 `current` 符号链接。部署日执行：

1. 前置核实（MCP 只读）：无在途 acquisition、无非终态 data task、
   记录 catalog/daily/yearly head 计数、`sync_status` 基线。
2. `gh release` 产物的 smoke-report 断言由脚本自校验（smoke 对包内
   断言已是 23 tools / v7——实现阶段已改）。
3. 执行 `switch_binary.sh <新 tag> <合并 sha> --execute`；
   S6 等 socket 恢复；S7 人工核对。
4. S7 核对改为：`capability_revision == generic-data-v7`、
   tools 23/10、`get_data_capabilities` 中 `plan_market_update` /
   `run_market_update` `implemented:true`、head 计数与基线一致。
   （`switch_binary.sh` S7 的提示行还写着 v3/20 的旧文案——属陈旧
   echo，实现阶段已在本清单记录；部署日以本节口径为准，或顺手在
   本提交修正脚本 echo。）
5. 回滚：symlink 切回即恢复（无 DB 变更，无需库还原）。

## 2. 脚本常量与 Skill（部署日同一提交翻转）

### `scripts/check_current_facts.py`
| 锚点 | 旧 | 新 |
|---|---|---|
| `EXPECTED_SKILL_CAPABILITY_REVISION` | `"generic-data-v6"` | `"generic-data-v7"`——驱动 Skill 文本检查与文档当前区 stale 声明检查 |
| `EXPECTED_DEPLOYED_DATA_TOOLS` | `21` | `23`——当前区要求的工具数声明随部署翻转 |
| `REQUIRED_FACTS` 的 `21 data tools` 正则 | `21` | `23`（与文档翻转同提交） |
| `EXPECTED_PACKAGE_VERSION` | 当前 preview 版本 | 部署 tag 对应版本 |
| `EXPECTED_DATA_CAPABILITY_REVISION` / `EXPECTED_DATA_TOOLS` | v7 / 23 | 不变（代码常量已就位） |

### `.devin/skills/x-data-daily-sync/SKILL.md`
- `capability_revision` 断言 `generic-data-v6` → `generic-data-v7`；
- 日更编排改为 plan/run 顺序，直接引用 `generic-data-v7.md` §7.1 的有序步骤
  （共 8 步，**两步补漏不可省略**）：acquire→verify→activate 之后，
  `plan_market_update`（market.daily）→ 复核 counts/digest →
  `run_market_update`（带 plan 返回的 `target_day`+`plan_id`）→
  轮询 `get_task_progress` 至终态（大任务按 `unfinished` keyset 分页，
  只看 `status`/计数/`next_cursor`，不要期待逐 item 结果数组）→
  **按 `failed_by_reason` 补漏日线 `REVISION_UNVERIFIED`**（§7.2 显式
  `update_data` 前向窗口）→ `plan_market_update`
  （market.yearly，要求 daily 无非终态任务）→
  `run_market_update`（market.yearly，只处理 `eligible`）→
  **按 `ineligible_reasons` 补漏年线 `value_semantics_changed` /
  `daily_replaced_since_derivation`**（§7.4 显式年线重推；**必须排在年线 run 之后**，
  否则 run 的 `plan_id` 会 `plan_stale`）→ readback。`plan_stale` → 重新 plan；
  不再按 ≤100 分批显式编排；
- `.devin/evals/daily-sync-config.json` allowlist 加
  `mcp__x-data__plan_market_update`、`mcp__x-data__run_market_update`
  （`check_daily_sync_eval.py` 要求 eval allowlist == Skill allowlist，
  同提交）。

#### `REVISION_UNVERIFIED` 失败集的路由（Skill 编排必须包含）
- 何时出现：上游对已发布历史重算（除权/复权变化）。plan/run 的刷新窗口
  是整段历史，窗口内已发布 bar 被改写又没有修订证据，所以对这些实体
  fail-closed，任务终态为 `partially_failed`。生产里同一批实体由
  `update_data` 显式前向窗口走重写分支（`replace`，continuity 3）完成。
  10-13 当天约 21 个，跨多个交易日的追赶会更多。
- 怎么取清单：`get_task_progress` 的 `plan.failed_by_reason` 给计数；
  `unfinished` 分页（`page_size`≤100）里 `reason_code=REVISION_UNVERIFIED`
  的项即实体（`entity` 字段）。
- 怎么补：对这些实体用 `update_data` 显式区间
  `[该实体 last_record+1, target_day]`，每任务 ≤100 项（既有显式契约
  上限），然后照常 readback。**不要**对它们 `retry_task`（确定性复败）。
- 其余 `reason_code` 的动作见 `generic-data-v7.md` §5。

### `.devin/skills/x-data-status/SKILL.md`
- `generic-data-v6` → `generic-data-v7`。

## 3. 文档「当前状态」区域（`CURRENT_DOCS` 覆盖的 5 个文件）

各区域把 `generic-data-v6`→`generic-data-v7`、「21 data tools」→
「23 data tools」（plan/run 两工具列名），schema 21 表述不变：

- `README.md`（`## 当前状态`）
- `docs/daily-sync.md`（`## 当前状态`）
- `docs/mcp-tools.md`（`## 版本与阅读方式`）
- `docs/generic-data-tools.md`（`## 决策与适用范围`）
- `docs/source-closure.md`（文件头到第一个 `##`）

## 4. `docs/STATUS.md`

- 「部署」表部署版本/时间行；「服务」表 `x-data-mcp-data` 行
  capability v6→v7；「数据」表工具数 21→23；数据库行不变（同库
  schema 21，无重建）。

## 5. 部署日验证

```bash
python3 scripts/check_current_facts.py
python3 scripts/check_deploy_facts.py
python3 scripts/check_daily_sync_eval.py
python3 scripts/check_public_hygiene.py
git diff --check
```
