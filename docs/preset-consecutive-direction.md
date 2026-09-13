# 预制算法：连续涨跌

## 状态与目的

2026-09-12 用户讨论连续涨跌、连续下跌多年，并要求推进预制算法设计。
本算法作为 compute_metrics 的 metric=consecutive_direction，不新增 MCP 工具。
本文件为目标 v1 契约草案，尚未实现；参数默认值和边界为本次设计决定。

目标是判断截至指定时点的连续涨跌，不负责预测、买卖建议或筛选策略编排。
首个应用为同一已发布 QFQ yearly 版本的连续年度收盘价；daily 可复用比较算法，
但交易日相邻性与最终性证据须单独满足，不因数组有序就宣称连续交易日。

## 输入

沿用 compute_metrics 的固定版本、range、执行模式与契约。
metric 参数：

| 字段 | 类型 / 默认 | 含义 |
|---|---|---|
| direction | up 或 down，必填 | 要判断的方向 |
| min_periods | integer >=1，必填 | 至少连续多少次同方向价格变化 |
| price_field | const close | v1 仅比较相邻周期收盘价 |
| completed_only | boolean，默认 true | 仅已证实结束且满足最终性要求的周期 |
| equal_policy | const break | 相等打断连续性 |
| gap_policy | const reject | 必要输入连续性不可证实时不可计算 |

range.end 指定截止边界；不从系统今天隐式推断。
range.start 限定可读观察范围，不自动读取范围外前一条；如需更长窗口由 AI 显式扩大 range。
最终参与截止周期、剔除的尾部未结束周期与原因必须返回。
completed_only=false 可纳入已保存的未结束周期，标记 provisional，
结果不等同“已连续下跌 N 个完整年度”。

年份已过不充分证明年线完整：year_unfinished=false 不能代替覆盖证据。
例如只保留某年部分日线聚合得到的年线，不自动作为完整年末收盘价。
须核验年度结束/最后有效交易观察与来源覆盖；不具备证据返回不可计算。
先前日线最终性、版本损坏及单位/复权证据规则继续适用。

## 计算定义

对符合范围与完整性规则的序列 C[0..m] 按周期升序读取。
精确比较相邻 close；大于为 up，小于为 down，相等为 flat。
当前方向是最后一对有效相邻周期的方向；不足两条时不可计算。

从最后一对向前计数，遇到相反方向或 flat 即停止。
run_length 是指定 direction 的连续变化次数，不是价格观察值数量。
最后一对与指定方向不一致时 run_length=0。
连续下跌三年需要四个连续年度价格点，不是三根阴线。
不以 close 与本期 open 比较；连阴/连阳是另一个算法口径，v1 不混用。

返回目标方向 run 的锚点价格与末价格；例如 2021→2022→2023→2024 三次下降，
anchor_period=2021、first_change_period=2022、end_period=2024。
实际日期来自年线记录，不强行写成 12 月 31 日。

阈值判断满足 run_length>=min_periods。
累计变化为 C[end]/C[anchor]-1，仅计算本次 run；run_length=0 时该字段为 null。
算法要求参与价格为正；遇到非正值明确不支持该输入，不改变原始行情发布规则。
比较本身为精确十进制；累计比值使用输出 scale=12、half-even，
中间比值按十进制整数系数/尺度精确有理运算后一次舍入。
结果 precision 声明该字段已舍入，不能把 Decimal 默认除法冒充精确比值。

## 连续性与左边界

yearly 的周期标签必须逐年相邻；daily 根据明确的交易日/证券有效观察契约验证，
周末和已确认不适用日期不机械当缺口。停牌不能自动补前收盘价，
v1 在预期观察缺失且无允许跳过规则时拒绝确认连续性。

如果从末尾回溯已找到打断点，则末尾 run 精确确定，不要求扫描打断点以前的无关历史。
若同方向一直延伸至 range.start，则只能确认至少这么多期：
run_length_kind=lower_bound，不声称更早已经打断。
此时达到 min_periods 可返回 matches=true；未达到则 matches=null，并给出需要更多历史的原因，
不能返回 false。找到了打断点且不足阈值，才是 matches=false。
相关尾部中遇缺失或未知周期则 outcome=not_computable，
run_length/matches 为 null，不能跨过缺口继续数。

完成性筛选只允许剔除明确未结束的尾部周期；
中间未知或不完整周期不得删掉后把两边当相邻。
该工具不隐式下载日历、补行情或切换版本。

## 输出

- metric_id、name、algorithm_version、parameters 与输入 version_ref/digest。
- outcome：computed 或 not_computable；后者进入 compute_metrics 的失败结果契约，
  多指标严格模式不会把它作为成功指标值返回。
- observed_direction：up/down/flat，无法判断时 null。
- run_length：整数或 null；run_length_kind：exact/lower_bound 或 null。
- matches：true/false/null；null 说明不足以作出阈值结论。
- anchor_period、first_change_period、end_period，以及实际日期和 close。
- cumulative_change、precision；下界时仅表示已观察部分的累计变化。
- actual_input_range、excluded_tail、provisional、continuity_evidence、limitations。

lower_bound 是可计算的有界事实，不能在 AI 总结时省略“至少/范围内”限制。
latest 版本和实时刷新均不在算法内发生。

## 独立验收样例

下面价格均为同一版本的相邻完整年度观察；方向 down、min_periods=3。
这些是手算验收向量，不是生产测试结果。

| 序列 | 预期 |
|---|---|
| 100, 90, 80, 70 | run=3，lower_bound，matches=true，累计变化 -0.300000000000 |
| 100, 90, 80 | run=2，lower_bound，matches=null；需更早观察以判断是否达到三期 |
| 80, 100, 90, 80, 70 | run=3，exact，matches=true |
| 100, 90, 90, 80 | 持平打断，run=1，exact，matches=false |
| 100, 90, 95 | 末期上涨，down run=0，exact，matches=false |
| 2020:100, 2021:90, 2023:80 | 尾部年度缺失，not_computable，不跨 2022 计数 |
| 100, 90, 80, 70，末期未结束 | completed_only=true 排除末期，run=2 下界；false 可得 provisional run=3 |
| 少于两个有效周期 / 非正价格 | not_computable，明确原因 |

还需验证 up 的对称条件、小数精确相等、重复周期拒绝、排序/版本不匹配拒绝、
半偶舍入和溢出资源保护、尾部范围与年度完整性证据。
实现测试的期望来自这些定义和独立样例，不能调用实现本身生成期望。

## 与其他算法的组合

AI 可以先用 yearly 连续下跌筛选对象，再选固定 daily 版本计算布林线，
最后比较 close 与中轨并检查向上穿越。不同周期的版本关联应记录并核对，
不能把不同复权基准当作自然兼容，也不把组合当作无前视回测。

布林线、比较和穿越仍为后续候选，公式、标准差、舍入及计算结果的类型化引用尚需定稿。
现有 compute_metrics 只接受已发布输入版本；在未交付受验证的计算序列引用之前，
不能宣称服务端已能将布林结果直接传给 cross_series。
AI 可先读取明确结果进行外部分析，后续以独立契约扩展计算产物输入。
本指标本身不计算布林线、不筛选全目录、不触发采集。
