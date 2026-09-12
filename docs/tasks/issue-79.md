# #79 Stage B 实施与验收记录

派发 `x-data-mcp-79-d2-20260912T1034Z`，既有 d2。工作树 `/root/x-data-mcp-issue79`，分支 `issue-79-sqlite-market`；共享脏树及 #78 历史树未修改。
实际 base `d8480dbfb03bafb5269381c447cbc5e46fa2c9a8`。
固定设计：SQLite `c2f89085850423734193f3090dd435c34f4688ec`；实施基线 `2c706325aa2020382631355f802403c05b9662d8`。
授权及开工核对见 #79 评论 5645353363、5645367338；未复用旧 Goal 或其他执行者工作树。

## 职责与调用

- `market_storage::compat` 保留完整原始 Value，删除且只删除顶层 integrity_sha256 重算 legacy-published-record-v1 摘要；解析已确认字段，不用结构体重建丢失未知字段/missing/null。原本缺摘要、coverage/单位/QFQ 未验证或 batch 证据缺失的记录不因重新计算摘要变成可发布。
- `market_storage::Publisher::prepare` 决定 intent 的 head 条件/推进动作，验证固定 daily 与年线数值，固化 base 摘要与任务 revision。`PublishPlan` 字段不可由外部装配写入。
- `SqliteMarket` 执行映射及事务。stage 写入未公开候选；prepare 后标记 validated；commit 取得新事实并执行应用层任务结束决策。正常写提交不重新聚合数值，而是核对 sealed candidate/base 摘要与投影。
- 事务 `BEGIN IMMEDIATE` 内核对 required heads、task revision/owner、目标 item、occupancy；同一事务写 publication_batches、版本 published、task_publications、必要 head 与任务结果。串行写锁覆盖读取条件到写入的区间；head 实际写入必须恰好一行，失败全部回滚。任务还有未完成项时保留保护，最后一项成功后才依既有决策收尾。
- `receipt(publication_id)` 读取持久版本/关联证据；同一 plan 再次 commit 核对完整 command 和 compat record 后返回原 receipt。它不是隐式重试新候选或自动 rebase。响应未知时先查原 publication_id。
- `market_storage::query::Pages` 是内部应用分页，不注册 MCP 工具。复用 #68 的 MarketQuery/VersionQuery 参数校验；游标固定版本/条件/查询时间，实例内最多 64 个。续页重新 verified read；版本列表固定首批条目，不跟随新 head/新条目。未规定跨重启游标有效。

DDL 仅增加 `data_versions`、`version_evidence`、`daily_bars`、`yearly_bars`、`yearly_lineage`、`publication_batches`、`effective_heads`、`task_publications`。
schema v3 使用已有显式 init/upgrade、checksum、WAL/FK/FULL 与 busy 预算；没有新依赖或 Cargo.lock 更新。
旧 #78 生命周期测试改为比较当前 schema 版本，v1 失败升级仍必须整体回滚。没有实际 JSON→SQLite 导入命令或自动迁移。

精确字段为规范化 Decimal TEXT；未知 volume/amount 为 SQL NULL，单位/币种/evidence 显式保存。日期/年度有版本内主键和有序范围 SELECT。
兼容副本及类型化表重复保存，空间随历史版本增长；不自动清理历史。

## 冻结发布矩阵

- base 不存在、损坏、身份/period/adjustment/source/单位不匹配：Base/Integrity 拒绝，不换 latest。
- historical-fixed-base：固定可核验 base，即使 daily head 已前进也可发布历史 yearly；plan 不携带 head promotion。
- current-head：固定 expected daily/yearly head；任何变化为 Conflict，不重算/rebase/降级，不覆盖新 head。
- base 摘要在 prepare 后变化即使当前内容能单独校验也拒绝，不能悄悄换成同 ID 的另一份输入。
- 数据损坏与可靠无版本区分；已有 publication 的 version 行缺失/状态改变或 effective head 失效，不返回“无数据”。

## Verified read 与成本边界

同一调用在一个只读事务中核验原 Value digest、完整版本/单位/coverage/lineage 投影及 publication 关联，再用 SQL 取范围行；SQLite 返回行与核验使用同一 snapshot。
yearly 显式读取同时核验固定 daily，并复用已合并年线核心验证数值。SQL yearly 选择自然年，不按月份裁剪年度 OHLC。

范围 SELECT 使用版本+日期/年度键及 LIMIT；完整性核验另需扫描整个版本，yearly 还需扫描固定 base。不能把整个调用称为仅扫描一页，也没有测量性能/延迟 SLA。
单版本校验实现预算为 100000 records，超限明确拒绝而不截断；这是实现保护，不是全市场覆盖承诺。
版本列表首批会核验全部成员，游标保留条目清单；这不是廉价元数据查询。跨用户翻页不保留数据库事务。

## AC → 具体自动测试

准确最终代码 HEAD、Actions run/job 和实际 PASS 在 PR #83 / #79 交付评论固定。
首个 slice 的手工 published-state fixture 已替换为真实 `stage → prepare → commit`；不能用早期 fixture 冒充原子性证据。

| AC | 测试名 | 证明范围 |
|---|---|---|
| AC-01 | `frozen_old_digest_matches_independent_literals_and_preserves_missing_null` | 固定旧 Rust 函数与独立 Python ASCII 样例期望均相等；missing/null、精确 Decimal、未知量额/units、coverage、daily/yearly lineage |
| AC-01/02 | `missing_digest_or_unverified_input_is_never_promoted_by_staging` | 缺摘要与重签名但未知 coverage 不进入发布 |
| AC-02 | `compat_bar_metadata_and_evidence_tampering_reject_without_fallback` | compat JSON、typed bar、版本 source、单位 evidence、digest 独立篡改均拒绝 |
| AC-02 | `yearly_lineage_projection_tampering_and_forged_published_flag_are_rejected` | lineage 投影篡改与只有 published flag 无 publication 关联均拒绝 |
| AC-02/04 | `damaged_published_state_or_missing_version_is_not_reported_as_no_data` | 已发布版本状态改变/行丢失不伪装为空数据 |
| AC-03 | `sqlite_market::tests::real_publication_exit_before_commit_and_response_loss_after_commit` | 真实 commit 子进程退出；提交前无半发布，提交后持久 receipt/版本/任务一致，同 plan 不重复版本 |
| AC-03 | `head_task_and_batch_write_failures_roll_back_every_publication_fact` | head/task/batch 写错误和 head 零行写入回滚，旧 head/任务保持 |
| AC-03 | `two_real_connections_compete_for_expected_head_without_last_writer_wins` | 两连接、同一合法 task item 的两个候选争 head，一成功一 Conflict；不是多业务 worker 证明 |
| AC-03 | `current_yearly_conflicts_when_daily_or_yearly_head_advances` | daily/yearly head 分别先变均拒绝，不降级历史发布 |
| AC-03 | `historical_fixed_base_publishes_but_never_promotes_even_after_daily_advances` | 旧完整 base 可派生历史 yearly，显式版本可读，现有 yearly head 不变 |
| AC-03 | `base_missing_corrupt_identity_period_and_adjustment_are_not_head_conflicts` | missing/corrupt/wrong identity/yearly-as-base/非 QFQ evidence 与“非最新”分开 |
| AC-03 | `fixed_base_change_after_prepare_is_rejected_without_recomputing_or_rebinding` | 固定 base 摘要改变不重新计算或换输入 |
| AC-03 | `cancellation_revision_and_occupancy_changes_block_publication` | 取消修订及占用缺失保守拒绝，无半发布 |
| AC-03 | `composite_task_keeps_protection_until_last_publication_commits` | 同一 day/year 复合任务，第一项发布仍占用，最后发布才终态/释放 |
| AC-04 | `verified_sql_range_is_ordered_precise_and_fixed` | SQL 日期范围、排序、精确值/NULL 和 keyset |
| AC-04 | `sqlite_market::tests::verified_projection_and_sql_return_use_one_snapshot_then_detect_later_damage` | 核验后另一连接篡改，本次返回旧 snapshot，下次读取拒绝 |
| AC-04 | `fixed_paging_ignores_new_head_preserves_conditions_and_rejects_later_damage` | 固定版本/查询时间、条件变更/跨实例拒绝、后续损坏停止不换版 |
| AC-04 | `version_list_freezes_members_and_tie_sort_order` | 发布时间倒序、同时间 ID 升序，新增版本不进入旧分页，旧成员损坏拒绝 |
| AC-05 | 现有 `tests/query.rs`、`tests/mcp_stdio.rs`、#78 SQLite/stdio suites 与全部 Cargo tests | 原 #68/默认入口回归；没有新增/切换生产 MCP wiring |

`crash_child` 只是故障测试子进程入口，普通运行返回，不另算一次故障证明。期望来源及固定旧函数见 `tests/fixtures/issue79/README.md`。

## 未验证及审查边界

- 当前仅 fixture/dry-run；真实来源、全市场采集、真实旧库迁移、目录生产、QFQ 文件链、生产后端切换、部署：NOT_RUN。
- #54/#6/#46/#71 门禁保持，NOT_PRODUCTION_READY。SHA-256 检查不证明官方来源或授权，不防止能重写全部证据的攻击者。
- 进程退出及 SQLite 事务失败不等于物理断电/磁盘故障耐久性；磁盘耗尽、真实 COMMIT I/O 模糊失败、性能与多业务 worker：NOT_RUN/NOT_VERIFIED。
- 仍使用单业务写 worker；两个 SQLite 连接测试只证明条件事务竞争，不表示外部执行/取消恢复协议已全量验证。
- 保留 staging/validated 失败候选，不自动修复/删除历史；本次不实现备份恢复、通用工具注册或 #80/#81。
- 普通读只核验，不建库/升级/回退 JSON。WAL/SHM 文件变化不是业务表写入。
- PR 保持 Draft，待 Coordinator 审查，不自动 merge/close #79 或父需求。
