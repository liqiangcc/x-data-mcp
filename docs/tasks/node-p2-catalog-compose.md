# Node 迁移 P2 — 目录纯组合（compose）

任务：NM-025（agent-coordination/node-migration）。日期：2026-09-23。
基线：`a289e40fc98e8c30e3a0a2e8899fcd75ce986e6e`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；#6 P1 ruling 5710586446；#168 contract。来源：`src/catalog_tdx/compose.rs` + `tests/catalog_tdx.rs`。
证据：`evidence/node-migration/p2/catalog_compose/`（probe-results.jsonl 26 行全 PASS、rust-oracle.jsonl、results.json、SHA256SUMS、README）。

**结果：26/26 PASS / 0 FAIL —— 真实 gen-2 全结果（1,330,712B JSON）与 live Rust 逐位一致。**

## 1. 移植：`src_node/catalog/compose.mjs`

| 语义 | 实现 |
|---|---|
| MissingDictionary | `TDXRSHY` 节缺失 → 在任何 member 前拒绝（空 members 亦然） |
| TDXNHY 可选 | 缺席 → `t_dictionary_checked=false`，跳过 residual 检查 |
| BadMarket 优先 | 每 member 先查 market∈{0,1} —— **先于** nameless 排除 |
| nameless（P1） | 无名 member = 治理性排除，逐字枚举，不进 candidate，不产生 industry/T 诊断 |
| industry | `researchLevel2(x_code)` null → Missing `no level-2 code derivable from {x}`；L2 不在 TDXRSHY → Missing `level-2 code {l2} absent from TDXRSHY`；否则 Assigned{code,name,scheme=`tdx_research_industry`} |
| T residual | 仅当 TDXNHY 存在且 member.t_code 缺席 → `{member,t_code}` 逐字 |
| record | symbol/name/exchange(sh\|sz)/a_share/`tdxhy:{market}:{code}`/industry/source/acquired_at，输入序保留 |
| snapshot | records 空 → null；否则 completeness=Complete{expected_count=records.len}（发布数，非全市场） |

## 2. 真实 gen-2 字节（source.rs 路径）

`official-base.zip!tdxhy.cfg`（`df7cb705…`）+ `official-base.zip!infoharbor_ex.code`（`79eed0f6…`）+ `official-gbbq.zip!zhb.zip!incon.dat`（`86f758b2…`）；外层 `dcdf6b49…`/`daad6290…` 运行时对 `manifest-sha256.txt` 校验后才提取（有界 `zip_member.mjs`）。

**5,230 scope → 5,224 named + 6 nameless**；missing=0、residuals=0、checked=true；结果摘要 `8ddfaba2…` 与全规范化 JSON **逐位**同 Rust。

## 3. 固定测试元数据（标注，不主张真实清单身份）

`source="official-file:deadbeefcafe0001"`、`acquired_at=2023-11-14T22:13:20+00:00`（ts 1700000000）——测试值；不构成真实 manifest identity/acquisition 声明。

## 4. 验证矩阵（26 例）

| 组 | 例数 | 结果 |
|---|---|---|
| 合成 oracle | 10（assigned 两市/命名方案、nameless 排除、全无名→null、L1/缺席 L2→Missing、T residual、TDXRSHY 缺失×2、TDXNHY 缺席跳检、同名两 member、输入序） | PASS 逐字 |
| 类型边界 | 9（bad-market-先于-nameless、非数组/Map/sections、非字符串 source、坏时间戳、畸形/null member、Date 接受） | PASS |
| 外层 manifest | 2 | PASS |
| member sha | 3 | PASS |
| 真实 digest+全结果 | 2 | PASS 逐位 |

## 5. 范围边界

不做 source.rs 编排、snapshot 检查、SQLite 发布、服务。

## 6. 验证记录

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p2/catalog_compose_probe.mjs .` | 0（26 PASS） |
| `rustc` oracle（`target/debug/compose_oracle`） | 0 |
| `json.tool` / `sha256sum -c` / `git diff --check` | 0 |

## 7. 范围声明

仅新增 `src_node/catalog/compose.mjs`、`scripts/node_migration_p2/{catalog_compose_probe.mjs,zip_member.mjs}`、`docs/tasks/node-p2-catalog-compose.md`、`evidence/node-migration/p2/catalog_compose/`。无既有模块改动、无 Rust 改动、live DB、ECS、deploy/push/merge。
