# Node 迁移 P2 — DBF 结构容器解析

任务：NM-020（agent-coordination/node-migration）。日期：2026-09-23。
基线：`fa004a093aa727c65d4e15abcc9e759d0e21f646`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271。来源：`src/dbf.rs::parse` + `DbfRecord::field_bytes`。
证据：`evidence/node-migration/p2/dbf_file/`（probe-results.jsonl 68 行全 PASS、rust-oracle.jsonl、results.json、SHA256SUMS、README）。

**结果：68/68 PASS / 0 FAIL —— 合成边界 + retained base.dbf 全记录摘要逐位一致。**

## 1. 移植：`src_node/parsers/dbf_file.mjs`

| 语义 | 实现 |
|---|---|
| 版本门 | `{0x03,0x83,0x30,0x31,0x32,0x43,0x63}`，其余 → `unsupported DBF version 0x..` |
| 错误优先级 | HeaderTruncated(<32) → UnsupportedVersion → InvalidHeaderLength(<33) → InvalidRecordLength(<1) → HeaderTruncated(<header_end) → 终止符 0x0d → 描述符循环 → ZeroLengthField → FieldsExceedRecord → BodyTruncated |
| 字段名 | 前 11B 截至首个 NUL，**Rust `from_utf8_lossy` 等价实现**（逐序列 U+FFFD + error_len 重同步——非 WHATWG TextDecoder） |
| 字段 | type@11 / length@16（0 拒） / decimals@17 |
| 记录 | `deleted = raw[0]=='*'`；`raw` = 删除标记后全部字节（**含 vendor 尾巴**，每条 fresh copy） |
| 长度校验 | `Σfield.length + 1 <= record_length`；body = `count*record_length` 经 `Number.isSafeInteger` 检查后才分配；**结尾多余字节允许** |
| `fieldBytes` | 前置长度求和切片，越界 → `undefined`（对齐 `Option`） |
| 错误文案 | 与 `DbfError` 全部 9 个变体逐字一致 |

## 2. 验证矩阵（68 例）

| 组 | 例数 | 结果 |
|---|---|---|
| 合成 oracle | 19（0/1/2 字段、删除标记、vendor 尾、7 种错误、UTF-8 lossy、11B 无 NUL 名、ragged 描述符） | PASS 逐字 |
| 类型边界 | 5（string/Array/null/number 拒 + Uint8Array 收） | PASS |
| field_bytes | 1（偏移/越界） | PASS |
| retained | 42（MANIFEST sha256 + member 提取 + header/字段/全记录摘要 + 40 个首记录 field_bytes） | PASS |

## 3. retained member

`official-base.zip` sha256 `dcdf6b49…`（MANIFEST 一致）→ 有界提取 `base.dbf` 4,270,189B（sha256 `6e6480bb…`，不入库）：version 0x03、8,875 记录、40 字段、header 1313、record_length 481、code_page 0x7a；records 摘要 `ff2fba6d…`、fields 摘要 `a26db445…` 与 Rust 一致。

## 4. 范围边界

不做代码页/业务字段解码、catalog 映射、DB/服务接线。原始 retained 字节不提交、不重新下载。

## 5. 验证记录

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p2/dbf_file_probe.mjs .` | 0（68 PASS） |
| `rustc` oracle（`target/debug/dbf_oracle`） | 0 |
| `json.tool` / `sha256sum -c` / `git diff --check` | 0 |

## 6. 范围声明

仅新增 `src_node/parsers/dbf_file.mjs`、`scripts/node_migration_p2/dbf_file_probe.mjs`、`docs/tasks/node-p2-dbf-file.md`、`evidence/node-migration/p2/dbf_file/`。无 Rust 改动、live DB、ECS、deploy/push/merge。
