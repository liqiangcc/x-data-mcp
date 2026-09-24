# Node 迁移 P3 — 差异验证线束（Rust oracle vs Node diff_runner）

任务：NM-049（agent-coordination/node-migration）。日期：2026-09-24。
基线：`97f58831fadb337f9b8948de2a586f7fd8fc8f50`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；真实 `SqliteTasks`/`task_control` API 与 `src_node` 端口逐操作对拍。
证据：`evidence/node-migration/p3/diff_harness/`（probe-results.jsonl 8 场景全 MATCH、negative-selftest.jsonl、results.json、SHA256SUMS、README）。

**结果：8 场景 / 59 操作 / 0 DIFF。**

## 1. 组成

- `src/diff_harness.rs`（`#[cfg(test)]`，lib.rs 注册）：读 `DIFF_SCENARIO`，对 `DIFF_DB` 调用真实 `repo.accept/transition/candidates/reconcile/read_task`，发 `{seq,op,status,code,message,task,unchanged}` JSONL 到 `DIFF_OUT`。rustup 1.98.1 编译；fixture 在 /dev/shm。
- `scripts/node_migration_p3/diff_runner.mjs`：同一词汇表跑 `TaskAdmission`；`$T1`/`$REV:T1` 符号解析；存储字节用 NM-048 规范序列化器（结构体序 + Value 排序）——保证 stored `command_json`/`frozen_work_json`/`outcome_json` 与 Rust `encode` 字节一致。
- `scripts/node_migration_p3/diff_compare.mjs`：逐 op 比对 status + 映射 code + 逐字 message 类 + task 投影 + 拒绝时 state_digest 不变性。

## 2. 错误码等价表（逐字）

RevisionConflict↔revision_conflict；NeedsVerification↔needs_verification；
InvalidRetrySelection/RetrySelectionInvalid/EmptyRetrySelection↔invalid_retry_selection；
RequestConflict/RetryRequestIdConflict↔conflict；RangeConflict↔range_conflict；
InvalidCommand/EmptyRequestId↔invalid_input；StorageUnavailable↔unavailable；
TaskNotFound↔task_not_found；OutcomeUnknown↔outcome_unknown；UnsupportedContract↔unsupported_contract。

## 3. 场景

| 场景 | ops | 覆盖 |
|---|---|---|
| lifecycle_ok | 8 | admit→claim→start/finish→finishExecution |
| lifecycle_cancel | 7 | cancel→finish item→finishCancellation |
| retry_ok | 9 | 失败 item 重试（key=原、retry_of） |
| retry_rejections | 11 | 成功 item/intent 多键/陈旧 rev/request 复用 |
| run_identity_gate | 2 | **分歧 1**：外 run claim → needs_verification |
| cancel_noop_order | 5 | **分歧 2**：cancel_requested 上外 run → needs_verification |
| reconcile_candidates | 8 | candidates 过滤 + reconcile 标记 |
| guards | 9 | 陈旧 rev/幽灵任务/success+error |

## 4. 已证检测力

回退 NM-043 R1 run-identity 门 → `run_identity_gate` DIFF（status error!=ok）。恢复后 MATCH。
本轮真实捕获：`InvalidRetrySelection` 是单元变体 —— Node 详情消息已替换为逐字 `INVALID_RETRY_SELECTION`。

## 5. 范围边界

无 market/publish/verification-execution ops、无 CI 接线、无服务。

## 6. 验证命令

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p3/diff_harness_probe.mjs .` | 0（8 场景 MATCH） |
| `node ... diff`（自测） | 1 |
| NM-048..NM-035 回归 | 0 |
| `sha256sum -c` / `git diff --check` | 0 |
