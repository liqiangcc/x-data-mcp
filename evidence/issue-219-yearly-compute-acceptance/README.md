# Issue #219——yearly 派生 + compute_metrics 边界验收证据

## 结果:**14/14 PASS**(results.json)

在 update5219 验收库拷贝上,以 gen-2 inspected 快照 env 驱动真实
`mcp_data` stdio:单请求 `update_data/market.yearly`(6 显式 items,
`current_head`/`expected_yearly_head=null`)→ worker 排空 → 全量断言。

### 核心验证

- **派生正确性(Y4)**:163 条 yearly bar 与 `daily_bars` 的**独立 SQL
  重聚合**逐值相等(首 open/末 close/max high/min low/首末交易日),
  不采样。sz.000002 复核样例:2025 年 `(7.25, 8.22, 4.62, 4.65)`
  @2025-12-31。
- **lineage(Y5)**:`base_daily_version_id` 绑 item base,
  `algorithm_version=local-natural-year-v1`,`as_of=2026-09-18`
  (上海历),`is_finished=0` iff year==2026。
- **sz.000035 停牌缺口**:2012 全年无 daily bar(2011-11-08 →
  2013-09-17)→ 只请求 32 个实际存在年;该缺失如实保留,不是已证明
  gap。
- **消费面(Y6)**:yearly query/versions/availability 正常,
  `current_head` 产生 6 个 `period='year'` 新头,day 头 5219 不动。

### 关键发现——compute 正向路径架构性不可达(Y7)

`computed` 在生产源数据上**不可达 by design**:`Completion` 证据仅
来自 `analysis_coverage_v1`,该字段只由 fixture/demo 代码写入,且
`sqlite_metrics.rs` 硬拒非 fixture 源("fixture coverage cannot
certify a production source")。真实 yearly 版本每观测
`completion=Unknown` → 恒 `not_computable{unknown_completion}` +
limitation "completion evidence is fixture-only; no production
source approval"。

**含义**:compute_metrics 正向验收在真实数据上**无法完成**,直到
存在生产级完成度认证路径。本切片验证的是诚实拒绝边界 + 拒绝前的
检查次序(`non_positive_price` 先于完成度检查、range 末端
`missing_year` 先于观测检查、`excluded_tail` 语义保留、伪造
version/digest 显式拒绝)。这不是实现 bug——是尚未实现的生产认证
机制,如实记录为门禁事实而非 PASS。

### 工件

- `run_yearly_compute_acceptance.py`——runner
- `results.json`——14/14
- `run-log.jsonl`——全量请求/响应/断言
- `run-log-v1a.jsonl` / `results-v1a.json`——v1 运行史(Y4 解包 bug
  崩溃 + compute 期望按"可计算"写的 10/13;二者都是断言侧修正,
  派生结果两次完全一致)
- 运行库:`/home/box/acceptance-run-yearly/db.sqlite`(拷贝,
  源库未动)

### 边界(如实)

- 证明的是"年聚合管线对已发布数据按契约正确 + 消费/计算边界诚实",
  **不构成** QFQ 业务正确性(#54 未退休)、不构成 production_ready;
- 负年收(sz.000002 早期)是前复权链的如实输出,不修补;
- env-absent 腿 verbatim `official-file source not installed`。
