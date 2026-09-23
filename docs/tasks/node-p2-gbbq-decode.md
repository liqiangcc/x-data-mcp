# Node 迁移 P2 — GBBQ 记录解密与事件解码

任务：NM-016（agent-coordination/node-migration）。日期：2026-09-23。
基线：`a7c75a19eb4bd9c5de9d2df3f222b73d2a0662d7`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271。来源：`src/gbbq.rs::{decrypt_record,event_from_clear,decode_record,decode_events}`。
证据：`evidence/node-migration/p2/gbbq_decode/`（probe-results.jsonl 19 行全 PASS、rust-oracle.jsonl、results.json、SHA256SUMS、README）。

**结果：19/19 PASS / 0 FAIL —— 含 retained 全量 205,776 条事件解码摘要与 Rust 逐位一致。**

## 1. 移植：`src_node/parsers/gbbq_decode.mjs`（纯 JS，零依赖）

| 语义 | 实现 |
|---|---|
| 4176B 固定 key table | 从 `src/gbbq.rs::KEY_TABLE_HEX` 逐字节提取内嵌（模板串 `.replace(/\s/g,"")`，字节数校验） |
| `decrypt_block` Feistel | j=0x40..4 step −4；u32 LE；`+`/`|` 经 `>>>0` 保持无符号；key 偏移 0x448/0x48/0x848/0xC48/j；末 XOR key[0..4] |
| 字节 24..29 原文拷贝 | `clear.set(record.subarray(24,29),24)` |
| `event_from_clear` | market=b0；code=UTF-8(b1..8) 去尾随 NUL（`TextDecoder` fatal 严格校验）；date_raw=u32le(8..12)；category=b12；c_raw=f32le(13..29) |
| 无损位保存 | `c_raw_bits`（u32）与 widened f64 `c_raw` 并列暴露 |
| 无效 code 字节 → `InvalidCode` | `GBBQ record {i} code bytes are not UTF-8: [b0, b1, ...]` Rust debug 列表逐字 |
| 类型边界 | 仅 29B `Uint8Array`；错误长度/类型 → `invalid_input`（不做 coercion） |

## 2. 验证矩阵（19 例）

| 组 | 例数 | 结果 |
|---|---|---|
| synthetic encrypt→decode | 4（合法记录、全 0xFF code、code 尾部非 UTF-8、空 code） | PASS（解密字节 hex + 事件字段 + 错误文案逐字） |
| member 样本事件 | 7（idx 0,1,2,100,1000,100000,205775 全字段含 c_raw_bits） | PASS |
| member 全量摘要+直方图 | 2（sha256 `a33ff825…` + 15 类计数全等） | PASS |
| 类型边界 | 6（28/30B 记录、字符串、Array、null、全零记录解密拒绝） | PASS |

## 3. 摘要序列化（契约记录）

每事件一行 `i,market,code,date_raw,category,<4×8hex c_raw_bits>`，按记录序拼接后 sha256。Node 与 Rust 输出 `a33ff825b118de4b7dc6ade4889443db411bef54c01f25b29fdbc5b720ff1adc`。

## 4. 范围边界

- 不做 category 业务解释（未知类别不重解释为业务事件）、QFQ 集成、event-day marks、服务接线。
- 全零记录解密后 code 非 UTF-8 → `invalid_code` 拒绝（符合设计，已记录）。

## 5. 验证记录

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p2/gbbq_decode_probe.mjs .` | 0（19 PASS） |
| `rustc` oracle（`target/debug/gbbq_decode_oracle`） | 0 |
| `json.tool` / `sha256sum -c` / `git diff --check` | 0 |

## 6. 范围声明

仅新增 `src_node/parsers/gbbq_decode.mjs`、`scripts/node_migration_p2/gbbq_decode_probe.mjs`、`docs/tasks/node-p2-gbbq-decode.md`、`evidence/node-migration/p2/gbbq_decode/`。无 Rust 改动、live DB、ECS、deploy/push/merge。retained 原始文件与大体积解码输出不入库。
