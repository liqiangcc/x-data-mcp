# NM-056 — market tail: gaps + yearly_results + read_facts + publication_checks + backend open

证据：`evidence/node-migration/p3/market_tail/`（probe-results.jsonl 27 场景全 PASS、negative-selftest.jsonl、results.json、SHA256SUMS、README）。

## 范围

- `src_node/market/gaps.mjs` — `recordGapDecision`（tx 内 `gap_verification_decisions` 记录 + 身份冲突/verified 决策/proven-gap 复制 + `proven_gaps` 写入 + head 校验），`recordGapDecisionGuarded`（subject_version_id==effective head 守卫），`gapVerificationDecision`/`provenGap`/`openProvenGaps`/`recordProvenGap`/`coverGap`（事务 tx：`open_proven_gap`→验证 covering→发布范围校验→`gap_coverings` 冲突检查）/`gapCoverings` — 全部 verbatim 移植 `sqlite_market/gaps.rs`。
- `src_node/market/yearly_results.mjs` — `readYearlyResult`/`readYearlyResultRegistry`/`readYearlyAudit`/`readYearlyReceipt`：`result_sha256` 摘要 + 任务关联 + yearly 契约与 lineage 校验 + audit/receipt serde 投影。
- `src_node/market/verification_facts.mjs` — `readFacts`（`EvidenceRepository::publication_facts`）：契约 kind 分派（daily/yearly/legacy arm），`market_update_results`/`yearly_execution_results`/`publication_batches`/`task_publications` 关联校验，`Evidence::Committed|Missing` serde 形状，证据摘要 `[audit,receipt]` / `[record.raw,command_value,receipt]`。
- `src_node/storage/publication_checks.mjs` — `checkPublicationLink`（绑定读/registry 校验：audit sha256、目标关联、快照一致→`NeedsVerification`）+ `readCheckReceipt`（`all_recorded` 时重放 facts）。
- `src_node/storage/verification_storage.mjs` — `PUBLICATION_KEY` 修正为 `check_publication`；`snapshotCanon` 时间戳 serde `Z` 规范化（行存 `+00:00` chrono 文本 vs serde `Z`）。
- `src_node/market/backend.mjs` + `src_node/storage/database_lease.mjs` + `src_node/market/compact_runtime.mjs` — `MarketBackend::open`/`open_shared`：schema 分派；compact 分支走 `SqliteCompactMarket::open`（DatabaseLease：常规文件、link count==1、dev+ino 恒等、flock、supported filesystem）+ WAL/私权/身份检查。
- `src_node/market/collection.mjs` — `collectionReceipt`（collection receipt 读侧）。

## Harness

- `src/diff_harness.rs`：`DIFF_CHECK=1` fixture（legacy: update_data target+committed no_change result+committed publication-check；compact: failed-item target→not_applicable；legacy 附加 `start_collection`/`v1` 任务持有两条 publication_batches 覆盖 legacy arm）+ ops：`record_gap_decision`/`_guarded`/`gap_verification_decision`/`proven_gap`/`open_proven_gaps`/`record_proven_gap`/`cover_gap`/`gap_coverings`/`yearly_read_result`/`read_facts`/`collection_receipt`/`check_receipt`/`backend_open`/`backend_open_shared`/`head`。
- `state_digest` 扩表：`market_update_results`/`yearly_execution_results`（legacy）+ `gap_verification_decisions`/`proven_gaps`/`gap_coverings`/`publication_checks`（双 schema）。
- `lookup_task` 在 compact 路由走 coordinator-bound repo。

## 验证

- `market_tail`（schema-18）：52 ops MATCH；`compact_tail`（schema-19）：38 ops MATCH；全量 27 场景 PASS，0 diffs；selftest exit 1。
- 覆盖：gap 身份冲突/head 移动/无效事实/无效 covering/unknown gap+version/scope 校验；yearly 读 + 篡改（摘要/关联）；committed `read_facts`（daily arm + legacy arm Committed 摘要）+ `UNSUPPORTED_CONTRACT`/`TASK_NOT_FOUND`；`check_receipt`/`checkpoint` 含 audit 摘要篡改→`NeedsVerification` 与 audit 缺失；backend open/open_shared 双 schema。

## 本轮 harness 捕获的真实偏差

- `_readBoundTaskValidated`：`command_key_json` 的 serde 解析须在 frozen_work 校验之前（列序），否则 `{'{}'}` 行首报错不同；`missing field` 位置 = 闭括号列。
- 绑定读 claims 查询须带 `state_revision`（snapshotCanon 比较/断言所需）。
- `DatabaseLease` flock 错误须映射 std `TryLockError::WouldBlock` 文案；`backend_open`/`open_shared` 返回对象须显式 close 租约（Rust Drop 语义）。
- 遗留 arm `PublishIntent`：`Daily.expected_head` 与 `CurrentHead.expected_yearly_head` 是 `Option`；`HistoricalFixedBase` 是 unit（字符串形态）；intent×period 的 matches! 校验顺序。

## 边界与后续

- 本切片不宣称迁移完成/生产就绪/性能结论；`commitTx` 内部集成的 `run_collection` 等任务运行时驱动不在存储面。
- `backend_open` 的 WAL/文件权限在 `/dev/shm`（tmpfs）验证；overlayfs 路径仍由 `X_DATA_MCP_UNSAFE_NONLOCAL_FS` 显式 opt-out（不用于生产）。
- 与 `database_lease.mjs` 关联的 ExecutionGuard 仍由 NM-048 的 `verification_runtime.mjs` 负责。
