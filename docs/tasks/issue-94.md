# 固定 daily 派生 yearly 应用链（#94）

依据 #3 当前本地年线规则、#4 AC-80—89 与 #79 已冻结发布矩阵。
SQLite 设计 c2f89085850423734193f3090dd435c34f4688ec；实施基线 2c706325aa2020382631355f802403c05b9662d8。
实现 base 5e3d645fa97506cb8640ccc6567253e4b4ec01f4。不修改开放设计 PR。

首个切片：Publisher.derive_yearly 只读 fixed daily，验证身份及证据绑定，调用现有精确聚合。
输入显式 symbol/base/年度集合/derived_at；DerivationEvidence 只供可信内部 verifier/test 装配，不支持反序列化/MCP。
完整取得说明须绑定 base digest、年度及 Shanghai as_of；不代表全年/上市以来完整，不生成 analysis_coverage_v1。
legacy record 保留 batch/digest；新增 local_yearly_derivation_v1 扩展保留逐年 lineage、未结束标记及原 daily 获取时间。
不修改任何旧 record。不存在新的 DDL、SQL 规则或在线 provider。

prepare_derived_yearly 校验固定 base 未变，stage 候选并复用已有 Publisher.prepare。
sealed PublishPlan 的 base digest、daily/yearly head、revision/occupancy 条件继续由已有原子提交核对。
historical-fixed-base 不推进 head；current-head 两头任一变化拒绝，不降级、不 rebase。
候选构造不改变任务/head。prepare 失败可能留下未发布候选，与已发布事实严格区分。

AC → 测试（tests/yearly_publication.rs）：
- AC-01/03 coverage_binding_and_base_identity_reject_before_staging；fixed_base_damage_after_derivation_never_substitutes_another_version。
- AC-02/05 local_candidate_publishes_and_queries_fixed_year_with_original_daily_untouched：多年 daily 只聚合 2025，真实 SQLite 发布/固定查询，保留原 daily。
- AC-04 historical_base_can_publish_after_daily_advances_but_current_intent_conflicts。
- 既有 tests/sqlite_market.rs 继续覆盖双 head、task/occupancy、篡改与真实进程退出矩阵；不是新范围/worker 的证明。

尚未交付：显式 yearly 更新任务、无变化复用、范围外旧 yearly 处理、public wire/worker/进度与部署。
当前只是新的候选构造路径，不能作为完整 update_data(yearly) 或重复更新规则。
真实 verifier/source 尚未实现，#54/#71/#46/#6/文件一致性 NOT_VERIFIED，live source 与四股复验 NOT_RUN。
所有新增测试只使用固定 fixture；NOT_PRODUCTION_READY。不扩展到目录、来源研究或迁移。

本机仅 fmt/diff；完整编译和测试由 Actions 执行，准确 run/job/结果记录在 PR 评论。未执行不得称 PASS。

范围保护：current-head 的既有 yearly 若含候选未选择的年度，prepare 拒绝，不隐式删除或从不同 daily 拼接。明确 historical 意图仍可发布选定年度 immutable version，原 head 不变。测试 selected_historical_year_cannot_discard_other_years_from_effective_head 使用真实 SQLite 验证两条路径；不是自动降级。

## 显式任务切片（编码前记录）

新增内部 explicit-yearly-task-v1：每项 symbol、固定 daily version、年度集合，以及明确 historical/current-head 意图。年度去重排序，证券集合规范化；同证券不同年度仍共用股票 × yearly 占用。request_id 全局语义沿用 #78，不从执行状态重新选择范围。使用单次 fixed_many 核验全部基础版本并冻结 digest/years/intent；resolve 按服务端上海日期拒绝未来年度，不能用调用方的 as_of 绕过。日期依赖是应用装配输入，不是 MCP 字段。

不新增 DDL。prepare_derived_yearly 对 update/backfill 任务要求匹配此内部契约及精确冻结输入（包括 intent）；无法扩大年度/更换 base/改变 head 策略。旧首次采集 fixture 契约仅继续验证原发布基础设施，不被当作新受理证据。

测试计划：真实 SQLite 同语义乱序/重复幂等、异参 request 冲突、不同年度占用冲突；缺失/未来年度无绑定；两连接同时受理一个 request；取消/重试保持冻结事实；有效任务可发布，扩大年度或改变意图拒绝。
