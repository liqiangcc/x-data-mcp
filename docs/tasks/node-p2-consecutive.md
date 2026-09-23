# Node 迁移 P2 — consecutive_direction 纯算法

任务：NM-030（agent-coordination/node-migration）。日期：2026-09-23。
基线：`1a0737be7202e00e6648979f89e58fa0d4d8a85d`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；#81 裁决 5645740750 / 实现 5645809215；`src/metrics/consecutive.rs`、`wire.rs`、`tests/consecutive.rs`。
证据：`evidence/node-migration/p2/consecutive/`（probe-results.jsonl 67 行全 PASS、rust-oracle.jsonl、negative-selftest.jsonl、results.json、SHA256SUMS、README）。

**结果：67/67 PASS / 0 FAIL —— 48 例 live Rust oracle 全字段一致（含全部 INVALID_MARKET_COMMAND 文本与 ratio_change 字符串）。**

## 1. 移植：`src_node/algorithms/consecutive.mjs`

- `evaluate(input, range, params)`：预算门（年域 1..9999、<1000 年窗、≤1000 obs、min_periods 1..=1000）→ 排序/重复/出界/日期年份一致 → missing_year → completed_only 尾摘（excluded_tail）→ window_too_short → provisional → 反向遍历（b-then-a check 序：nonpositive/unknown/incomplete）→ exact/lower_bound → matches true/false/null → anchor/first_change/cumulative_change → actual_input_range/observations_used/limitations 全字段
- `ratioChange(end, anchor)`：系数按 scale 对齐的 BigInt 有理数，×10^12，512 位预算，商+一次 half-even，负号仅在非零，padStart(13) 定小数点
- Decimal 复用 `_dec`（qfq_affine 已验收引擎）：from_str 半进位>28dp、MAX96 界、Display 逐字、scale 对齐精确比较
- 类型边界：`ConsecutiveInputError`——NaiveDate 形状/日历、year 整数域、completion/direction/参数默认与定值、Decimal 形状/溢出/孤立代理（19 例）

## 2. 纯净性

确定性、只读；无 SQLite/已验证读适配器/服务/调度/网络/发布/代际/部署；完成性证据不由 MCP 调用方自报。

## 3. 验证矩阵（67 例）

| 组 | 例数 | 结果 |
|---|---|---|
| evaluate oracle（方向/断点/门槛/尾部/缺失/完成态/非正价/错误/真实日期） | 33 | PASS |
| ratio_change oracle（half-even 正负/循环/前导尾随零/高精度/MAX/错误） | 15 | PASS |
| 类型边界 | 19 | PASS |
| 自测 `consecutive` | exit 1 | |

## 4. 范围边界

不含存储核验、服务/MCP、调度、发布、部署、生产就绪断言。

## 5. 验证命令

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p2/consecutive_probe.mjs .` | 0（67 PASS） |
| `node ... consecutive`（自测负证） | 1 |
| `node scripts/node_migration_p2/qfq_affine_probe.mjs .`（_dec 回归） | 0（37 PASS） |
| `rustc` cons_oracle | 0 |
| `sha256sum -c` / `git diff --check` | 0 |
