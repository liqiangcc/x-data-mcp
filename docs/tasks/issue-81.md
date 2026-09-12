# Stage D: consecutive_direction v1

授权：用户承担全部开发、审查与发布的最新要求；执行 base 为 #84 合并提交 787bc81。
固定设计：2c706325aa2020382631355f802403c05b9662d8 的 implementation-baseline.md、
compute-capabilities.md、preset-consecutive-direction.md；AC 以 #81 为准。

## 实施前参数与证据决策

注册 compute_metrics：inline，恰好一个 named input、一个 metric；固定 yearly VersionRef（含 digest），
明确自然年闭区间；consecutive_direction/local-consecutive-direction-v1；direction up/down，min_periods≥1，
price_field=close、equal_policy=break、gap_policy=reject，completed_only 默认 true。
支持 false，并返回 provisional；完整性未知/中间不完整仍不可计算，不静默改参数。

工程预算：1 input、1 metric，最多 1000 观察/跨度 1000 年，min_periods≤1000；价格沿既有 Decimal，
有理运算用 BigInt，整数工作值≤512 bit，输出 scale=12 half-even 一次最终舍入。无预热、无 latest、无任务。
正常 computed 含 lower_bound/matches=null；not_computable 单独结果联合，不冒充成功指标值。

既有 yearly is_finished 只证明按发布 as_of 是否年度未结束，不能证明年度覆盖完整。
新增可选、闭合的 `analysis_coverage_v1` 兼容记录扩展：origin=fixture、年度集合和每年 Complete/Unknown/Incomplete
覆盖说明及非空 evidence_ref。扩展在发布前写入完整 PublishedRecord JSON，被同一 legacy digest 固定；
正常普通查询不写入/补齐此字段。计算 Adapter 必须同快照核验整个 fixed version + 该证据，检查年份属于
固定 yearly、实际首末日期来自已核验 lineage/base。缺扩展则覆盖 unknown，绝不由 is_finished=false 升级。
目前只接受标记 fixture: 来源的显式测试证据；不提供 MCP 输入“complete=true”或生产覆盖放行开关。
这是 fixture 完成性契约，不是官方来源契约；真实完整性证明仍由 #54/#6/#46/#71 等生产链路承接。

纯算法使用已核验的 typed observations，业务完成性/连续性判断与 SQLite/MCP 分离。
输入窗口内严格有序不重复；尾部缺年/未知完成性拒绝；只剔除明确未结束尾年；打断点以前不无限扫描。
N 次变化需要 N+1 点。左界下界足阈值 true、不足 null；已知打断且不足 false。实际交易日期原样返回。

AC→测试：独立手算向量、同一固定版本 SQLite/MCP 组合、schema 负例、篡改拒绝、精确半偶与预算拒绝。
全部完整编译/测试在 Actions。本机来源、真实行情、迁移、远程认证 NOT_RUN；fixture 不解除生产门禁。
