# M1 研究数据底座：发布说明草稿

关联 Issue：[#383](https://github.com/liqiangcc/x-data-mcp/issues/383)。
**状态：草稿，未发布。** 部署时用作 GitHub Release 正文与 [STATUS](../STATUS.md) 当前状态区的
更新依据；部署前不要当已发布事实引用。设计与实现见 [M1 设计](issue-383-m1-research-data.md)、
[实现记录](issue-383-m1-implementation.md)、[部署清单](issue-383-m1-deploy-checklist.md)。

## 一句话

`market.daily` 的前复权价从 **2 位小数**改成**最多 6 位小数**（精确有理数链 `qfq-exact-v1`），
同一实体同一周期新增一条**不复权**序列；全部序列的 `version_id` 因摘要格式升级与全新重建而变化。
工具数、dataset、wire 字段与错误码不变，`capability_revision` 不升版（D-32）。

## 变了什么

1. **前复权价精度：2dp → 最多 6dp。** 值仍是十进制字符串，形状不变；整分值不再补尾随零
   （`11.75` 而不是 `11.750000`）。在"恰好落在 2dp 半分位"的 bar 上，新值与旧 2dp 会差 1 分
   （实测样本 `sh.600580`：476/23,104 个字段 ≈ 2.06%，见 [M1 设计](issue-383-m1-research-data.md) §4.3）。
   这不是回归：旧 2dp 的舍入方向由中间精度决定，本身不可跨实现复现。
2. **全部 `version_id` 变化。** 摘要格式升到 `series-fold-v2`（口径/品种/精度进种子），并且按
   D-08 归档旧库 + 全新 init 重建，所以每个序列的 `version_id` 都会变。`stale_version` 语义不变：
   旧 id 曾存在但已被取代 → 重新列版本。
3. **同一实体多一条不复权序列（`adjustment=0`）。** 它只在研究快照导出里可见，`query_data`
   仍然只返回前复权（AC-M1-10）。
4. **日更多 10 个服务器内部 item**（9 个指数基准 + 1 个交易日历）。它们复用现有任务/进度面，
   `entity`/`period` 为 null，wire 形状不变（[M1 设计](issue-383-m1-research-data.md) §9.2）。
5. **研究快照导出（可选 feature）。** `export_research` bin 产出 5 个 Parquet 文件 + `manifest.json`，
   默认落 `/root/x-data-exports/`。**发布包暂不含该命令**（`scripts/build_release.sh` 未加
   `--features export`，需 owner 批准），MCP 也没有 `export_data`（D-39）。

## 为什么

- **2dp 在原理上不可跨实现复现**：约 8.44% 的价格字段恰好落在 2dp 半分位（`sh.600580` 样本
  1,951/23,104），舍入方向取决于中间精度。6dp 下该样本半分位为 0，且"28 位近似"与"精确"无差异
  （[M1 设计](issue-383-m1-research-data.md) §4.3）。
- **低价段误差不可接受**：全库 12.6% 的 bar 前复权收盘价 < 2 元，该区间日收益误差中位
  0.42–2.9pp（[数据目录](../data-catalog.md) §7 的复核值）。
- **6dp 仍不够就再升**：精度是序列属性（`price_scale`），以后换精度只改序列声明与派生算法版本，
  不改 wire。

## 使用者要做什么

- **解析**：按十进制字符串解析，不要假定 2 位小数，也不要按固定宽度格式化。服务端已按"最多 6 位"
  输出（整分值不带尾随零）。
- **按时点复权**：前复权序列以快照最后一条事件为基准、**含未来事件**，不适合直接做回测输入；
  按时点复权要用不复权 + 因子事件按 `effective_day ≤ T` 自算（导出的 `daily_raw` + `factors`）。
  因子的知晓时间未知（`known_at` 恒 NULL），这一点写在导出清单的 `limitations` 里。
- **下游 `x` 仓库**：**可以直接兼容**——只读核对（2026-10-26，另一仓库，未改任何文件）确认
  `KlineReader` 把价格按 `Number()` 解析、没有任何 2 位小数假设，`version_id` 既不 pin、也不缓存、
  不持久化；`listTools()` 从未被调用，只依赖 `query_data`。`docs/decisions.md` 的 `P-02`
  （版本 pin 改为 `latest_per_entity`）因此已撤销。
  **但数值结果会变**：所有用到价格的计算（回撤事件边界、BOLL、区间收益率、模拟触发日与成交价）
  都会因为价格粒度从 1 分变成 1e-6 元而变化。这是精度变化的必然结果，不是兼容性问题。
  建议 `x` 仓库在文档与 `market_get_kline` 的工具描述里补一句精度说明——这只是建议，
  `x` 仓库的改动需要另行批准，本仓库不做。
- **部署前必须确认 `x` 仓库的 `KlineReader` 兼容性**（D-32 的部署阻塞项）：核对两个上游契约点
  ——`query_data` 的日线读路径仍只解析 `adjustment=1`；清单的 `metadata.adjustment.value` 仍是
  字面 `"qfq"`。方法见[部署清单](issue-383-m1-deploy-checklist.md) §5。

## 部署前置（都要 owner 批准）

1. 合并、打 tag、发布、部署（D-10 的同类边界）。
2. 下游 `x` 仓库 `KlineReader` 的兼容性确认（见上）。
3. `scripts/build_release.sh` 增加 `--features export`（若本次发布要带导出命令）。
4. 私有栈上的实测清单全部跑完并贴 Issue（[部署清单](issue-383-m1-deploy-checklist.md)）。
5. 部署方式按 D-08：归档旧库 + 全新 init 重建，不做原地迁移（D-33）；保留上一版 release 作回滚点。

## 回滚

- 回到上一版 release + 归档的 schema 21 库（D-08 标准做法）。导出产物是独立文件，不受影响。
- 只想撤掉导出功能：`export` 是可选 feature，发布包不带 `--features export` 即可，不影响库与日更。

## 明确不变

- 工具数量与入口：M1 本身不新增工具（data 23 / acquire 10）。若发布构建同时含 v8 的入口拆分，
  工具面变化以 v8 的发布说明为准。
- dataset 枚举、wire 字段、错误码、幂等/取消/版本语义。
- `capability_revision` 仍是 `generic-data-v7`（D-32：精度变化不算契约升版）。
- 不复权、因子、日历、指数**不通过 `query_data` 暴露**（AC-M1-10）；它们只出现在导出里。

## 未验证（发布时如实保留）

- 真实全量导出的耗时与最终体积（夹具实测约 1.5GB，设计 §7.7 已修正；全量仍未跑）。
- 全市场 6dp 复权的耗时与峰值内存、schema 22 下的冷启动、write-loop 与部署体积。
- 指数历史早期 `volume` 是否也是手。
