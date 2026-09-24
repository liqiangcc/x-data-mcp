# Node 迁移 P3 — 市场读取路径（legacy schema-18 后端）

任务：NM-051（agent-coordination/node-migration）。日期：2026-09-24。
基线：`47957eea916f49f99e41de563a859be2284bc29b`。Node v20.19.2。
关联：Epic #271；`src/sqlite_market.rs` 读路径 + `envelope.rs` + manifest 读。
证据：`evidence/node-migration/p3/market_read/`。

**结果：14 场景 / 124 操作 / 0 DIFF（市场读 6 场景 37 ops 全 MATCH）。**

## 1. 范围

`src_node/market/market_read.mjs`：

- `isCompactSchema`：按 `publication_batches` 缺席判定；compact DB → 类型化 unsupported（compact 派发属后续切片）。
- `readProjection` 逐字：行+publication_batches 存在性 → NotFound vs integrity 拆分；published-only 状态门；envelope `$ref` 展开（manifest `segments`/`records`/`payload:`/`annual_bars:` + 共享证据块）；compat::check；version_id/integrity_sha256/digest_format 身份；日上下文 yearly 拒绝（Base）；publication link 核对；metadata 投影等值；evidence_digest 重算；yearly/lineage 投影；日记录不得带 yearly 行。
- `baseRecord`/`validateBaseIdentity`/`validateBase`（复用 NM 移植 `yearly::aggregate`）。
- `range`：`Range::validate`、head 默认、scope 不符 `version/scope mismatch`、limit+1 `next_after`；段态记录走 manifest clip + `segment_bars`，payload 记录走 expand 后过滤。
- `versions`：orphan `publication references missing version`；`published_at DESC, version_id ASC`；published-or-referenced 过滤。
- `MarketRepository.open`：schema gate → readonly adapter。

## 2. Fixture 与差异验证

fixture DB 由真实 Rust `stage()` 构建（`diff_harness::build_market_fixture`：init → stage daily → 标记 published（FK 一致 publication/task_publications + head）→ stage yearly → published）。两侧 runner 各自拷贝 fixture；`sql` tamper op 逐语句同序执行。

- `market_read`（20 ops）：heads、fixed/candidate、versions、分页（limit 边界 + after）、version_id 覆盖、scope 不符、head 缺失、range 校验。
- tamper：payload 行删除 → `referenced payload missing`；metadata 列分歧；orphan（FK off 删除 data_versions）→ versions integrity + NotFound/integrity 拆分；staging 状态拆分（fixed→NotFound/candidate→ok）；envelope `$ref` 改 bogus → `unknown envelope reference`。

## 3. 范围边界

Node 侧无 stage/publish/commit 写路径、无 compact 实现、无 gaps/verification 执行、无服务。

## 4. 验证命令

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p3/diff_harness_probe.mjs .` | 0（14 场景 MATCH） |
| `node ... diff`（自测） | 1 |
| NM-050..NM-035 回归 | 0 |
| `sha256sum -c` / `git diff --check` | 0 |
