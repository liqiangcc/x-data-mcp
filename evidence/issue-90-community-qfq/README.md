# 开源 GBBQ/QFQ 固定对比（#90，纯 evidence）

结论：COMMUNITY_MODELS_DIFFER，官方 normalization/QFQ 仍 NOT_VERIFIED。
本目录不接入 Cargo 或生产 runtime，不部署、不修改既有失败证据。

## 冻结与输入

事前冻结 commit `b28ec5a`：plan.json、sources.json 及许可证/源码快照先提交，再计算 result.json。
两个上游固定提交：
- injoyai/tdx `2b3dcae30c42cae1f5e2f3a33359d12b761ae7fe`。
- yutiansut/QUANTAXIS `5006a009fba7b8e392e32f8b577f1bcb81deba5c`。
完整路径、固定源码链接、sha256、长度在 sources.json；均 MIT，许可证随快照保留。
软件许可证不是官方行情文件获取/使用许可，不解除 #71。

复用 #54 inputs.json 全部九事件、prior_bar 原始字节和旧 matrix；#53 provenance 固定官方档案/member 及社区 decoder 身份。
本次核验这些 retained JSON 和上游源码快照的摘要，不重新下载或重验整个官方档案，不冒充官方解码契约。
没有重做 #53 selection、四股 BaoStock 查询或停牌占位诊断。

## 比较内容与结果

injoyai 的 Gbbq.XRXD 将四字段 float32 提升到 float64 后各按两位小数舍入。
Factors 路径是仿射 A*raw+B，累计过程中使用 float64，最终价格舍入到分。
其 Pre 路径另转为厘价格整数，不能把 Pre 的截断行为误认为 Factors 的每事件计算步骤。
QUANTAXIS _QA_data_stock_to_fq 则以未舍入参考价/昨收构造累计比例，乘 OHLC；该函数没有字段规范化或最终分位舍入。
其上游数据库输入如何规范化，本次未验证；实验明确用已有原始 float32 值作为输入，不能推断所有 QUANTAXIS 用户的实际输出。

result.json 是九个独立事件应用到各自前一 bar OHLC 的 scalar adaptation，不是完整 Go/pandas 项目运行，也不是累计多年 QFQ。
Python binary64 保留所审查单事件公式的操作顺序；Go math.Round 以输入 binary64 的整数比实现 ties-away，避免用 floor(x+0.5) 错算相邻浮点数。
输出 17 位有效数字保留二进制差异；例如 7.2699999999999996 表示 binary64 的 7.27，不是声称业务输出需要这么多位。

- sh.600006 2025-07-17：injoyai scalar 输出前一 close 7.25；QUANTAXIS 原始字段 scalar 得到约 7.2449999999254944，函数本身不做最终分位舍入；旧 #54 frozen_repr 参考价分位为 7.24。
  只能说明步骤可产生不同结果，不能据此宣称解释了 #53 的全部 129 个历史日期或选定官方规则。
- sz.000002 2000-01-10：C4 原始 float32 对应约 2.727；injoyai 两位小数变为 2.73，重编码不能返回原 bits。
  这是信息损失证据，不单独证明某种业务规则错误；需要独立业务精度证据。
- 即便没有 float32 尾差，仿射与比例也不是同一模型：人工控制例昨收 10、每十股现金 1、另一 raw 20，仿射为 19.9，比例为 19.8。
  因此不能把模型差异全部归为 normalization；这个控制例是算法区分测试，不是新证券样本或官方价格。

## 上游证据审查

injoyai 测试注释称预期值为人工核对的通达信桌面显示值，保留了硬编码 bars/events 和已知不一致点。
但本次所检查文件没有附带官方客户端版本、原始导出/截图的摘要、取得时点或可复核采集记录。
buildXRXDs 直接从业务 float 字段构造 XRXD，绕过 Gbbq.XRXD；所以该测试不能验证 binary32→两位十进制规则。
KnownException 测试断言算法 -260.97，并记录其与声称的桌面 -260.98 不同，不能写成“所有官方点通过”。
QUANTAXIS 所检查的计算文件没有附官方目标样本；这是文件范围判断，不声称整个项目没有其他证据。

有价值的下一证据是：同一 raw/event 集对应的官方导出，明确客户端版本、前复权设置、锚点/区间、时间、价格精度和逐字段映射。
应分别判别 normalization、仿射/比例、事件位置/未来事件及显示舍入；不能只挑一个匹配结果倒推全部内部语义。
未获得这些证据前不修改 #54 冻结规则，不修改生产 QFQ，不新增来源 fallback。

## 复现与验证边界

```
python3 -B evidence/issue-90-community-qfq/compare.py --check
python3 -B -m unittest discover -s evidence/issue-90-community-qfq -p 'test_*.py'
```

五个测试验证固定输入/结果、ties 与邻近浮点数、模型区别、半分/配股信息损失、摘要篡改拒绝。
两次独立 clean checkout 结果与 SHA 见 determinism.json；Actions 用同一入口运行。
上游 Go/pandas tests NOT_RUN；#53 full four-stock revalidation NOT_RUN；live source NOT_RUN。
离线诊断 PASS 只证明本实验可复现，不代表 official semantics、生产算法或许可 PASS。
