# 真实来源闭环：证据、门禁与实施顺序（#88）

当前摘要：#54 QFQ、#131 固定文件集合一致性和 #71 限定个人研究范围已分别裁定；这些结论只适用于明确证据与声明范围。新 generation 仍需新 manifest 和独立一致性决定。
当前 acquisition 已实现 immutable generation、验证后条件激活和 generation-bound
routine update（见[每日同步说明](daily-sync.md)）；双 MCP 拓扑已在本机
部署运行——`mcp.sock` 的 `ops` 入口提供 15 data tools（`generic-data-v8`，含
`plan_market_update`/`run_market_update`，文本模式 `full`，market 单版本序列：
version_id=内容 hex，latest-only）、acquire socket 提供
10 acquisition tools；生产只有 `ops` 入口，`research`/`maint` 入口未部署（v8 第二阶段）。
已部署实例为 compact schema 24，代码（main）同为 schema 24
（M1 的 schema 22 + M2 PR-1 的 023 + M2 PR-2 的 024），注册 24 data tools（三个入口）；部署版本、generation
与数据量等机器事实只在 [STATUS](STATUS.md)
维护，本文不复述。历史记录（preview.29 时期）：2026-09-30 operator-path 曾激活
generation `2e9183306d4292c96602185152770a5ef54fbc28d1c9c909d3297c9d9bfe70a8`，
当时为 20 data tools / `generic-data-v3`。
generation 生命周期与全市场分批
collection/update 均有真实证据（历史：preview.26–28 连续验收：catalog 5,227 实体、
daily 首采约 340s、同代 no-change `update_data` 5,222 items 约 616s）。#259 已在
2026-09-28 owner re-baseline 后关闭：同代 no-change `update_data` 现行合同为
≤15min / ≤4KiB/item / `data_versions +0`，preview.27/schema20 实测
5,222 items / 616s / +18.5MB / 0 new versions PASS。尚未收口：cross-generation
envelope 约 37min 仅 measured baseline、真实 append/rewrite 待独立证据、
独立新会话 Skill 验收与外部分发授权；#294 为独立 admission 协议问题。
declared scope 仍为 owner 个人研究/本地使用、不对外暴露，external
distribution is not authorized，overall `production_ready=false`。

## 已发生的验收

实现基线 b21ba036479820bc36cdc75aa8aa312923ec64c6，v0.2.0-preview.1。
#78–#81 交付 SQLite/通用 API/首个固定 yearly 指标的 fixture 切片，未交付官方文件真实闭环。
用户报告 ChatGPT 当前会话已发现 13 工具并实际进入工具层；原始网页 JSON/请求 ID 未提供，不能冒充执行者独立重放。
[完整 live 反馈](https://github.com/liqiangcc/x-data-mcp/issues/86#issuecomment-5646368264)。
普通部署库任务、catalog/daily/yearly 版本列表为空，catalog 与 sh:600000/sz:000001 抽查为 absent。
抽查 daily 2020-01-01~2026-09-12、yearly 2020~2026，record_count=0。
query/compute ready 仅为接口实现；没有 published yearly 就不能算真实连续涨跌。
旧 API 429 是独立路径失败证据，保留；新增网页反馈不改写它。

## 门禁逐项判定

1. #54 GBBQ（已解决，2026-09-16 Final Gate PASS）：Windows V7.73 官方客户端行为证据
   （#167）把 C1=每10股派现（元，含税）、C2=配股价（元）、C4=每10股配股数、
   2dp HALF_UP 业务十进制输入、事件日分位取整参考价、非逐事件取整的仿射 QFQ 链
   提升为已验收契约；#169 全冻结域 94 事件回归 `unexplained_total=0`。
   实现：`src/gbbq.rs` + `src/qfq.rs`，official_market providers 已接线；
   `GBBQ_QFQ_NOT_VERIFIED` 已从 PRODUCTION_BLOCKERS 移除。
   精度边界（M1 PR-3，`qfq-exact-v1`，issue-383 §4.3）：**输入**仍是上面那条 2dp
   HALF_UP 业务十进制契约（`normalize_factor`），**输出**改为精确有理数链 + 末次一次
   舍入到序列精度（前复权 `price_scale=6`）。事件日参考价 `displayed_previous_close`
   是显示值，仍按 2dp 保留（`src/gbbq.rs` 的 `displayed_previous_close`）。2dp 基线
   （`qfq::forward_adjust`/`forward_adjust_reference`）保留为历史 oracle，不再写库。
2. #71 使用范围（已解决，2026-09-20 FG 限定 ACCEPT，comment-5749934485）：
   使用方声明范围=仓库所有者个人研究、本地使用、不对外暴露不再分发；
   服务架构本为本地 socket+UID 私有运行时。`FILE_ACCESS_USE_NOT_VERIFIED`
   已从 PRODUCTION_BLOCKERS 移除。边界：用途/主体/暴露面变化即失效需重裁；
   官方文档未给出授权文本，本裁定是使用方承担声明而非厂商授权证明。
3. #6/#46 目录：retained base 的 infoharbor_ex.code、tdxhy.cfg 只是原字段证据。
   缺当前沪深 A 身份/范围语义、名称覆盖/编码和 TDX 研究行业二级字典/层级关联证明。
   验收需固定文件/member/hash、字段语义和有界沪深/前导零/缺失/冲突矩阵；不能靠代码前缀或行数。
   已确认身份的行业缺失/冲突按 #6 降级，不能把未知身份视为行业缺失放行。
4. 文件一致性（已解决，2026-09-20 FG 裁定 ACCEPT，#131 comment-5749856666）：
   `evidence/issue-131-file-set-consistency/` 审计 pinned 2026-09-20 snapshot 集合：
   三角色字节重哈希匹配 manifest（manifest 自身 sha256 钉住）；全部 10,443 个已发布
   day 版本 `snapshot_inputs` 唯一绑定该 day+gbbq 归档对、零违例；5,221/5,221 heads
   出自该集合；base 经 catalog 版本绑定（source id=manifest 前缀+acquired_at 精确）。
   `consistency_check` → `VERIFIED_COMPATIBLE` 零 reason。
   `FILE_SET_CONSISTENCY_NOT_VERIFIED` 已从 PRODUCTION_BLOCKERS 移除。
   边界：仅认定该 pinned 集合对本代际可组合；新一代下载需新 manifest + 重审计。
5. LOCAL_PIPELINE（已退休）：#187 FG 裁定 bounded daily/yearly/update/verify+backfill
   四条链路真实源验收 PASS，`LOCAL_PIPELINE_NOT_IMPLEMENTED` blocker 整条移除。
   真实目录已有 bounded 官方快照发布验收（#194 v2：5224 成员发布 + 六缺名成员 `reject_candidate` verbatim 排除，FG 5711367159 裁定 `CATALOG_PUBLISH_NOT_VERIFIED` 退休）；changed-generation 目录更新与生产部署仍未验证。接口/fixture 测试不能填补来源证据。

## 跨文件一致性 validator（#131）

逐文件 `FILE_BYTES_VERIFIED` 与跨文件可组合性是两个独立门禁。`src/file_consistency.rs` 只接受显式、不可变的兼容性证明：证明必须绑定 base、hsjday、GBBQ 三个角色的精确 SHA-256、兼容集合自身的 identity/revision 和适用时间窗。`compatibility_set_identity` / `compatibility_set_revision` 只标识已审计组合事实，不表示三个独立发布文件天然共享同一来源版本；`member_bindings` 按角色保存各自摘要及可选 source identity/revision，`FileFact` 中的 source identity/revision 仍属于各自角色。相同 URL、下载日期、ETag、Last-Modified 或分别正确的摘要不会被推断为兼容。

validator 的结果是 `VERIFIED_COMPATIBLE`、`NOT_VERIFIED` 或 `CONFLICT`；明确矛盾优先于证据不足。输入顺序与完全相同的重复事实不改变结果，并保留角色/摘要/来源元数据和证据引用。`CompatibilityEvidence` 只应承载已验证、可审计的 evidence fact，不把调用方填入的字符串当作批准依据。初始 #74 retained 快照没有跨文件证明，测试稳定返回 `NOT_VERIFIED`；后续 #131 Final Gate 只接受上文“文件一致性”条目记录的 exact pinned 集合。该模块本身不增加 MCP 工具、不联网、不提供 fallback/force 开关。按 #262 D1 裁定，`verify_generation` 可在固定 validator 全部 PASS 后由 `x-data-acquire.auto-probe` 写入绑定 exact generation bytes 的决定 artifact；HTTP、ETag、文件名、单文件 SHA 或调用方字符串本身均不能自签 PASS。

## 本轮官方公开文档复核（2026-09-12）

只读取网页，不运行 SDK、不下载新行情、不登录或联系厂商。
- [get_divid_factors](https://help.tdx.com.cn/quant/docs/markdown/mindoc-1ctuhthaq5qmg/mindoc-1h10hsiat36k4.html)：字段和示例不定义 GBBQ 存储映射/scale 或参考价舍入顺序，未补齐 #54。
- [个人行情下载](https://www.tdx.com.cn/article/vipdata.html)：描述个人 PC 软件盘后下载及混合证券包，不能证明本项目服务器范围或 A 股目录契约。
- [全品类数据库](https://www.tdx.com.cn/tdxdata.html)：介绍面向客户的数据库和文件包推送；没有将该产品合同范围授予当前公开 base/hsjday/gbbq 使用，不能作为许可替代。
这是有界文档检查，不声称所有官方资料均不存在所需契约。旧 evidence 保持原样。

## 最新官方文件字节证据（2026-09-13）

新增的有界快照证据已经合并：

- [官方 `.day` 单成员](../evidence/issue-64-day-parser/official-sh600006-range.json)：通过 HTTP Range 读取 `hsjday.zip` 的 `sh/lday/sh600006.day`，成员 SHA-256 `91cb06b2ba0cca331f41f759e3aa0647e6eb5bd62c450cd0fca2e6b397362f7d`，仅证明 32 字节记录结构和日期边界。
- [官方 GBBQ 归档](../evidence/issue-54-gbbq-decimal/official/archive-20260913.json)：归档 SHA-256 `6b77f598004c2d651e86b7ec20e44ac42c8988132e389c9ce2d7c638e0dc924c`，`gbbq` 成员 SHA-256 `24f46cecc9b085ee6c0ef126fab33b405b8057747793eb5d25d44e624769ff60`，仅证明归档字节和观察到的 29 字节容器形状。
- [官方 base 归档](../evidence/issue-6-tdx-dbf-source/official/archive-20260913.json)：归档 SHA-256 `39de51e26c0ee8fb3fd970ee870f4cb955b8fbbd7f84d752157f3998e49b71d9`，包含 542 个成员及结构化 `base.dbf` 摘要，仅证明本次归档事实。

三份快照来自独立获取时点；这些证据没有证明业务字段、GBBQ/QFQ 语义、目录行业映射、跨文件一致性、服务器使用边界或生产 pipeline。门禁状态保持 `NOT_VERIFIED`，不得据此接入生产。

## 历史最小实施与验收计划（后续状态见本文开头）

以下 A–F 保留 stage-1 原计划，不是当前待办清单；已完成项和新 generation 缺口分别以上文裁定与 #260 为准。

A. 在 #54/#71/#46 补齐上述证据；跨文件一致性须有独立契约后才能组合。证据不足继续 blocked。
B. 契约成立后实现独立 Rust 目录/GBBQ 解析及 normalization/QFQ；保留未解释字段、拒绝不支持事件，先测试再接线。不得引入 tolerance、逐股规则或多源 fallback。
C. 文件验证与计算在发布事务外完成，产出固定 provenance；应用层判定 publish intent，SQLite Adapter 只提交 head/revision/occupancy 条件事务。
D. 有界真实验收固定 #53 四证券作为研究回归样本，不重选；先确认其身份和文件覆盖。QFQ 独立验证通过后发布 daily，再由同一版本派生 yearly；historical/current head 按 #79 原规则。
E. 用发布版本 ID 贯穿 query→availability→consecutive_direction；完整性证据不足返回 unknown/not_computable，不用 year_unfinished=false 代替年度完整。
F. ChatGPT 侧实际调用并保存参数、版本、结果、来源时间/摘要及失败记录；到此才能验收真实闭环，未覆盖的年度/证券不得扩大宣称。

Stage-1 当时不能进入 B–F；该历史判断不覆盖后续有界验收。当前仍不得以用户开发授权代替来源语义或第三方使用范围证据；新 generation 只能经当前 `verify_generation` validator 产生独立决定后条件激活。
Overall `NOT_PRODUCTION_READY` 保留，具体当前原因以本文开头和 runtime capability 为准。

## 历史能力发现修复切片

以下只记录 #88 当时切片；当前工具集合以两个运行实例的 `tools/list`/capability
为准（release smoke 当前验证 acquisition 10 个、data 20 个），剩余全市场与容量
语义冲突见 #259/#262。
公开 run_worker_once 纳入同一 registry，保留旧 {} 输入和 idle/task_id/status 输出。
详情说明同步任务/占用副作用和只拒绝来源的 Stage A 开发边界；不成为新生产执行器。
真实 stdio 测试比较工具名称集合、详情 schema，验证空队列、额外字段拒绝和执行后 failed。
受限应用上下文继续不发现/不能调用控制工具。远程工作区身份授权不由本修复新增。
