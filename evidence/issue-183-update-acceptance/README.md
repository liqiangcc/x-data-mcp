# Issue #183 Phase C — `update_data` official-file 真实执行验收记录

2026-09-16/17，按本目录 `PLAN.md`（PR #184 冻结）对 retained-inputs gen-2
官方快照执行真实 `mcp_data` stdio 服务验收。Phase B（`TdxRangeProvider`，
PR #185）已合并。本机一次性手工驱动，不接 ecs、不改 gate。

## 计划偏差说明

执行前修订一处（如实）：PLAN §4/§5 引用的 `publication_receipts` 表名不
存在，update 执行留痕实为 `market_update_results`（含 `base_version_id`/
`result_version_id`/`no_change`/audit+receipt sha256 列）；S3/S5/A6 改为
引用真实表名，断言语义不变。无其他偏差。

## 运行身份

| 项 | 值 |
|---|---|
| 代码 | `c48797a`（merge #185，TdxRangeProvider 实装） |
| 构建 | `cargo build --locked --bins`（rustup stable 1.98.1，debug） |
| 时间 | 2026-09-16T16:52:00Z – 17:02:31Z（UTC，run-log.jsonl 首末行） |
| 验收日 | `end_date = 2026-09-17`（Asia/Shanghai，PLAN §2 冻结口径） |
| manifest | `evidence/issue-3-acceptance-run/snapshot-manifest.json` sha256 `b8a0d7b9…f444d3`（复用 #178） |
| 快照根 | `/home/box/acceptance-run-issue3/snapshot/`（retained 硬链接） |
| 库 | `/home/box/acceptance-run-issue183/db-{main,r1,r2,r3}.sqlite` |

## 执行结果（全部留痕于 `run-log.jsonl`）

**S2 base 发布**（db-main）：sz.000002 `collection-version-6ce8d0bd…f639`
8479 bars（1991-01-29→2026-09-14）；sh.600006
`collection-version-d4f35003…f623` 6467 bars（1999-07-27→2026-09-14）。

**S4–S5 update 执行**:`update_data` explicit-range（u1，range
`[1991-01-29, 2026-09-17]`）→ `task-c55411c56e26a215751` item
`success:true`；`market_update_results` 留痕 `no_change=1`、
`result_version_id` == base `…6ce8d0bd`、audit_json 绑定
`algorithm_version=local-range-update-v1` + base integrity sha256。

**S6–S7 幂等**：同 request_id `u1` 重发 → `reused` 同 task_id；新
request_id `u2` 同 frozen inputs → `task-0268a1581efb9a58324` 仍
`success:true` + `no_change=1` + 同 result_version_id；全库
sz.000002 market.daily 版本恰 1 行——**无第二版本**。

**S8 读回不变**：oracle `2000-01-10`（-8.14/-8.06/-8.15/-8.06）与末 bar
`2026-09-14` 读回逐字段一致；effective_heads 不变；全量 daily_bars
canonical 读回哈希与 update 前一致。

**S9 provenance**：版本 `snapshot_inputs` archive sha256 = 实测
hsjday `c82f852a…` + gbbq `daad6290…`；gbbq member sha256
`4a3e9b86…9d77` 命中 MANIFEST pin。

## 拒绝路径（verbatim，`results.json.blocked_paths`）

| 拒绝 | scratch | 实际 verbatim |
|---|---|---|
| R1 env-absent | db-r1 | `INVALID_MARKET_COMMAND: SOURCE_NOT_VERIFIED: NOT_PRODUCTION_READY; publish blocked; <五条 PRODUCTION_BLOCKERS>` |
| R2 uninspectable manifest(`{"files":[]}`) | db-r2 | 同上 verbatim |
| R3a base 不存在（64×0） | db-r3 | `INVALID_COMMAND: explicit published daily base unavailable` |
| R3b base 身份不符（entity=sz.000002,base=sh.600006 版本） | db-r3 | `INVALID_COMMAND: fixed daily base identity mismatch` |

R1/R2 为 accept 成功 + 执行期 item `success:false`（无 env →
`OfficialRangeProvider` 阻塞路径）；R3a/R3b 为 accept 期拒绝，四 scratch
均无任何新增 market.daily 版本。

## 断言（`results.json`：16/16 PASS)

A1 输入完整性 / A2 双证券 base 发布 / A3 受理 frozen_inputs /
A4 执行成功 / A5 no-change 复用 base 版本 / A6 `market_update_results`
留痕 / A7 同 request_id reused / A8 新 request_id 幂等 /
A9 读回不变 / A10 provenance / A11–A14 四拒绝 verbatim+无版本 /
A15 五条 PRODUCTION_BLOCKERS + NOT_PRODUCTION_READY 原样保留。

## 如实标注

- **changed-version update = NOT_RUN**：仓库只有一个 pinned 官方
  snapshot generation（gen-1 永久不可得），不伪造、不 synthetic 冒充。
- update item 成功以"复用既有 effective version 且无第二版本"为
  PASS 口径；本 slice 不证明 changed-update 语义（需第二 generation）。
- `PRODUCTION_BLOCKERS` 未解除；`NOT_PRODUCTION_READY` 不变；
  volume/amount 未提升；本机验收非生产部署。

## 复现

```bash
cargo build --locked --bins
python3 evidence/issue-183-update-acceptance/run_update_acceptance.py
```

前置：`/home/box/retained-inputs` 三 zip 与
`/home/box/acceptance-run-issue3/snapshot/` 硬链接目录在位。
