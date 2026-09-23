# Node 迁移 P2 — `.day` 结构解析器纵向切片

任务：NM-011（agent-coordination/node-migration）。日期：2026-09-23。
基线：`88bc6ebe5068d6d414587cddb5937b4f1edc5445`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271。上游 oracle：P0 `day_parse` golden 向量（4 行已验收）。
证据：`evidence/node-migration/p2/day/`（probe-results.jsonl 13 行全 PASS、results.json、SHA256SUMS、README）。

**结果：13/13 PASS / 0 FAIL —— 与已验收 golden 零失配，首个 Node 运行时模块落地。**

## 1. 移植：`src_node/parsers/day_file.mjs`（纯 JS，零依赖）

Rust `src/day_file.rs::parse` 的逐语义镜像：

| 语义 | 实现 |
|---|---|
| `RECORD_BYTES = 32` | `export const RECORD_BYTES = 32` |
| 字节数 %32 先检查 | 抛 `DayFileError("day file byte count N is not divisible by 32", "truncated")` |
| word0 = YYYYMMDD u32，`year = raw/10000` 限 `1..=9999` | `Math.floor(raw/10000)` 同样的 u32 语义 |
| `NaiveDate::from_ymd_opt` 严格公历 | `setUTCFullYear` + 字段回读（年 <100 防 1900 偏移；溢出归零校验） |
| `prev.date >= date` 拒绝（含相等） | `day <= prevDay` 比较 epoch day |
| 8×u32 LE 原始字段 | `DataView.getUint32(off, true)`，f32 位、r7 等原样保留 |
| 错误文案 | 与 Rust enum variant 逐字一致；`.kind` 供程序判别 |

## 2. 验证矩阵（13 例）

| 组 | 例 | 状态 |
|---|---|---|
| golden | two_valid_records（2 条记录全字段值）、truncated_31_bytes、invalid_month_13、non_increasing_date | 4/4 PASS |
| 边界补充 | year 0 拒、相等日期拒、Feb 30 拒、Feb 29（闰年）放行、空文件 → []、33 字节截断优先、day 0 拒、month 0 拒、`kind` 判别 | 9/9 PASS |

## 3. 范围边界（诚实声明）

- 本模块只做**结构层**：帧切分 + 原始 u32 字段 + 日期合法性与顺序。f32→十进制投影是 NM-007 已证 codec 层；QFQ/yearly 属后续切片。
- 记录返回 ISO 日期串 + 原始 u32；与 Rust `RawDayRecord` serde 形状字段一致（键序按 BTreeMap 排序，与 golden 字节级比较通过）。

## 4. 验证记录

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p2/day_probe.mjs .` | 0（13 PASS） |
| `json.tool` / `sha256sum -c` / `git diff --check` | 0 |

## 5. 范围声明

仅新增 `src_node/parsers/day_file.mjs`、`scripts/node_migration_p2/day_probe.mjs`、`docs/tasks/node-p2-day-parser.md`、`evidence/node-migration/p2/day/`。无 Rust 改动、live DB、ECS、deploy/push/merge。
