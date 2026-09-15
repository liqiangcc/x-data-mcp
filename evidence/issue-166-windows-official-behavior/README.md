# issue-166-windows-official-behavior

Windows 实机官方客户端行为验证，为 #46/#54 的 `BLOCKED_EXTERNAL_EVIDENCE` 补齐外部证据入口。
机械证据，不声称契约；结论状态以 result.json 为准。

## 交付物

- `plan.json` — 观察任何客户端行为输出前冻结的计划（样本、方法、绑定规则、状态集）
- `environment.json` — Windows / 客户端 / 安装包 / TQ 组件事实
- `inputs.json` — 官方数据文件快照 hash（含安装包、hq_cache、vipdoc .day、TdxW.exe）
- `raw/` — 非敏感原始材料
  - `raw/files/` — 客户端下载的官方数据文件副本（.day ×4, tdxhy.cfg, gbbq.map, tdxzs3.cfg, incon.dat）
  - `raw/files_manifest.json` — 全部快照文件 sha256（含未复制的 gbbq/base.dbf/zhb.zip/TdxW.exe 等）
  - `raw/observations.json` — 官方显示逐字段转录（每条绑定截图 sha256）
  - `raw/screenshots/` — 29 张带 sha256 manifest 的截图（大帧经 1600px/JPEG 处理；原帧 sha256 保留在 manifest）
- `scripts/verify_issue166.py` — 确定性重放：文件 hash → .day 记录 ↔ 显示值 → 边界参考价区分 → tdxhy/incon 绑定 → 000002 复权链对比
- `determinism.json` — 重放结果（19/19 通过）
- `result.json` — 机器可复核对比结果与终端状态

## 采集方法

- 官方客户端：通达信金融终端 V7.73，`C:\new_tdx64\TdxW.exe`，安装包 `new_tdx64.exe` sha256 已绑定（2026-09-15 自 data.tdx.com.cn 下载，ETag/Last-Modified 见 inputs.json）
- 行情侧：K 线十字光标数据窗（前复权/不复权切换）、训练模式（功能码 47，截止日跳转触发全历史 .day 下载）
- F10 侧：图文F10 分红转增/配股/公司资料页
- 行业侧：Ctrl+R 所属板块、F10 公司资料（中上协/研究行业双栏）

## 关键结论摘要

### #54（GBBQ 语义绑定）

- **C1 = 每10股派现（元，含税）**：600006「10派0.05元(含税)」@2025-07-17；600004「10派2.85元(含税)」@2026-07-24
- **C2 = 配股价（元）、C4 = 每10股配股数**：000002「配股价格7.50元 / 每10股配股2.73」@2000-01-10（存储 2.727 比显示更精确）
- **事件日昨结 = 分位取整后的除权参考价**：两处半分边界（7.245→7.25、7.675→7.68）复现；600006 边界同时排除 exact-binary f32 输入链（会得到 7.24/+1.04%，实测 7.25/+0.97%）
- **K线前复权 = 仿射链** `T_i(x)=(x-C1/10+C4·C2/10)/((10+C3+C4)/10)`：000002 事件窗两根 K 线 8/8 字段复现（含负价格 -8.06，证明减法成分）；逐事件分位取整变体预测 -8.08/-8.29 被排除（中间参考价不取整）；**输入层判别：2dp 归一化业务值 8/8 命中，exact-binary f32 7/8 失配**

### #46（行业目录绑定）

- `tdxhy.cfg` = `f0|code|T-code|f3|f4|X-code`：**f0 = 交易所枚举 {0:SZ, 1:SH, 2:BJ}**（5659 行零例外）；**f2 → 通达信行业（incon.dat 命名）**；**f5 → 通达信研究行业**；f3/f4 全空
- 显示绑定：F10「研究行业: L1—L2」= X 码前两级；所属板块行业叶 = X 码 L2-L3；6 个固定样本全部唯一命中
- 范围：沪深京上市证券 + B 股（B 股行存在但 T/X 为空）；**退市证券缺席**（000004 无行，身份降级为 T000004 扩展市场）
- 裸代码歧义：`000004` → 工业指数（退市股降级）；`000001` → 平安银行（上市股优先）

### 终态

- `#54`: **#54_BEHAVIOR_BINDING_VERIFIED**（C3 仅经链式拟合间接行使；本地 gbbq 为新混淆编码、未解码——绑定建立在官方显示与已提交 gen-2 记录之间）
- `#46`: **#46_FILE_FIELD_BINDING_VERIFIED**（f3/f4 未绑定即恒空；B 股 T/X 恒空；范围限定上市沪深京证券）

供协调者/Final Gate 裁决，本任务不改生产语义。
