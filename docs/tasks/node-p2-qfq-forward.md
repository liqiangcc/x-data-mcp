# Node 迁移 P2 — QFQ forward_adjust 日bar遍历

任务：NM-014（agent-coordination/node-migration）。日期：2026-09-23。
基线：`6344d667f1610693dd79b7b8d7895c8ace1aff6c`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271。上游：P0 `qfq_forward_adjust` golden（3 行已验收）+ NM-013 仿射原语。
证据：`evidence/node-migration/p2/qfq_forward/`（probe-results.jsonl 15 行全 PASS、rust-oracle.jsonl、results.json、SHA256SUMS、README）。

**结果：15/15 PASS / 0 FAIL —— golden 字节级一致 + 8 个 Rust 实测边界。**

## 1. 移植：`src_node/algorithms/qfq_forward.mjs`

镜像 `src/qfq.rs::forward_adjust`：

| 语义 | 实现 |
|---|---|
| 事件先归一化后排序（非有限因子先于排序失败） | `events.map(factors)` → stable `sort` |
| `sort_by_key` 稳定排序（相同 date_raw 保持输入序） | JS `Array.sort`（ES2019 起稳定） |
| `event_date > bar_date` 严格大于 | `partitionPoint(dated, k, barDate)` |
| 后缀 memoized 链（每 distinct boundary 一次 fold） | `chains[k]` 惰性 `composeChain` |
| 乱序 bar → 全量 partition 回退 | `barDate < lastBarDate` 分支 |
| `raw_price` = `Decimal::from(cents)/100` | `_dec.div({m:cents,s:0}, {m:100,s:0})` |
| 2dp `MidpointAwayFromZero` 输出 + `to_string` | `_dec.roundAway(applyAffine(...),2)` |
| `amount_float32_bits/volume_raw/r7_raw` 原样透传 | 字段透传 |
| `chain_event_count` = 严格后于 bar 的事件数 | `dated.length - k` |
| category 过滤留在调用方边界 | 文档约定，与 Rust 一致 |

## 2. 验证矩阵（15 例）

| 组 | 例数 | 结果 |
|---|---|---|
| golden | 3（one_dividend_event 全字段、empty_events_identity、nan_factor_reject 错误文案） | PASS |
| rust_oracle 边界 | 8（未排序事件、重复事件日期、无当日bar事件、事件在全部bar前/后、乱序bar回退、两事件两bar、raw字段透传含 0x4d0f4406/999999/0xdeadbeef） | PASS（rustc+rlib 现场期望值） |
| fail-closed/结构 | 4（NaN 在合法事件间、index 3 因子、非法 bar 日期、空 bars→[]） | PASS |

## 3. 输出形状

`{amount_float32_bits, chain_event_count, close, date, high, low, open, r7_raw, volume_raw}` —— 与 `QfqBar` 字段一致；价格为 Decimal `to_string` 契约串（含 scale 保留形式如 `"15"`/`"15.10"`/`"10"`）。

## 4. 范围边界

- 无 GBBQ 字节解密、无 `EventDayMark`、无 yearly、无服务接线。
- 修正记录：NM-013 `qfq_affine.mjs` 无改动需求（add/mul 零短路语义在 NM-013 已修正并复验）。

## 5. 验证记录

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p2/qfq_forward_probe.mjs .` | 0（15 PASS） |
| `rustc` 边界 oracle（`target/debug/qfq_fwd_oracle`） | 0 |
| `json.tool` / `sha256sum -c` / `git diff --check` | 0 |

## 6. 范围声明

仅新增 `src_node/algorithms/qfq_forward.mjs`、`scripts/node_migration_p2/qfq_forward_probe.mjs`、`docs/tasks/node-p2-qfq-forward.md`、`evidence/node-migration/p2/qfq_forward/`。无 Rust 改动、live DB、ECS、deploy/push/merge。
