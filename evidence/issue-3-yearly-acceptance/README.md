# Issue #3 yearly 验收证据运行记录（main `c5351ad`，含已合并 verifier）

2026-09-16，按本目录 `PLAN.md`（PR #179 冻结）对 retained-inputs gen-2 官方
快照执行真实 `mcp_data` stdio 服务端到端验收。本机一次性手工驱动，不接
ecs、不改 gate。

## 计划偏差说明

PLAN §4.3 初稿把 `yearly_lineage.as_of` 误写为"最后参与交易日"；按
`yearly::aggregate`/`lineage_values` 实现语义，`as_of` 是派生时刻的
Asia/Shanghai 日期（本次运行 `2026-09-16`），首末参与日由
`first/last_trade_date` 承载。PLAN 已在同分支修正后执行，无其他偏差。

## 运行身份

| 项 | 值 |
|---|---|
| 代码 | `c5351adfd6a41055407352987c986f261c860852`（merge #180，`OfficialYearlyVerifier` 实装） |
| 构建 | `cargo build --locked --bins`（rustup stable 1.98.1，debug） |
| 时间 | 2026-09-16T08:59:16Z – 09:18:10Z（UTC，`run-log.jsonl` 首末行） |
| manifest | `evidence/issue-3-acceptance-run/snapshot-manifest.json`，sha256 `b8a0d7b9…f444d3`（PLAN §1 pin） |
| 快照根 | `/home/box/acceptance-run-issue3/snapshot/`（retained 硬链接） |
| 库 | `/home/box/acceptance-run-issue3-yearly/acceptance-yearly.sqlite` 新建 |

## 执行序列与结果（全部留痕于 `run-log.jsonl`）

- **S1–S2** `task_sqlite init` → `mcp_data`（env 注入）→ `start_collection`
  market.daily → `task-86ec601cfc31b95243d0a311180a9142` succeeded：
  sz.000002 8479、sh.600006 6467、sh.600004 5630 bars，末日 2026-09-14。
- **S3–S5** 逐证券 `update_data` market.yearly（`current_head`，
  `expected_yearly_head:null`，years=全覆盖自然年）→ 三 task 全部发布：

  | 证券 | yearly version | 年数 | 首记录 | 末记录 |
  |---|---|---|---|---|
  | sz.000002 | `yearly-version-1c88b8a4…9702` | 36 (1991–2026) | 1991-12-31 | 2026-09-14 |
  | sh.600006 | `yearly-version-071b6298…8fd7` | 28 (1999–2026) | 1999-12-30 | 2026-09-14 |
  | sh.600004 | `yearly-version-91de60af…b1b9` | 24 (2003–2026) | 2003-12-31 | 2026-09-14 |

  `yearly_lineage`：`base_daily_version_id` 各绑定本运行 daily head；
  `algorithm_version=local-natural-year-v1`；`first/last_trade_date` =
  参与聚合首末日线（1991-01-29/1999-07-27/2003-04-28 → 2026-09-14）；
  `as_of=2026-09-16`（派生日）。
- **S6** `query_data` market.yearly 读回首年/2025/2026/末年共 12 条，
  与 `yearly_bars` 逐字段一致。
- **S7** 幂等：同 request_id 重发 → `reused`；新 request_id 同输入 +
  `expected_yearly_head=<head>` → `task-1fefb14e…`，
  `yearly_execution_results.no_change=1`，结果复用同一 version id，
  无新版本行。
- **S8** 三拒绝 verbatim（`results.json.blocked_paths`）：
  - env-absent（同库重启无 env 服务）：
    `BASE_UNAVAILABLE: SOURCE_NOT_VERIFIED: official-file source not installed`
  - base-unavailable（`collection-version-`+64×0）：
    `ACCEPTANCE_NEEDS_VERIFICATION: MARKET_NOT_FOUND`
  - coverage-不足（sh.600004 years:[1990]，daily 始于 2003）：
    `ACCEPTANCE_NEEDS_VERIFICATION: selected year absent from fixed daily`
  三条均无新版本入库（全库 yearly 版本恰 3 行）。

## 断言（`results.json`：24/24 PASS）

- 三证券 yearly item `success:true`；版本 `record_count` = 请求年数；
  `effective_heads` period=year 就位。
- **期望值独立聚合**：python stdlib `Decimal` 从已发布 `daily_bars` 重算
  自然年 OHLC（首 open/max high/min low/末 close），逐 (symbol,year,
  field) 与发布值比对——88 个年度记录零差异；**不引用 #54 冻结 oracle**。
- 未结束年 2026：`is_finished=0`、`last_trade_date=2026-09-14`。
- `volume`/`amount` 全年 NULL（daily 未提升 → yearly unknown，不为 0）。
- 幂等：`reused` + `no_change=1` + 无新增版本。

## 未验证项（如实）

- `PRODUCTION_BLOCKERS` 五条原样；受理响应与版本 provenance 携带
  `NOT_PRODUCTION_READY`；C3 间接角色 caveat、非 cat-1 排除（76/5/18）
  与快照 inspect 各 `NOT_VERIFIED` 维度不变。
- derived yearly 仅覆盖 daily 版本实际覆盖的自然年；不声称上市完整史。
- 本次为本机验收运行，非生产部署；`run_worker_once` 手工驱动。

## 复现

```bash
cargo build --locked --bins
python3 evidence/issue-3-yearly-acceptance/run_yearly_acceptance.py
```

前置：`/home/box/retained-inputs` 三 zip 与
`/home/box/acceptance-run-issue3/snapshot/` 硬链接目录在位（见 #178
evidence README 的复现节）；缺席即前置失败退出。
