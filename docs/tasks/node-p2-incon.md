# Node 迁移 P2 — `incon.dat` 分节字典解析

任务：NM-024（agent-coordination/node-migration）。日期：2026-09-23。
基线：`f94b5e79c7601fc5c009e7a1e710fec6894f1a3f`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；catalog contract v1。来源：`src/catalog_tdx/incon.rs`（`parse` + `code_level` + `research_level2` + `section_map`）。
证据：`evidence/node-migration/p2/incon/`（probe-results.jsonl 49 行全 PASS、rust-oracle.jsonl、results.json、SHA256SUMS、README）。

**结果：49/49 PASS / 0 FAIL —— 合成边界 + 真实字节（官方客户端包内 incon.dat）全节/条目摘要逐位一致。**

## ⚠ retained member 差异（已如实上报）

任务假定 `incon.dat` 在 gen-2 `official-base.zip` —— **实际不存在**（完整 listing 核验；gen-3 `base.zip` 亦无）。改用官方客户端包 `com.tdx.tdxcfv_7.64_amd64.deb`（MANIFEST 登记输入）内 `./opt/apps/com.tdx.tdxcfv/files/tc/incon.dat`（65,853B，sha256 `ca941e57…`）做真实字节对比——来源不同已明确标注，未做静默替代。

## 1. 移植：`src_node/parsers/incon.mjs`

| 语义 | 实现 |
|---|---|
| GB18030 严格解码 | `TextDecoder(fatal)` 整缓冲先解码（trail 可等于 `\|`） |
| 分节 | `#` 前缀行 → section（name = `#` 后全文，可空/`#####`）；retained 尾随 `######` → 空节 |
| 条目 | `code\|name[\|extra...]`；extra 逐字保留不解释 |
| code | 非空且全 ASCII → `non-ASCII or empty code` |
| name | 空 → `empty name` |
| 重复 code | 仅同节内拒收（跨节合法）；`{:?}` 转义 section+code |
| 优先级 | decode → 行（空行→malformed 除末行）→ header/entry-before-header → fields≥2 → code → name → dup；文案逐字 |
| LF-only | 单行合法（name/extra 内嵌 `\n`——Rust 实测） |

## 2. helpers

| 函数 | 语义 |
|---|---|
| `codeLevel` | 长 3\|5\|7、**任意** ASCII 字母 + 数字 → `(len-1)/2`；否则 null（`t01`/`Z999999`/`A1234` 已验） |
| `researchLevel2` | level2→自身；level3→前 5 字符；level1/畸形→null |
| `sectionMap` | 首个同名节 → `[[code,name]...]`（14 个 `#####` 重名节验证 first-match） |

## 3. 验证矩阵（49 例）

| 组 | 例数 | 结果 |
|---|---|---|
| 合成 oracle | 21（空文件、header-only、单/多节、extra 列、尾随空节、空节名、节内/跨节重复、header 前条目、中/双空行、1 字段、空/非 ASCII code、空名、decode 坏、优先级×2、LF-only、转义×2） | PASS 逐字 |
| helpers | 17（长度/前缀/畸形、小写字母、非 T/X 字母） | PASS |
| 类型边界 | 3（string/null 拒 + Uint8Array 收） | PASS |
| 真实字节 | 7（deb member 来源 + 37 节/3554 条目摘要 + 4 secmap + first-match） | PASS |

## 4. 范围边界

不做名称联接/组合、DB/服务、行业含义推断。

## 5. 验证记录

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p2/incon_probe.mjs .` | 0（49 PASS） |
| `rustc` oracle（`target/debug/incon_oracle`） | 0 |
| `json.tool` / `sha256sum -c` / `git diff --check` | 0 |

## 6. 范围声明

仅新增 `src_node/parsers/incon.mjs`、`scripts/node_migration_p2/incon_probe.mjs`、`docs/tasks/node-p2-incon.md`、`evidence/node-migration/p2/incon/`。无 Rust 改动、live DB、ECS、deploy/push/merge。
