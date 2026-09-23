# Node 迁移 P2 — catalog scope 分区与对账

任务：NM-023（agent-coordination/node-migration）。日期：2026-09-23。
基线：`02128b573cc6f7d4173f06124dfc6dce4f5c10b2`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；#168 scope rule S。来源：`src/catalog_tdx/scope.rs::apply` + `reconciliation`。
证据：`evidence/node-migration/p2/catalog_scope/`（probe-results.jsonl 35 行全 PASS、rust-oracle.jsonl、results.json、SHA256SUMS、README）。

**结果：35/35 PASS / 0 FAIL —— 全部分支 + retained member 成员/排除摘要与对账计数逐位一致。**

## 1. 移植：`src_node/catalog/scope.mjs`

| 语义 | 实现 |
|---|---|
| S | market ∈ {0,1} ∧ t_code ≠ null ∧ x_code ≠ null → member（T/X 非可选） |
| BjMarket | market == 2 → 排除（无论码是否齐全） |
| BShare | market 0/1 + 缺 T 或 X + code 前缀 `200`/`900` |
| Anomalous | market 0/1 + 缺 T 或 X + 其余前缀 |
| 顺序 | 单遍，输入序保持；分区互斥且穷尽 |
| reconciliation | `{total,scope,bj_market,b_share,anomalous}`；`sum != total` → `ScopeBoundaryError` 抛（Rust `debug_assert` 在 release 被编译掉——Node 按契约意图改为无条件检查，文档化 delta） |
| 类型边界 | 分类前校验行形状：line 正整数、market 0..2 整数、code 6 位数字串、t/x 为 string\|null；拒绝抛 `ScopeBoundaryError`（确定性、文档化） |

## 2. 验证矩阵（35 例）

| 组 | 例数 | 结果 |
|---|---|---|
| 合成 oracle | 17（空、全 scoped、market2 有/无码、缺 T/X/双缺 × BShare/Anomalous、前缀边界 199/200/201/899/900/901、混合顺序） | PASS 逐字 |
| 类型边界 | 11（null/数组行、line 0/浮点、market 3/-1、code 短/非串、t 数字、x 缺字段、rows 非数组） | PASS |
| 对账违例 | 1（sum≠total 拒绝） | PASS |
| retained | 5（MANIFEST sha256 + member 提取 + 成员/排除摘要+对账 + 3 采样排除行） | PASS |

## 3. retained member

NM-021 `tdxhy.mjs` 原样组合，5,659 行 → 5,230 members + 429 excluded；成员摘要 `c96c914b…`、排除摘要 `ad9cc4ea…`、对账 `{5659, 5230, 348, 78, 3}` 与 Rust 一致。原始字节不入库。

## 4. 范围边界

不做 ex_code 名称联接、incon 字典、CatalogSnapshot 组合、DB/服务接线。机械标签不引申业务含义。

## 5. 验证记录

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p2/catalog_scope_probe.mjs .` | 0（35 PASS） |
| `rustc` oracle（`target/debug/scope_oracle`） | 0 |
| `json.tool` / `sha256sum -c` / `git diff --check` | 0 |

## 6. 范围声明

仅新增 `src_node/catalog/scope.mjs`、`scripts/node_migration_p2/catalog_scope_probe.mjs`、`docs/tasks/node-p2-catalog-scope.md`、`evidence/node-migration/p2/catalog_scope/`。无 Rust 改动、live DB、ECS、deploy/push/merge。
