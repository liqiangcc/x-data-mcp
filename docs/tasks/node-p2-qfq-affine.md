# Node 迁移 P2 — QFQ 仿射算术切片

任务：NM-013（agent-coordination/node-migration）。日期：2026-09-23。
基线：`b60b0ab28bd54c3c4be8323558147eb14d039387`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271。上游：P0 `gbbq_factor_normalize`（6）+ `qfq_affine`（3）golden。
证据：`evidence/node-migration/p2/qfq_affine/`（probe-results.jsonl 37 行全 PASS、rust-oracle.jsonl、results.json、SHA256SUMS、README）。

**结果：37/37 PASS / 0 FAIL —— golden 字节级一致，Decimal 引擎语义经真实 rust_decimal 1.43.0 rlib 逐条验证。**

## 1. 移植：`src_node/algorithms/qfq_affine.mjs`（纯 JS + BigInt，零依赖）

镜像 `src/gbbq.rs` 纯函数：`normalizeFactor`、`factors`（fail-closed NonFiniteFactor）、`eventTransform`、`composeChain`（空→恒等）、`applyAffine`、`referencePrice`、`displayedPreviousClose`。

## 2. Decimal 引擎语义（关键，全部 live Rust 验证）

| rust_decimal 行为 | 复现 |
|---|---|
| div：整数商后余数循环，每轮 ≤9 位（`find_scale`），`scale≤28` | 同构循环 |
| div 余数为零且 scale≥0 → 快速路径（`10/2="5"`） | ✓ |
| div cap 28 → **HALF-EVEN**（`2*rem>den` 或 `==` 且商奇） | ✓（`10/11="0.909…9091"`） |
| 余数处理后 `unscale` 分块去零 {8（需 mantissa%2³²==0）、4、2、1} | ✓（`10/4="2.50"`、`10/12.50="0.8"`、`10/5.00="2"`） |
| mul：scale 相加，**不归一化**（`0.5*2="1.0"`）；零积→ZERO | ✓ |
| add：**零操作数短路返回另一操作数原样**（`0.0+0="0"`、`0.00+0.0="0.0"`） | ✓ |
| mul/add 溢出 rescale → HALF-EVEN | ✓ |
| `from_str`：scale>28 HALF_UP（`5e-29→1e-28`）；mantissa>2⁹⁶−1→Err | ✓ |
| `round_dp(2, MidpointAwayFromZero)` 业务 2dp | ✓ |
| f32→f64→Rust Display 位置小数（e-记法展开） | ✓（NM-012 复用） |

## 3. 验证矩阵（37 例）

| 组 | 例数 | 结果 |
|---|---|---|
| golden | 9（normalize×6 + event_transform/compose_chain_two_events/displayed_previous_close） | PASS 字节一致 |
| rust_oracle 补充 | 14（零因子变换、纯现金、终止小数分母、负因子、单/三事件链、空链、3×displayed_prev_close、NaN/inf/-0.005/1.005 normalize） | PASS（期望值=rustc+rlib 现场输出） |
| decimal_semantics | 11（div/mul/add 各形态 + 零短路 + `-0`→`0`） | PASS |
| fail_closed | 3（NaN/inf factor → 错误文案逐字；空链恒等 `apply`） | PASS |

## 4. 范围边界

- 仅算术原语：无 `forward_adjust` bar 遍历、无 GBBQ 记录解码/解密、无 yearly、无服务接线。
- `decimal_overflow` 分支（>2⁹⁶ 或 scale<0 rescale）以 fail-closed 抛错实现，QFQ 域不可达，已记录。

## 5. 验证记录

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p2/qfq_affine_probe.mjs .` | 0（37 PASS） |
| `rustc` 补充 oracle（`target/debug/qfq_oracle`） | 0 |
| `json.tool` / `sha256sum -c` / `git diff --check` | 0 |

## 6. 范围声明

仅新增 `src_node/algorithms/qfq_affine.mjs`、`scripts/node_migration_p2/qfq_affine_probe.mjs`、`docs/tasks/node-p2-qfq-affine.md`、`evidence/node-migration/p2/qfq_affine/`。无 Rust 改动、live DB、ECS、deploy/push/merge。
