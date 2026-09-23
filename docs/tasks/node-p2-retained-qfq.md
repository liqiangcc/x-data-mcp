# Node 迁移 P2 — retained 集成切片（.day + GBBQ + QFQ）

任务：NM-018（agent-coordination/node-migration）。日期：2026-09-23。
基线：`e65e9694266c82d1ba6e3141144b706656668f2f`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；NM-011..NM-017 模块的无修改组合。
证据：`evidence/node-migration/p2/retained_qfq/`（probe-results.jsonl 115 行全 PASS、rust-oracle.jsonl、results.json、SHA256SUMS、README）。

**结果：115/115 PASS / 0 FAIL —— 8,479 条 bar 的完整 QFQ 输出摘要与 Rust 逐位一致。仅限单标的 sz000002，不做全市场等价声明。**

## 1. 标的与映射

| 项 | 值 | 依据 |
|---|---|---|
| 安全代码 | `000002` | GBBQ member 中 112 条事件**全部** `market=0`（SZ） |
| `.day` member | `sz\lday\sz000002.day`（271,328B，sha256 `6f168cf0…`） | 有界提取，不入库 |
| 事件过滤 | `market==0 && code=="000002" && category==1` → 36 条 | 与 `official_market.rs` 一致（(market,code) 键 + cat-1，其余不解释） |
| 歧义排除 | `sh000002.day` 存在但无 (1,000002) 事件 | 映射唯一 |

## 2. 输入 MANIFEST 验证

| 输入 | sha256 | 结果 |
|---|---|---|
| `official-hsjday.zip` | `c82f852a…` | 与 MANIFEST 一致 |
| `official-gbbq-member` | `4a3e9b86…` | 与 MANIFEST 一致 |

ZIP 读取为探针内 EOCD→central-dir→local-header 有界实现（store+deflate，经 `node:zlib`），只提取选中 member。

## 3. 等价性验证

| 层 | Node | Rust | 判定 |
|---|---|---|---|
| 解析原始记录 | 8,479 | 8,479 | `c482596a…` 一致（`date,o,h,l,c,amount_bits,vol,r7` 每行 sha256） |
| cat-1 事件 | 36（日期序一致） | 36 | 一致 |
| QFQ 全部 bar | 8,479 | 8,479 | **`836c9117…` 一致**（`date,open,high,low,close,chain_count,amount_bits,vol,r7` 每行） |
| 采样 bar | ~72（全部 chain_event_count 边界 + 首/末） | 同 | 逐字段一致 |
| event-day marks | 36 | 36 | 逐字段一致 |

早期 bar 调整后为负值（约 −8.8）——属 Rust 语义正确输出，已记录。

## 4. 范围边界

- 单标的证据切片，非服务接线；无 yearly、DB、ECS。
- 非 category-1 记录不解释（112−36 条排除原因已记录）。

## 5. 验证记录

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p2/retained_qfq_probe.mjs .` | 0（115 PASS） |
| `rustc` oracle（`target/debug/nm018_oracle`，同字节输入） | 0 |
| `json.tool` / `sha256sum -c` / `git diff --check` | 0 |

## 6. 范围声明

仅新增 `scripts/node_migration_p2/retained_qfq_probe.mjs`、`docs/tasks/node-p2-retained-qfq.md`、`evidence/node-migration/p2/retained_qfq/`。复用模块零修改。无 Rust 改动、live DB、ECS、deploy/push/merge、新官方下载。
