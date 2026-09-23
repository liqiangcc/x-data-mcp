# Node 迁移 P2 — `infoharbor_ex.code` 严格 GB18030 解析

任务：NM-022（agent-coordination/node-migration）。日期：2026-09-23。
基线：`926a9068b9925262d9bca94dc8fb26a51408aed3`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；catalog contract。来源：`src/catalog_tdx/ex_code.rs::parse`。
证据：`evidence/node-migration/p2/ex_code/`（probe-results.jsonl 38 行全 PASS、rust-oracle.jsonl、results.json、SHA256SUMS、README）。

**结果：38/38 PASS / 0 FAIL —— 合成边界 + retained member 全行摘要逐位一致。**

## 1. 移植：`src_node/parsers/ex_code.mjs`

| 语义 | 实现 |
|---|---|
| GB18030 严格解码 | `TextDecoder('gb18030',{fatal:true})`，整缓冲先解码再切行（trail 字节可等于 `|`）；等价性已按 WHATWG 规范与 encoding_rs 对验证 |
| 行切分 | `\r\n`；末尾单空行容忍，中间空行拒；LF-only 单行**合法**（keywords 不解释，可含 `\n`）——Rust 实测 |
| 字段 | ≥3 个 `|` 字段（4+ 合法，keywords 可含 `|`） |
| code | 恰 6 位数字（前导零字符串保留） |
| name | 非空（GB18030 解码文本） |
| 重复 code | `code "…" appears more than once (lines A and B)` 拒收 |
| 错误优先级 | decode → 行（empty/fields）→ code → name → duplicate；文案逐字 |
| Debug 转义 | BadCode/DuplicateCode 值经 Rust `{:?}` 等价转义 |

## 2. GB18030 解码器等价（vs encoding_rs）

拒收一致：孤立 trail `0xbf`、截断 lead、`0xff`、`EF BB BF`、坏 4 字节 `95 32 82 FF`。解码一致：双字节/四字节序列——由 5,571 行 member 摘要一致端到端确认。

## 3. 验证矩阵（38 例）

| 组 | 例数 | 结果 |
|---|---|---|
| 合成 oracle | 28（空文件、单/多行、4+ 字段、无尾 CRLF、5 类解码拒收、LF-only、中/双空行、字段数、code 形态、空名、重复×2、decode 优先、4 例 `{:?}` 转义） | PASS 逐字 |
| 类型边界 | 3（string/null 拒 + Uint8Array 收） | PASS |
| retained | 7（MANIFEST sha256 + member 提取 + 摘要 + 4 采样行） | PASS |

## 4. retained member

`infoharbor_ex.code` 182,145B（sha256 `79eed0f6…`）自 MANIFEST 验证 zip 有界提取：5,571 行、行摘要 `124bc22d…` 与 Rust 一致。原始字节不入库。

## 5. 范围边界

不解释 keywords；无 catalog scope/compose、DB/服务接线。

## 6. 验证记录

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p2/ex_code_probe.mjs .` | 0（38 PASS） |
| `rustc` oracle（`target/debug/excode_oracle`） | 0 |
| `json.tool` / `sha256sum -c` / `git diff --check` | 0 |

## 7. 范围声明

仅新增 `src_node/parsers/ex_code.mjs`、`scripts/node_migration_p2/ex_code_probe.mjs`、`docs/tasks/node-p2-ex-code.md`、`evidence/node-migration/p2/ex_code/`。无 Rust 改动、live DB、ECS、deploy/push/merge。
