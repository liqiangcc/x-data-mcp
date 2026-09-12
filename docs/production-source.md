# 官方文件生产来源架构（stage 1）

当前决策：[Issue #55](https://github.com/liqiangcc/x-data-mcp/issues/55)。
状态：**NOT_PRODUCTION_READY**。本阶段收敛生产入口和能力契约，不交付下载、parser、QFQ 或 yearly 算法。

## 唯一目标链路

```text
官方可下载文件 → 原始快照及获取记录 → 各格式解析与独立校验
  base/行业文件 → 当前沪深 A 股身份 + TDX 研究行业二级目录
  hsjday .day → 原始日线版本
  gbbq → 公司行动事件版本
原始日线版本 + 事件版本 + 已验证一致性/算法/基准 → 本地 QFQ daily
同一个已发布 QFQ daily version → 本地自然年聚合 → derived yearly
校验 → 有效版本发布 → MCP 读取
```

官方包可能包含北交所、B 股、指数等；文件成员不是范围/身份的证明。
base 或行业文件的确切映射未确认，不能把 HY、文件名或代码规律当作研究行业二级契约。
行业缺失/冲突可按既有规则降级，但不能用降级掩盖整个行业来源尚未成立。

生产装配只使用 `OfficialFilePipeline`，它目前无 I/O、无候选数据输出，始终返回
`SOURCE_NOT_VERIFIED`。没有配置能够将门禁改为通过。
`MarketDataPipeline::materialize_daily` 表达官方文件本地 QFQ；
`derive_yearly` 必须接收一个 `EffectiveVersion` 日线输入，没有外部 `fetch(year)` 方法。
Collector 在 year 操作中读取一次已发布 daily，固定该输入后调用本地派生能力。
没有 daily 时明确 `PUBLISHED_QFQ_DAILY_REQUIRED`，不自动转向外部 year，也不隐式追加采集范围。
日线失败且没有旧有效 daily 时年线同样不能成功；若有旧版本，其使用须显式引用该版本。

当前没有聚合算法。测试 Fake 的年线只验证编排、身份和版本关联，不证明 OHLC 聚合正确。
Publisher 拒绝无基础版本或与当前已发布日线不匹配的年线；保留 lineage 并参与版本内容比较。
参与首末日期须顺序正确，且两个端点均存在于固定 daily version 的记录中；不要求等于整个
版本的首末边界。例如基础版本跨 2024–2026 年，只派生 2025 年时可引用 2025 年的实际首末
参与交易日，符合 #3 AC-147 的指定历史年度重聚合规则。
这只是 stage-1 lineage gate，不证明区间内完整性、精确自然年参与集或聚合数值；这些由未来
真实 yearly 算法验证，不能把仅通过端点检查当作聚合正确性的证据。
读取旧年线不会补造 lineage，旧记录不因此获得新生产来源认证；不迁移或删除旧文件。

## 旧路径处置

- BaoStock：仅 evidence/交叉验证，不进入 runtime 或生产 fallback。
- TdxQuant/TdxAiData/tqserver：不保留生产接线/fallback；已有官方文档、Key 调查作为历史证据。
- Python bridge/eltdx：原 `src/tdx.rs`、`scripts/tdx_bridge.py` 和相关测试逐字移入
  `archive/online-tdx/`，移除 crate 导出与二进制接线。旧 Python 配置不再读取。
- 东方财富匿名 datacenter/gzfx：仅 historical evidence。
- 源端直接 QFQ：只作 evidence/独立比较，不与本地 QFQ 混合发布。
- 源端直接 year：从生产处理契约移除。API 中 `year` 仅表示本地 derived yearly 输出。

所有既有 `evidence/` 与历史文档保持原样，包括失败结论。
`docs/issue-6-catalog-design.md` 是旧 slice 记录，其中东财行业表述已由 #6 后续确认替代；
当前分类是 TDX 研究行业二级，当前生产来源方向以 #55 和本文为准。
不为已有研究投入保留备用生产实现。更新失败时报告失败、保留旧有效版本，不冒充刷新成功。

## 未通过的硬门禁

1. #6/#46：官方文件的身份、范围完整性及研究行业二级映射均需验证。
   #46 原 TdxAiData API 调查不能证明官方文件能力；不恢复 Key-gated SDK 生产路径。
2. #54：GBBQ float32 到业务十进制、事件字段、单位、舍入、复权基准及历史修订语义未完成。
   不采用容差、逐股规则、过滤失败事件或拟合 BaoStock 结果放行。
3. 官方文件长期服务器获取/使用边界未验证。公开 URL 或 HTTP 200 不等于用途授权。
4. .day/gbbq 独立发布时点的一致性未验证。同日下载、同一供应者或相同文件名均不足以放行。
5. parser、单位/币种、全目标范围覆盖、本地 QFQ/yearly 算法及真实发布闭环尚未交付。

前三项任一不成立就不能宣称纯文件方案可满足首期；应重新审视范围或选型，不能偷偷增加 fallback。

## 后续实现必须兑现的 provenance / gate 契约

本节是设计约束，不是已采集数据，也不声称已经实现持久化字段。
当前阶段始终阻止生产输出，所以不允许以一个空 manifest 或布尔开关模拟通过。

- 原始文件快照：来源 URL、获取时间、字节数、SHA-256、归档成员及成员哈希、可用的
  来源更新时间/ETag。未知来源时间保留 unknown；本地获取时间不冒充来源生效时间。
- 解析输入：分别固定 catalog、.day、gbbq 输入的快照身份、解析器版本、字段及单位契约版本。
  文件名不能代替内容身份，ZIP 校验不能代替业务完整性校验。
- 一致性门禁：记录所组合的 .day/gbbq 哈希与来源更新时点、实际行情截至日期、事件覆盖，
  以及证明该组合可计算的独立证据引用/规则版本。没有可审计的组合依据则拒绝发布。
  判定阈值和更新时差策略尚未确认，不自行设定。
- QFQ lineage：raw daily version、event version、适用目录版本、一致性证据、算法版本、
  基准日及事件链。事件修订可能改变历史因子，不能只追加最新一天并声称历史一致。
- yearly lineage：同一个 published QFQ daily version_id、算法版本、本次实际参与的首末交易日期。
  参与范围可以是基础版本的子集，不能强制扩大到整个版本的边界。当前 `YearlyDerivation`
  已保留这些字段；精确自然年参与集和完整多年度输出的逐年度截至信息留给聚合实现。
- yearly 计算按 Asia/Shanghai 自然年；OHLC 使用首开、最高、最低、末收，精确十进制。
  volume/amount 仅在全部参与值已知且单位/币种一致时求和，否则 unknown；不填 0。
  未结束年度标明未结束及实际截至交易日期。基础 daily 改变后重新生成受影响 yearly 版本。

最终 parser 应有格式/损坏/单位/范围契约测试，QFQ 应有独立语义证据，yearly 应验证固定版本
聚合及修订传播。当前离线 gate 测试不能替代以上验收。

## 运行与验证边界

MCP 仍提供原五个任务工具，受理返回 pending；显式 `run_worker_once` 后官方文件日线报告失败。
年线无基础 daily 时报告未验证，有基础 daily 时同样由 blocked pipeline 拒绝。
这是诚实的停用状态，不是持续运行/自动采集服务。无下载、账号、Key 或网络探测。

现有发布/任务存储非原子问题未在本次修复，旧 bridge 管道问题只因移出生产而不可达，
未修复归档代码。真实来源、部署、全市场完整性及性能仍 NOT_RUN / NOT_VERIFIED。
