# Node 迁移 P2 — `tdxhy.cfg` 严格行解析

任务：NM-021（agent-coordination/node-migration）。日期：2026-09-23。
基线：`f02178e8625313ed65c4d11dfdb081c1b96bd1e2`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；catalog contract v1（#168）。来源：`src/catalog_tdx/tdxhy.rs::parse`。
证据：`evidence/node-migration/p2/tdxhy/`（probe-results.jsonl 47 行全 PASS、rust-oracle.jsonl、results.json、SHA256SUMS、README）。

**结果（R1）：63/63 PASS / 0 FAIL —— 合成边界 + 16 例 Rust `{:?}` 转义回归 + retained member 全行摘要逐位一致。**

## 1. 移植：`src_node/parsers/tdxhy.mjs`

| 语义 | 实现 |
|---|---|
| ASCII 门 | 任一字节 >0x7f → `tdxhy.cfg is not pure ASCII` |
| 行切分 | `\r\n`；末尾单个空行容忍（尾随 CRLF），中间空行 → `line N is empty` |
| 字段 | 恰好 6 个 `|` 字段 → `has N fields, expected 6` |
| f0 | `"0"/"1"/"2"` → market；其余 → `f0 "v" is not a single digit 0..=2` |
| f1 | 恰 6 位数字（前导零保留为字符串）→ `f1 "v" is not exactly six digits` |
| f2 | 空 → null；`T`+2/4/6 位数字（len 3/5/7）→ `f2 "v" is not a T code of anchored length` |
| f3/f4 | 必须为空（f3 先于 f4）→ `reserved field fN must be empty` |
| f5 | 空 → null；`X`+2/4/6 位数字 → `f5 "v" is not an X code of anchored length` |
| 优先级 | market → code → t → f3 → f4 → x（与 Rust 一致） |
| 行号 | 1-based；整文件在首个坏行失败，无跳过/修复 |

## 2. 验证矩阵（47 例）

| 组 | 例数 | 结果 |
|---|---|---|
| 合成 oracle | 37（空文件、单/多行、T/X 各长度、空可选、非 ASCII、LF-only、中空行、双尾随 CRLF、字段数、market/code/T/X 各拒、保留字段、3 例优先级） | PASS 逐字 |
| 类型边界 | 4（string/Array/null 拒 + Uint8Array 收） | PASS |
| retained | 6（MANIFEST sha256 + member 提取 + 全行摘要 + 4 采样行） | PASS |
| 转义回归（R1） | 16（f0/f1/f2/f5 中 `"` `\` `\n` `\r` `\t` `\0` 控制字节 DEL ESC 单引号） | PASS |

## 3. retained member

`tdxhy.cfg` 150,332B（sha256 `df7cb705…`）自 MANIFEST 验证 zip 有界提取：5,659 行、markets `{0:2946, 1:2365, 2:348}`、行摘要 `dcd97d9d…` 与 Rust 一致。原始字节不入库。

## 3.1 R1 Rust `{:?}` 转义修复（REVIEW-NM-021）

缺陷：四个含字段值的错误（BadMarket/BadCode/BadIndustryCode/BadResearchCode）按 Rust `{:?}` 渲染 —— Node 原样插入原始字节，`f0 "3""` 应为 `f0 "3\""`。修复：`dbg()` 实现 ASCII 域 `escape_debug`（`"` `\` `\n` `\r` `\t` `\0` 具名转义，其余控制字节 `\u{hex}`，单引号透传），已对 live rustc 输出核验。优先级不变。

## 4. 范围边界

不分配交易所/行业业务含义；无 catalog scope/compose/source 编排、DB/服务接线。

## 5. 验证记录

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p2/tdxhy_probe.mjs .` | 0（63 PASS，R1） |
| `rustc` oracle（`target/debug/tdxhy_oracle`） | 0 |
| `json.tool` / `sha256sum -c` / `git diff --check` | 0 |

## 6. 范围声明

仅新增 `src_node/parsers/tdxhy.mjs`、`scripts/node_migration_p2/tdxhy_probe.mjs`、`docs/tasks/node-p2-tdxhy.md`、`evidence/node-migration/p2/tdxhy/`。无 Rust 改动、live DB、ECS、deploy/push/merge。
