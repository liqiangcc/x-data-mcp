# Node 迁移 P3 — 市场写入路径：stage + 记录插入 + 结果/回执读取

任务：NM-052（agent-coordination/node-migration）。日期：2026-09-24。
基线：`a063d7eb98060feb0386e51decd344108bca7c3f`。Node v20.19.2。
关联：Epic #271；`src/sqlite_market.rs` 写路径 + `envelope.rs` pack +
`manifest.rs` build + `sqlite_market/update.rs` 结果读取。
证据：`evidence/node-migration/p3/market_write/`。

**结果：17 场景 / 149 操作 / 0 DIFF。**

## 1. 范围

`src_node/market/market_write.mjs`：

- `stage(raw)`：compat::check → `BEGIN IMMEDIATE`（Rust
  `TransactionBehavior::Immediate`）→ base_record →
  validate_base_identity → insert_record → COMMIT；内部 MarketError
  原样抛出（adapter `transaction()` 会重映射一切 throw）。
- `insert_record`：data_versions（'staging' + 逐字 metadata 投影 +
  envelope_json + content_digest + digest_format + evidence_digest
  重算）；yearly → yearly_bars/yearly_lineage；daily →
  bar_segments/segment_bars + `manifest::build` + manifest_root 回写。
- `envelope::pack` 逐字：`$segments`/`$records`/`payload:`/
  `annual_bars:` 标记 + 共享证据块提取 + 内容寻址 id。
- `manifest::build` 写侧：slice/concat/rebalance/mk/leaf/
  build_balanced —— base piece 直接复用 head manifest 子树。

`market_read.mjs` 新增：`requireRefs`、`read_result` /
`item_receipt` / `range_receipt`（digest 校验、audit 关联、
no_change==base==result、algorithm_version、base/result 重读、
task binding（revision+finished+success）、publication receipt 匹配）、
`read_item_binding` 移植。

## 2. 差异验证

新 ops：`stage`（含 11 表符号归一化摘要 —— 写入逐字节等价）、
`read_result`/`range_receipt`/`item_receipt`/`lookup_task`。

- `market_write`（20 ops）：fixture 由真实 stage() + 两次发布标记 +
  两个 result 行（res-nc/res-ch）；stage daily（Base-piece 共享 head
  manifest）+ yearly（published base）+ 重复 stage → UNIQUE conflict +
  yearly 无 published base → `BASE_UNAVAILABLE: MARKET_NOT_FOUND`。
- tamper：payload 删除 → staged 读 fail-closed；audit_json 覆写 →
  digest mismatch；publication receipt 覆写 → mismatch。

## 3. 范围边界

无 publish commit（commit_range/commit_publication/head CAS）、无
compact 后端、无 gaps/update 任务、无服务。

## 4. 验证命令

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p3/diff_harness_probe.mjs .` | 0（17 场景 MATCH） |
| `node ... diff`（自测） | 1 |
| NM-051..NM-035 回归 | 0 |
| `sha256sum -c` / `git diff --check` | 0 |
