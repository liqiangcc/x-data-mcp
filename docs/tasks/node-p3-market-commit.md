# Node 迁移 P3 — 发布提交事务（commit_range / commit_publication）

任务：NM-053（agent-coordination/node-migration）。日期：2026-09-24。
基线：`579e00a2aea5bbf683a34f8ed7c856a84cc35dfe`。Node v20.19.2。
关联：Epic #271；`src/sqlite_market/update.rs` `commit_range` +
`src/sqlite_market.rs` `commit_publication` + `task_control::decide_head`
+ `sqlite_tasks::persist_decided`。
证据：`evidence/node-migration/p3/market_commit/`。

**结果：18 场景 / 180 操作 / 0 DIFF。**

## 1. 范围

`src_node/market/market_commit.mjs`：

- `commitRange` 逐字顺序：read_result 重放（audit 全等否则
  "range result id reused with different inputs"）→ publication_id
  在 publication_batches + yearly_execution_results 的唯一性 →
  base 重读 + raw 相等 → no_change 分支（required_heads CAS →
  finish_task → persist_decided）或 commit_publication → audit
  payload 写入 → market_update_results 插入（result_digest）→
  回读 receipt。
- `commitPublication` 逐字顺序：批次重放 → result-id 重用 →
  candidate 重读 + raw 相等 → `validated` 状态闸 → base digest →
  required_heads CAS → read_item_binding + finish_task →
  publication_batches + task_publications 插入 → 受 state='validated'
  守护的发布 UPDATE → promote head upsert（恰好 1 行）→
  persist_decided → 提交后 read_projection 校验。
- `task_repository.mjs` 导出 `decideHead`（decide_head 全臂移植）
  与 `persistDecided`（write_item_row + counter_delta +
  guarded_task_update + 终态释放 occupancy）——market commit 复用
  已移植的任务写路径，未重写 SQL。
- `market_read.mjs`：`parsePublication`/`parseReceipt` serde 形态
  校验（缺字段 → "missing field \`X\`" 逐字），check_publication_link
  与 read_result 复用。

## 2. 差异验证（market_commit：31 ops / 0 diffs）

fixture：`|validated` 记录修饰符 + DIFF_RESULTS 两行结果 + 串行
admit→claim→start→commit 链（occupancy 每 scope 独占）。

覆盖：no_change 发布、完整发布 + head promote、重放幂等、审计分叉
冲突、publication-id 重用（batches/results 两路）、base 已变、head
已动、未验证候选、坏 expected_revision；提交后 head/versions/
read_result/item_receipt/range 读。每个 commit_range 输出 11 表
归一化摘要——逐字节等价已证。

## 3. 范围边界

无 compact 后端、无 gaps、无 yearly_execution_results 写入器、无
RangePlan 产生端（prepare/审批）、无验证执行、无服务。

## 4. 验证命令

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p3/diff_harness_probe.mjs .` | 0（18 场景 MATCH） |
| `node ... diff`（自测） | 1 |
| NM-052..NM-035 回归 | 0 |
| `sha256sum -c` / `git diff --check` | 0 |
