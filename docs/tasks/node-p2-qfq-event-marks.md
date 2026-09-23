# Node 迁移 P2 — QFQ event_day_marks

任务：NM-017（agent-coordination/node-migration）。日期：2026-09-23。
基线：`588d19981faf8c2c80b801786a247f8775bbfe61`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271。来源：`src/qfq.rs::event_day_marks`。上游：NM-013 仿射、NM-016 解码事件。
证据：`evidence/node-migration/p2/qfq_event_marks/`（probe-results.jsonl 29 行全 PASS、rust-oracle.jsonl、results.json、SHA256SUMS、README）。

**结果：29/29 PASS / 0 FAIL —— 13 例 Rust oracle 逐字一致 + 16 例类型边界。**

## 1. 移植：`src_node/algorithms/qfq_event_marks.mjs`

| 语义 | 实现 |
|---|---|
| bars 升序稳定排序；**事件保持输入序** | `sort((a,b)=>a.ymd-b.ymd)` + `events.map` |
| **每事件先归一化因子再查 bar** | `factors(c_raw)` 在 bar 查找之前 —— 无前置 bar 时 NonFiniteFactor 仍触发（Rust 错误优先级一致） |
| `previous` = 严格早于 event_date 的最后一条 bar（`ymd < date_raw`） | 从排序数组尾部向前找 |
| `displayed_previous_close` | NM-013 `displayedPreviousClose` → 契约串，无前置 bar 为 null |
| 输出 | `{category, displayed_previous_close, event_date, previous_bar_date}` 字段对齐 `EventDayMark` |

## 2. 类型边界（REVIEW-NM-014 条件落实）

**两模块均已强制执行并测试**——不是文档备注：

- `event_day_marks`：`date_raw` u32、`category` u8、`ohlc_raw` 4×u32、`YYYY-MM-DD` 全部校验；`TypedBoundaryError`，**先于**因子检查（fail-closed 优先序，已记录）。
- `forward_adjust`（窄修正）：`date_raw >>> 0` → 严格 u32 校验（拒绝负数/小数/字符串/>2³²−1）；`ohlc_raw` 4×u32 校验；非法 bar 日期已拒。u32 边界值 0 与 2³²−1 仍接受。
- NM-014 探针回归：15/15 PASS。

## 3. 验证矩阵（29 例）

| 组 | 例数 | 结果 |
|---|---|---|
| rust_oracle | 13（valid、no_prior、event_between、after_all、重复日期、未排序事件保序、未排序 bars、3×非有限因子错误优先级、空 bars/事件、当日首bar） | PASS 逐字 |
| marks 边界 | 9（负/小数/字符串/超大 date_raw、超 u8 category、坏 ohlc/日期/非数组、边界先于因子） | PASS |
| fwd 边界跟进 | 7（同四类 date_raw 拒绝、坏 ohlc、u32 边界值接受） | PASS |

## 4. 范围边界

- 不做 category 业务解释、服务接线、yearly。
- 边界检查先于因子检查：两类均 fail-closed；Rust 侧为静态类型无此层，已记录差异。

## 5. 验证记录

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p2/qfq_event_marks_probe.mjs .` | 0（29 PASS） |
| `node scripts/node_migration_p2/qfq_forward_probe.mjs .` | 0（15 PASS 回归） |
| `rustc` oracle（`target/debug/marks_oracle`） | 0 |
| `json.tool` / `sha256sum -c` / `git diff --check` | 0 |

## 6. 范围声明

新增 `src_node/algorithms/qfq_event_marks.mjs`、`scripts/node_migration_p2/qfq_event_marks_probe.mjs`、`docs/tasks/node-p2-qfq-event-marks.md`、`evidence/node-migration/p2/qfq_event_marks/`；窄修正 `src_node/algorithms/qfq_forward.mjs`（类型边界）。无 Rust 改动、live DB、ECS、deploy/push/merge。
