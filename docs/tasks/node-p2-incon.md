# Node 迁移 P2 — `incon.dat` 分节字典解析

任务：NM-024（agent-coordination/node-migration）。日期：2026-09-23。
基线：`f94b5e79c7601fc5c009e7a1e710fec6894f1a3f`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；catalog contract v1。来源：`src/catalog_tdx/incon.rs`（`parse` + `code_level` + `research_level2` + `section_map`）。
证据：`evidence/node-migration/p2/incon/`（probe-results.jsonl 58 行全 PASS、rust-oracle.jsonl、results.json、SHA256SUMS、README）。

**结果：58/58 PASS / 0 FAIL —— 合成边界 + 真实字节（gen-2 official-gbbq.zip!zhb.zip!incon.dat）全节/条目摘要逐位一致。**

## ⚠ retained member 来源修正（REVIEW-NM-024）

任务假定 `incon.dat` 在 `official-base.zip` —— **实际不存在**（完整 listing 核验；gen-3 `base.zip` 亦无）。按 Rust `src/catalog_tdx/source.rs` 的实际消费路径，权威 gen-2 来源为 **`official-gbbq.zip!zhb.zip!incon.dat`**：外层 `daad6290…`（运行时对 `manifest-sha256.txt` 校验）、嵌套 `zhb.zip` 1,411,229B（`e1ded566…`）、member 67,543B（`86f758b2…`）。探针进程内嵌套提取（有界、只读、无 `/dev/shm`）：41 节（含 20 个 `#####` 空节 + 尾随）、3,662 条目、节/条目摘要 `fd14965e…` 与 live Rust 一致。

可选分离样本保留：客户端 deb `tc/incon.dat`（65,853B，`ca941e57…`，37 节/3,554 条目）——不同来源，仅字节等价证据，**不构成 gen-2 对齐声明**。

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
| 真实字节（gen-2） | 10（外层 manifest sha + zhb sha + member sha + 41 节/3662 条目摘要 + 6 secmap + first-match） | PASS |
| 分离样本（deb） | 1（来源标记，无对齐声明） | PASS |
| 负向验证（R3） | 4+2（CD 签名/非法 method/超大声明/inflate 上限拒绝 + outer/member sha selftest 退出 1） | PASS |

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
