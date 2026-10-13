# generic-data-v6 / schema 21 部署日切换清单

本文列出**部署日**要做的机械切换：把各文档「当前状态」从
`generic-data-v5` / compact schema 20 改成 `generic-data-v6` /
compact schema 21。**现在不要改这些文件**——已部署实例仍是
preview.31 / schema 20 / `generic-data-v5`（[STATUS](../STATUS.md)），
提前改会让 `scripts/check_current_facts.py` 失败，且两个 Skill 被
生产日更使用。

位置一律用**锚点**（原文句子、区域标题、表行名）标注，不用行号：
这些文档的「当前部署」句在并行 PR 中正被对齐到 preview.31 /
21 tools / v5，行号会漂移。执行前以当日文件为准。

## 0. 顺序约束

1. **S4 提交内（capability bump，部署日之前，属主线任务）**：
   `src/data_supply/mcp.rs` 中 `capability_revision:
   "generic-data-v5".into()`（函数 `capabilities()` 返回体）改
   `"generic-data-v6"`；同一提交必须同步：
   - `scripts/check_current_facts.py` 常量
     `EXPECTED_DATA_CAPABILITY_REVISION = "generic-data-v5"` → `"v6"`；
   - `scripts/smoke_release.py` 两处断言
     `cap["capability_revision"] == "generic-data-v5"` 与
     `capabilities["capability_revision"] == "generic-data-v5"` → `"v6"`
     （smoke 对发布包断言，漏改则 release 冒烟必挂）；
   - 两个 Skill 的 revision 断言（§3），前提是接受「bump 提交合入
     到部署完成之间，strict Skill 对仍是 v5 的生产实例报
     `SKILL_CONTRACT_MISMATCH` 并停止」这一预期 fail-closed 窗口。
     headless 日更读取的 SKILL.md 已核实：`run_daily_sync_eval.sh`
     先 `cd` 到仓库根（`git rev-parse --show-toplevel`）再起 devin，
     读的是该工作树里的 `.devin/skills/x-data-daily-sync/SKILL.md`
     ——即日更前 Owner 工作树必须已更新到包含本切换 PR 的 main
     提交。
   - `scripts/deploy_phase_g.sh` 的 schema-20 断言与
     `task_sqlite init20` 的命名/打印（OWNER-RULING-PAR-1A R4：
     **必须进 S4 与 bump 同一提交**，`init20` 保留别名、输出 21，
     并由 CI 的 switch-binary/deploy facts 检查覆盖——否则部署日
     脚本自断言必炸）。§1 仅为核对备查。
   - registry 中 `verify_publication_result`、`republish_data` 等
     description 的 v6 措辞（S4 范围，本文不列句）。
2. **本清单§2–§4（部署日）**：facts 常量、文档「当前状态」区域、
   STATUS 更新。文档区改法与 `check_current_facts.py` 的
   `REQUIRED_FACTS` 正则必须在同一提交翻转（docs 写 "schema 21"
   ⇄ 正则期望 21），否则 facts 门必挂。
3. **release 版本号**：`scripts/check_current_facts.py` 常量
   `EXPECTED_PACKAGE_VERSION` 随部署 tag 一并 bump（惯例，本次
   未核实目标 tag）。

## 1. 脚本断言（S4 同提交改，部署日核对）

OWNER-RULING-PAR-1A R4：本节改动**必须在 S4 与 capability bump
同一提交合入**（部署日再改已来不及）。`task_sqlite init20` 保留
子命令别名、输出 21。

### `scripts/deploy_phase_g.sh`
| 锚点 | 旧 | 新 |
|---|---|---|
| 文件头注释 `Fresh schema-20 install semantics` | schema-20 措辞 | schema-21（注释同步） |
| 部署步骤注释 `schema=20 + tools smoke` | `schema=20` | `schema=21`（以实际行内容为准） |
| `step=schema20_fresh` | step 名含 `schema20` | `step=schema21_fresh`（日志标签，可同步改） |
| `[[ "$init_out" == *"task schema 20 ready"* ]]` | 期望 `task schema 20 ready` | 期望 `task schema 21 ready`——`task_sqlite init20` 在 v21 代码上打印 `task schema 21 ready`（`src/bin/task_sqlite.rs` init20 分支打印 `COMPACT_SCHEMA_VERSION`，现为 21）；不改则部署脚本自断言失败 |
| `[[ "$fresh_schema" == "20" ]]` | `"20"` | `"21"` |

### `scripts/check_current_facts.py`
| 锚点 | 旧 | 新 |
|---|---|---|
| `EXPECTED_COMPACT_SCHEMA` 上方注释 | 「DEPLOYED preview instance remains on schema 20 … doc-fact regexes below stay keyed to 20」 | 部署完成后改为描述 schema 21 已部署 |
| `EXPECTED_COMPACT_SCHEMA = 21` | 21 | 不变（fresh init 常量已是 21） |
| `EXPECTED_DATA_CAPABILITY_REVISION` | `"generic-data-v5"` | `"generic-data-v6"`（**与 capability bump 同一提交**，见 §0.1） |
| `REQUIRED_FACTS` 第一项 `("schema 20", re.compile(r"schema[\s:=v]*20(?!\d)"…` | 正则期望 20 | `("schema 21", re.compile(r"schema[\s:=v]*21(?!\d)"…`——与 §2 文档翻转同提交 |
| `EXPECTED_PACKAGE_VERSION` | `"0.2.0-preview.31"` | 部署 tag 对应版本 |

### `scripts/check_deploy_facts.py`
无 schema/capability 常量要改；它只校验 STATUS.md 的「最后核实」
日期、版本串格式和 ecs-node 不作当前主机。部署日只要按 §4 正确
更新 STATUS.md 即可，无需动脚本。`scripts/test_deploy_facts.py`
夹具文本含 "schema 20"，是测试数据不是断言——**不改**。

## 2. 文档「当前状态」区域（`CURRENT_DOCS` 覆盖的 5 个文件）

`check_current_facts.py` 要求下列区域同时出现 "schema 20"→（改后）
"schema 21"、"21 data tools"、"10 acquisition tools" 三个事实。
部署版本号/主机等以 STATUS.md 为准的句子按当日事实填写，不只改
版本号。另有一并行 PR 正把这五个文档的部署句对齐到
preview.31 / 21 tools / v5——部署日时若该 PR 已合入，「旧句」可能
已写成 preview.31/v5 形态，按下述「目标语义」改即可。

### `README.md`（区域锚点 `## 当前状态`）
- 锚点句「`task_sqlite init` 的新库使用 fresh compact schema 20」
  → `schema 21`。
- 锚点：「当前开发分支…宣告 `generic-data-v5`」至「`generic-data-v3`」
  整句 → 按部署后事实重写为「当前部署宣告 `generic-data-v6`，
  21 data tools」口径（preview.29/v3 句为历史，按 STATUS.md 重写）。
- 锚点句「`COMPACT_SCHEMA_VERSION=20` 是 fresh 部署的 compact
  schema」→ `=21`。
- `preview.27/schema20 实测…` 历史实测记录——**不改**。

### `docs/daily-sync.md`（区域锚点 `## 当前状态`）
- 锚点句「显式 init 的新库使用 fresh compact schema 20」→ `schema 21`。
- 锚点：「当前开发分支…宣告 `generic-data-v5`」至「`generic-data-v3`」
  整句 → 按部署后事实重写。
- 「preview.29 Phase-G 部署为 fresh-init schema 20」为部署史——
  **不改**；v6 部署记录按 STATUS.md 口径追加。
- 验收表内 `preview.27/schema20` 行为历史实测——**不改**。

### `docs/mcp-tools.md`（区域锚点 `## 版本与阅读方式`）
- 锚点句「fresh init 库使用 compact schema 20」→ `schema 21`。
- 锚点：「当前开发分支…宣告 `generic-data-v5`」至「`generic-data-v3`」
  整句 → 按部署后事实重写。

### `docs/generic-data-tools.md`（区域锚点 `## 决策与适用范围`）
- 锚点句「fresh init 库为 compact schema 20」→ `schema 21`。
- 锚点：「当前开发分支…宣告 `generic-data-v5`」至「`generic-data-v3`」
  整句 → 按部署后事实重写。
- 「`related_tasks` … `generic-data-v5` 起…」为契约演变记录，保留
  v5 字样合理；S4 重写该段时可顺手标注「v6 仍沿用」。

### `docs/source-closure.md`（区域 = 文件开头到第一个 `##`，
`CURRENT_DOCS` 中 heading=None）
- 锚点句「fresh init 库使用 compact schema 20」→ `schema 21`。
- 锚点：「当前开发分支…宣告 `generic-data-v5`」至「`generic-data-v3`」
  整句 → 按部署后事实重写。

## 3. Skill 文件（旧句 → 新句）

### `.devin/skills/x-data-daily-sync/SKILL.md`
- 锚点句 `` `capability_revision` 必须等于 `generic-data-v5` `` →
  `` `capability_revision` 必须等于 `generic-data-v6` ``。
- 紧随其后括号注「（v5 起 `get_data_availability` 的 `related_tasks`
  只含在途任务与最近 5 个已结束任务…不得静默适配。）」→ 替换为
  v6 说明，建议：「（v6 起 market 数据为单版本序列：`version_id`
  为内容 hex，`list_data_versions` 每实体至多一项；显式 pin 读取
  被取代时报 `stale_version`，写路径 stale base 为 item 级
  `needs_verification`，均须重取当前 head 重试，不得静默适配；
  v5 的 `related_tasks` 口径沿用。）」——stale 行级字段名以 S3
  定稿为准，TODO(S3)。
- 锚点句「不得从 `data_versions=0` 自动推断重建授权」→ v6 无
  `data_versions` 表，建议改为「不得从 `list_data_versions` 返回
  空列表自动推断重建授权」。
- `base_version_id:<head version_id>` 语义不变（v6 下即当前 head
  的内容 hex）；可在该句补「被取代的 base 受理为 item 级
  `needs_verification`（`STALE_BASE:` 前缀），重取 head 后重提」
  （OWNER-RULING-PAR-1A R1 已定该档）。
- 年线编排段（「可选：派生 `market.yearly`」段）：
  `get_yearly_base_years`/`expected_yearly_head` 用法字段名不变；
  stale 标记（behind/invalid）的判读句子待 S3 出参定稿后补，
  TODO(S3)。

### `.devin/skills/x-data-status/SKILL.md`
- 锚点句「`x-data-acquire-v1` / `official-generation-v1` /
  `generic-data-v5`；不匹配 → 停止并报告 `SKILL_CONTRACT_MISMATCH`」
  → 仅把 `generic-data-v5` 改为 `generic-data-v6`。

## 4. `docs/STATUS.md`（部署执行者同 PR 更新，维护规则见文件末节）

- 「最后核实」日期 → 部署日。
- 「部署」表「部署版本」「部署时间」行 → 新 tag、新归档库名
  （`preview.sqlite.v20-archived-<部署时间>` 形态沿用，archive 名
  里的 `v20` 是**被归档库的** schema，保持事实即可）。
- 「服务」表 `x-data-mcp-data` 行 `capability generic-data-v5` →
  `generic-data-v6`。
- 「数据」表「数据库」行 `fresh compact schema 20` → `schema 21`，
  字节数按 `ls -l` 实测。
- 「已知问题」表建议新增：`market_events` 事件日志无界增长
  （RULINGS Q9 已接受，估计 ~5,224 行/日 ≈ 1MB/日），与任务簿记
  增长（既有行，10–16GB/年）分开记录。
- 「已知问题」XP-043 sweep 行可补一句 v21 启动检查为逐 series
  索引点查（S2b F3 已修 loose-scan），不改也可。

## 5. 顺带发现（不在 `CURRENT_DOCS` 门禁内，建议同批处理）

- `docs/design.md`：表格行「| 行情 | SQLite fresh compact schema
  20、…」→ `schema 21`。
- `AGENTS.md`：部署管线段「全新 init 最新 compact schema（现 v20）」
  → `现 v21`；preview.29 runtime 段为历史记录不改（STATUS.md
  维护规则指明 AGENTS 环境段由后续 PR 清理，本项可与之一并）。
- `docs/tasks/`、`docs/contracts/mcp-tools-current.json`、
  `docs/contracts/generic-start-collection-v1.json`：历史快照/任务
  档案——**不改**。
- `docs/SLO.md`：无 v5/schema-20 当前事实要改；schema20 字样都在
  历史实测记录内——**不改**。S5 验收后新增 schema-21 实测行属
  正常演进。

## 6. 部署日验证（改完跑）

```bash
python3 scripts/check_current_facts.py
python3 scripts/check_deploy_facts.py
git diff --check
# 公开仓库卫生门（scripts/check_public_hygiene.py）在 ci/public-hygiene
# 分支，本分支尚未包含；若已合入则一并运行。
```
