# Node 迁移 P2 — yearly 自然年聚合

任务：NM-019（agent-coordination/node-migration）。日期：2026-09-23。
基线：`47d9efeac96e91093a283cf6f39b86e48d84e77e`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271。来源：`src/yearly.rs::aggregate` + `sum_known`。上游：NM-013 Dec 引擎。
证据：`evidence/node-migration/p2/yearly/`（probe-results.jsonl 26 行全 PASS、rust-oracle.jsonl、results.json、SHA256SUMS、README）。

**结果（R2）：61/61 PASS / 0 FAIL —— 5 个 P0 golden + 20 个 Rust 实测边界 + 35 个类型边界回归（R1+R2 修复后）。**

## 1. 移植：`src_node/algorithms/yearly.mjs`

| 语义 | 实现 |
|---|---|
| 校验顺序 | identity/period/evidence → empty input/selection → per-bar 扫描 → missing-year → per-year 求和 |
| per-bar 扫描 | `date>as_of`、非严格递增、OHLC 边界（open/close ∈ [low,high]）、负 volume/amount → `invalid bar or date order` |
| 自然年分组 | 连续区间（输入已排序） |
| missing year | 所选年升序逐一检查，报**第一个**缺失 → `requested natural year is absent: {y}` |
| 聚合 | first-open/max-high/min-low/last-close；date=实际最后交易日；`year_unfinished = year==as_of.year` |
| `sum_known` | 单位未确认或任一 None → 整字段未知（null），**先于**溢出；`checked_add` 链 + 减法往返验证（`next-sum==value && next-value==sum`）→ `InexactSum`，绝不静默 rescale |
| lineage | `{base_daily_version_id, "local-natural-year-v1", first/last_trading_date}` |
| 输出 | `{symbol, source, base_daily_version_id, as_of, unit_currency, records}` 对齐 `AnnualSeries` |
| 错误文案 | `invalid fixed QFQ daily input: {reason}` / `requested natural year is absent: {y}` / `exact decimal sum is not representable` 逐字 |

## 2. 验证矩阵（26 例）

| 组 | 例数 | 结果 |
|---|---|---|
| golden | 5（two_years、missing_year、None 传播、MAX×4 溢出、乱序拒） | PASS |
| rust_oracle | 20（selected year/both、空选择、as_of 在 bar 前/中、adjustment unknown/非qfq、volume/amount 单位未知、open>high、负 volume/amount、重复日期、空记录、**精确 MAX 和**、精度损失、blank version_id、already-yearly） | PASS |
| 补充 | 1（0.05+0.05 精确和，验证无 Number 路径） | PASS |
| typed_boundary（R1+R2） | 35（asOf junk/Feb-30/月13/日0/年0/残缺/数字/null；bar 日期同类；years 浮点/字符串/NaN/Inf/>i32/非数组；daily null/records 非数组/Dec 字段畸形/scale>28/volume undefined；边界先于 Rust 层；i32 边缘年仍达 MissingYear；R2: ±2^96 mantissa 拒收、±MAX96 精确界接受并聚合） | PASS |

## 3. 范围边界

- 无 `aggregate_key`/memoization、DB/服务接线、全市场生成。
- 证据模型为 `{confirmed, value}` 形状（镜像 `EvidenceValue::is_confirmed`），已记录。
- NM-013 算术零改动。

## 4. 验证记录

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p2/yearly_probe.mjs .` | 0（61 PASS，R2） |
| `rustc` oracle（`target/debug/yearly_oracle`） | 0 |
| `json.tool` / `sha256sum -c` / `git diff --check` | 0 |

## 5. 范围声明

仅新增 `src_node/algorithms/yearly.mjs`、`scripts/node_migration_p2/yearly_probe.mjs`、`docs/tasks/node-p2-yearly.md`、`evidence/node-migration/p2/yearly/`。无 Rust 改动、live DB、ECS、deploy/push/merge。
