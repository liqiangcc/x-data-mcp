# Node 迁移 P2 — `.day` amount f32 投影 codec

任务：NM-012（agent-coordination/node-migration）。日期：2026-09-23。
基线：`b6e710febb780e4d982916de36d86e3c3215368f`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271。上游：P0 `amount_f32_projection` golden（6 行已验收）。
证据：`evidence/node-migration/p2/amount_f32/`（probe-results.jsonl 25 行全 PASS、results.json、SHA256SUMS、README）。

**结果：25/25 PASS / 0 FAIL —— 6 个 golden 字节级一致 + 12 个 Rust 实测边界 + 7 个 fail-closed 输入拒绝。**

## 1. 移植：`src_node/codecs/amount_f32.mjs`（纯 JS，零依赖）

镜像 Rust 表达式 `official_market.rs:396-398`：`Decimal::from_str(&f64::from(f32::from_bits(bits)).to_string()).ok()`。

| 语义层 | Node 实现 |
|---|---|
| u32 bits → f32 → f64 | `DataView.getFloat32`（精确提升） |
| Rust f64 Display 位置小数形式 | JS `String()` 数字序列相同；`|v|≥1e21` 或 `<1e-6` 时展开 e 记法为位置小数 |
| `Decimal::from_str` 约束 | scale>28 → 28 位 half-up 舍入；mantissa >2^96−1 → Err→null；`-0`→`0` |
| 非有限 | NaN/±inf → null |
| 契约输出 | `Decimal::to_string` 形式（scale 保留，underflow → `"0."+28 个 0`） |
| 输入校验 | 非 uint32 → `AmountF32Error` 抛出（fail-closed，不静默截断） |

**与朴素理解的两处关键差异（均由 live Rust 验证）：**
- `from_str` 对 scale>28 是**舍入**（half-up）而非拒绝：`5e-29 → 1e-28`，`1.5e-28 → 2e-28`；极小 f32 → Decimal(0, scale 28)。
- Rust f64 Display **永不**用指数记法；JS `String()` 会 → 必须展开。

## 2. 验证矩阵

| 组 | 内容 | 结果 |
|---|---|---|
| golden 6 | finite/负/NaN/inf 字节级一致 | PASS |
| Rust 边界 12 | f32max→null、min-denormal/min-normal→`0.000…0`（28位）、-0→`0`、1e20/8e20/1e29/1e-10/1e-5/1e8 展开、-inf/quiet-NaN→null | PASS（期望值由 rustc+真实 rust_decimal rlib 现场产出） |
| 输入校验 7 | -1、2³²、1.5、字符串、NaN、undefined、bool → `AmountF32Error` | PASS |

## 3. 范围边界

- 本模块只覆盖该投影表达式的 `from_str`/`to_string` 语义（f64 Display 域，≤17 位有效数字），**不是**通用 decimal 库；Decimal 算术属后续切片。
- 与结构解析器 `day_file.mjs` 解耦；无服务接线。

## 4. 验证记录

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p2/amount_f32_probe.mjs .` | 0（25 PASS） |
| Rust 边界校验（`target/debug/dec_check*`） | 0 |
| `json.tool` / `sha256sum -c` / `git diff --check` | 0 |

## 5. 范围声明

仅新增 `src_node/codecs/amount_f32.mjs`、`scripts/node_migration_p2/amount_f32_probe.mjs`、`docs/tasks/node-p2-amount-f32.md`、`evidence/node-migration/p2/amount_f32/`。无 Rust 改动、live DB、ECS、deploy/push/merge。
