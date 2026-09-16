# Issue #3 yearly 验收证据运行计划（freeze）

**状态：PLAN FROZEN — 先计划后观察。本文件冻结输入、序列、断言与如实记录
项；不含运行产物。** 运行脚本与结果文件在后续执行切片提交，运行行为不得
偏离本文件冻结内容；如需变更先改计划再跑。

## 0. 前置条件（冻结时状态）

- 冻结基线：main `6c395d68012628fcc0a6ecee8a91588048dc001d`。
- 冻结时 `OfficialYearlyVerifier` 未实现：服务级 `update_data`/
  `market.yearly` item 以 `BASE_UNAVAILABLE: SOURCE_NOT_VERIFIED:
  NOT_PRODUCTION_READY; publish blocked; <五条 blocker>` 拒绝
  （#178 验收运行已留痕）。
- **本计划的 daily 发布与三条拒绝路径在当前代码上可如实执行**；yearly
  实跑发布段（§3.S4–S6）只有在 yearly verifier 前置实现落地后才可执行。
  若执行时前置仍未满足，S4–S6 的每个 item 结果按拒绝留痕记录，
  `results.json` 标 `yearly_publish: GATE_REFUSED`，不伪造通过。

## 1. 固定输入

| 输入 | 固定值 |
|---|---|
| 快照根 | `/home/box/acceptance-run-issue3/snapshot/`（retained-inputs 硬链接） |
| manifest | `evidence/issue-3-acceptance-run/snapshot-manifest.json`（sha256 `b8a0d7b9aec1a270549d1e53ba26c0ab04842a23c05d1d1f7e75dd44a9f444d3`） |
| base.zip | sha256 `dcdf6b49898926675698263535021aed7ad92b34be82a1a9e8a2a8982bd68bc6` |
| gbbq.zip | sha256 `daad6290c4f1cd478d7436b1e7077260a3e3e52cefd7eae58e4f24878fa2d0e9` |
| hsjday.zip | sha256 `c82f852a7bc2e52c7e89347a1e0d1878b01ce997946c2745f7af323752270892` |
| 证券 | `sz.000002`、`sh.600006`、`sh.600004`（与 #178 验收一致） |
| 服务入口 | `task_sqlite init` + `mcp_data <db>` JSON-RPC stdio；env `X_DATA_MCP_OFFICIAL_MANIFEST`/`X_DATA_MCP_OFFICIAL_DIR` |
| 调度 | 一次性手工 `run_worker_once`，无守护无重试 |
| 构建 | `cargo build --locked --bins`，rustup stable 1.98.1，记录 `--version` 与 head sha |

## 2. 固定 daily 输入版本

每年度证券的 `base_daily_version_id` 必须取自本运行内 `start_collection`
发布所得 `effective_heads`（不允许引用 #178 库或任何旧版本——幂等/谱系
断言要求 base 与 yearly 同库同事务链）。发布断言沿用 #178 冻结口径：
bar 数 8479/6467/5630，末日 2026-09-14，provenance sha 复核。

## 3. 固定执行序列

- **S1** `task_sqlite init <scratch db>`；`mcp_data` 注入两 env 启动；
  `initialize`。
- **S2** `start_collection` market.daily v1 explicit selection 三证券 →
  `run_worker_once` → 三 item `success:true`；记录 task/item/version/
  publication receipt id。
- **S3** 读取三证券 `effective_heads`（period=day）作为 base。
- **S4** 逐证券 `update_data` market.yearly v1：
  `items:[{entity, base_daily_version_id, years, publication:{mode:
  "current_head", expected_yearly_head:null}}]`；`years` = daily 覆盖的
  全部自然年并含当前未结束年：
  - sz.000002：`1991..=2026`（1991-01-29 起）
  - sh.600006：`1999..=2026`（1999-07-27 起）
  - sh.600004：`2003..=2026`（2003-04-28 起）
- **S5** `run_worker_once` 至三 item 终态；记录
  `yearly_execution_results`/`yearly_bars`/`yearly_lineage`/
  `data_versions(period='year')` 全行。
- **S6** `query_data` market.yearly 读回：每证券首年、末年、2025、
  未结束年 2026 四条；`version:{kind:"latest_per_entity"}`，
  `filter:{kind:"yearly",start_year,end_year}`。
- **S7** no-change 幂等：对 sz.000002 重发完全相同的 `update_data`
  （同 request_id → `reused`；不同 request_id 同 frozen inputs →
  `no_change=true` 或不产生新有效版本），逐字留痕 `yearly_execution_results
  .no_change` 与 `effective_heads` 未变。
- **S8** 三条拒绝路径（各自独立 scratch DB/任务，verbatim 留痕）：
  - **env-absent**：不注入两 env 起服务 → 同 S4 请求 → item 拒绝原文；
  - **base-unavailable**：`base_daily_version_id` 指向不存在/非 published
    版本（固定用 `"collection-version-" + 64×"0"`）→ 拒绝原文；
  - **coverage-不足**：以 sh.600004 请求 `years:[1990]`（daily 始于 2003，
    该年无任何参与日线）→ 拒绝原文。

## 4. 固定断言清单（`results.json.assertions` 逐项记录 ok/detail）

1. 三证券 yearly item `success:true`（前置未满足时记 `GATE_REFUSED`）。
2. 每证券 `data_versions` period='year' 一行，`record_count` = 请求年数，
   `first/last` 覆盖请求年域；`effective_heads` period='year' 指向该版本。
3. `yearly_lineage`：`base_daily_version_id` == 本运行 daily head；
   `algorithm_version`/`first_trade_date`/`last_trade_date`/`as_of` 在列；
   `as_of` 等于最后参与交易日（非采集时刻）。
4. `query_data` 读回 bar 与库内 `yearly_bars` 逐字段一致（O/H/L/C/
   trade_date/first_trade_date/last_trade_date/is_finished）。
5. **期望值独立机械聚合**：运行内 python（stdlib `Decimal`）从已发布
   `daily_bars` 重算自然年聚合——首交易日 open、max(high)、min(low)、
   末交易日 close；与发布值逐 (symbol,year,field) 比对并写入
   `results.json.expected_vs_actual`。**该聚合不使用 #54 冻结 oracle
   值，独立产出期望**；volume/amount 因 daily 未提升必须为 unknown/NULL，
   不得为 0 或推算值。
6. 未结束年（2026）：`is_finished=0`，`last_trade_date=2026-09-14`
   （最后参与日），不以运行时刻冒充。
7. S7 幂等：同 request_id `reused:true`；不同 request_id 同输入 →
   `no_change=true`，无新 version 行，`effective_heads` 不变。
8. S8 三拒绝：item `success:false`，error 逐字入
   `results.json.blocked_paths`；断言含 `SOURCE_NOT_VERIFIED`（env-absent）/
   非空 Integrity/Base 类错误（base-unavailable）/ coverage 类拒绝
   （coverage-不足）；三 scratch 库均无新版本入库。
9. 全部 `task_items.outcome`、receipt、`compat_json` 原文进
   `run-log.jsonl`；无选择性摘录。

## 5. 如实记录项（写入 README/results，不得省略）

- `PRODUCTION_BLOCKERS` 五条在运行时刻原样；发布版本 provenance 携带
  `NOT_PRODUCTION_READY`。
- C3=每10股送转数角色为链约束间接命名（indirect caveat），不另作主张。
- daily `volume`/`amount` 未提升 → yearly 对应字段 unknown。
- 非 cat-1 GBBQ 排除计数按 daily provenance note 留痕（76/5/18）。
- 快照 inspect 各 `NOT_VERIFIED` 维度原样；coverage=Complete 仅指成员
  自身区间，不声称上市完整历史。
- 运行方式 = 本机一次性手工；非部署、非 ecs、不改 gate。
- 若 yearly verifier 前置未落地，S4–S6 以门禁拒绝如实收尾，
  `yearly_publish=GATE_REFUSED`，不视为验收通过。
