# Node 迁移 P2 — GBBQ 容器结构解析器

任务：NM-015（agent-coordination/node-migration）。日期：2026-09-23。
基线：`ed61961ef57341a3fa6f34f8f271a954d747100e`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271。来源：`src/gbbq.rs::parse` + `RawFile`。
证据：`evidence/node-migration/p2/gbbq_file/`（probe-results.jsonl 21 行全 PASS、rust-oracle.jsonl、results.json、SHA256SUMS、README）。

**结果：21/21 PASS / 0 FAIL —— 全部 Rust oracle 错误文案逐字一致 + retained 官方文件实测。**

## 1. 移植：`src_node/parsers/gbbq_file.mjs`（纯 JS，零依赖）

| 语义 | 实现 |
|---|---|
| 类型化输入边界（REVIEW-NM-014 条件） | 仅 `Buffer`/`Uint8Array`；string/Array/number/null/object → `invalid_input` 拒绝，不做 coercion |
| 头 <4 字节 → `HeaderTruncated` | `GBBQ file is shorter than its count header: N bytes` 逐字 |
| `count` = u32 LE | `DataView.getUint32(0, true)` |
| `expected = count*29+4` 检查先于分配 | `Number.isSafeInteger` 护栏（count≤2³²−1 → 数学上不可达溢出，保留等价分支） |
| 长度不符 → `LengthMismatch` | `GBBQ length {actual} does not equal 4 + count({count}) * 29 = {expected}` 逐字 |
| 记录逐字节保存 | `new Uint8Array(subarray)` 每记录独立拷贝（`Buffer.slice` 会别名共享缓冲，不符合 Rust `to_vec` 所有权语义） |
| 声明巨大 count 不产生大分配 | 长度校验在记录切片之前；实测 heap 增长 <16MB |

## 2. 验证矩阵（21 例）

| 组 | 例数 | 结果 |
|---|---|---|
| rust_oracle | 14（头截断 0–3、空容器、1/3 条记录、body 短/溢、count2 body1、count0 带 body、0xFFFFFFFF、0x80000000、逐字节保存） | PASS 文案逐字 |
| 类型拒绝 | 5（string/Array/number/null/object） | PASS |
| 分配安全 | 1（count=2³²−1 时 heap 增长 <16MB） | PASS |
| retained 实测 | 1（`official-gbbq-member` 5,966,087B → 205,776 条，sha256 与 MANIFEST 一致） | PASS |

## 3. 范围边界

- 只做容器形状校验 + 加密记录逐字节保存；`decrypt_record`/`decode_events`、category/factor 解释属后续切片。
- `RecordCountOverflow` 分支为等价保留：count≤2³²−1 时 expected≤124,554,051,559 在安全整数内，实际不可达。

## 4. 验证记录

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p2/gbbq_file_probe.mjs .` | 0（21 PASS） |
| `rustc` oracle（`target/debug/gbbq_parse_oracle`） | 0 |
| `json.tool` / `sha256sum -c` / `git diff --check` | 0 |

## 5. 范围声明

仅新增 `src_node/parsers/gbbq_file.mjs`、`scripts/node_migration_p2/gbbq_file_probe.mjs`、`docs/tasks/node-p2-gbbq-file.md`、`evidence/node-migration/p2/gbbq_file/`。无 Rust 改动、无解密逻辑、live DB、ECS、deploy/push/merge。
