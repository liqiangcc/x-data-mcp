# 真实来源闭环：证据、门禁与实施顺序（#88）

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

1. #54 GBBQ：既有九事件矩阵的五个规范化候选均能解释 600006 半分边界，不能区分官方规则。
   缺 C1/C2/C3/C4 业务 scale、单位、float32→十进制映射和半分舍入位置的官方语义。
   验收需要独立可审计依据及事前冻结规则，再验证原矩阵与 #53 四股；不是新增 BaoStock 拟合。
   当前 NOT_VERIFIED，无 normalization contract，#53 四股本轮 NOT_RUN。
2. #71 使用范围：公开下载说明不等于本项目服务器使用许可。
   缺明确覆盖使用主体、base/hsjday/gbbq、获取频率、留存、解析计算及个人 AI 访问范围的依据。
   当前 FILE_ACCESS_USE_NOT_VERIFIED；不推定禁止，也不推定授权。
3. #6/#46 目录：retained base 的 infoharbor_ex.code、tdxhy.cfg 只是原字段证据。
   缺当前沪深 A 身份/范围语义、名称覆盖/编码和 TDX 研究行业二级字典/层级关联证明。
   验收需固定文件/member/hash、字段语义和有界沪深/前导零/缺失/冲突矩阵；不能靠代码前缀或行数。
   已确认身份的行业缺失/冲突按 #6 降级，不能把未知身份视为行业缺失放行。
4. 文件一致性：#74 能核验字节和取得时间，不能证明 .day/gbbq 业务上可组合。
   缺来源修订关联或可审计兼容性证据（适用证券、事件、截止时点及后续修订）。
   验收必须分别保存包/member hash、取得区间、语义契约版本和组合依据；同天下载/相同 ETag 不足。
5. LOCAL_PIPELINE：已有独立 .day parser、yearly core、snapshot inspector 和 SQLite 条件发布。
   尚无通过以上门禁的真实目录、GBBQ/QFQ 和生产装配。接口/fixture 测试不能填补来源证据。

## 本轮官方公开文档复核（2026-09-12）

只读取网页，不运行 SDK、不下载新行情、不登录或联系厂商。
- [get_divid_factors](https://help.tdx.com.cn/quant/docs/markdown/mindoc-1ctuhthaq5qmg/mindoc-1h10hsiat36k4.html)：字段和示例不定义 GBBQ 存储映射/scale 或参考价舍入顺序，未补齐 #54。
- [个人行情下载](https://www.tdx.com.cn/article/vipdata.html)：描述个人 PC 软件盘后下载及混合证券包，不能证明本项目服务器范围或 A 股目录契约。
- [全品类数据库](https://www.tdx.com.cn/tdxdata.html)：介绍面向客户的数据库和文件包推送；没有将该产品合同范围授予当前公开 base/hsjday/gbbq 使用，不能作为许可替代。
这是有界文档检查，不声称所有官方资料均不存在所需契约。旧 evidence 保持原样。

## 最小实施与验收计划（尚未执行）

A. 在 #54/#71/#46 补齐上述证据；跨文件一致性须有独立契约后才能组合。证据不足继续 blocked。
B. 契约成立后实现独立 Rust 目录/GBBQ 解析及 normalization/QFQ；保留未解释字段、拒绝不支持事件，先测试再接线。不得引入 tolerance、逐股规则或多源 fallback。
C. 文件验证与计算在发布事务外完成，产出固定 provenance；应用层判定 publish intent，SQLite Adapter 只提交 head/revision/occupancy 条件事务。
D. 有界真实验收固定 #53 四证券作为研究回归样本，不重选；先确认其身份和文件覆盖。QFQ 独立验证通过后发布 daily，再由同一版本派生 yearly；historical/current head 按 #79 原规则。
E. 用发布版本 ID 贯穿 query→availability→consecutive_direction；完整性证据不足返回 unknown/not_computable，不用 year_unfinished=false 代替年度完整。
F. ChatGPT 侧实际调用并保存参数、版本、结果、来源时间/摘要及失败记录；到此才能验收真实闭环，未覆盖的年度/证券不得扩大宣称。

当前不能进入 B–F 的生产路径；不以用户开发授权代替来源语义或第三方使用范围证据。
更多指标在闭环之后。NOT_PRODUCTION_READY，live market source NOT_RUN。

## 能力发现修复

公开 run_worker_once 纳入同一 registry，保留旧 {} 输入和 idle/task_id/status 输出。
详情说明同步任务/占用副作用和只拒绝来源的 Stage A 开发边界；不成为新生产执行器。
真实 stdio 测试比较工具名称集合、详情 schema，验证空队列、额外字段拒绝和执行后 failed。
受限应用上下文继续不发现/不能调用控制工具。远程工作区身份授权不由本修复新增。
