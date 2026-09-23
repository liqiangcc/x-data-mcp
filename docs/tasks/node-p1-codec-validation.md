# Node 迁移 P1 — 数值/JSON 编解码验证

任务：NM-007（agent-coordination/node-migration）。日期：2026-09-23。
基线：`ff1d5506309fde836ad77be936ca1914dec3a141`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271。上游：P0 golden 向量 `evidence/node-migration/p0/numeric/vectors.jsonl`（51 行，已验收）。
证据：`evidence/node-migration/p1/codec/`（probe-results.jsonl 51 行、results.json、SHA256SUMS、README）。

**结果：39 PASS / 12 NOT_RUN / 0 FAIL —— 与已验收 golden 零失配，无 BLOCKED。**

## 1. 覆盖矩阵（按族）

| 族 | 行 | 状态 | Node 实现要点 |
|---|---|---|---|
| `day_parse` | 4 | PASS | u32 LE 帧解码、YYYYMMDD→日历校验、严格递增拒绝；错误文案逐字一致 |
| `amount_f32_projection` | 6 | PASS | DataView f32 bits→f64→`String()` 最短回推→canonical_decimal；NaN/±inf→null |
| `gbbq_factor_normalize` | 6 | PASS | f64 精确二进有理分解（BigInt）→2dp HALF_UP；`-0.001…`→`"0.00"`；非有限→null |
| `metric_ratio_change` | 6 | PASS | 精确有理 (end-anchor)/anchor ×10¹²，floor-division 修正 + HALF_EVEN；平局偶/奇商两例全对 |
| `digest_legacy_published` | 3 | PASS | serde_json `to_vec` 字节等价物（排序键紧凑 JSON）− `integrity_sha256` 后 sha256；非对象 `MARKET_INTEGRITY` 拒绝 |
| `digest_canonical_evidence` | 4 | PASS | 语义检查（object/bars·records 递归拒/VersionEvidence 形状）+ JCS + `x-data-evidence-v1\0` 域分离 sha256，digest 字节数组逐字节一致 |
| `digest_rfc8785` | 2 | 1 PASS + 1 NOT_RUN | `jcs()` 子集输出与 `canonicalize@2.1.0` 独立库逐字节一致；reject_boundary 保留 golden 的 justified_not_run |
| `compact_bar` | 6 | PASS | cents 精确 2dp、volume 整数非负 i64、canonical amount 词法规则、r7-on-yearly 拒、epoch_day |
| `segment_digest` | 3 | PASS | `x-data-segment-v1\0` + entity + period + count + bar 帧（be_i64、可选 flag+push_text）sha256 |
| `qfq_affine` / `qfq_forward_adjust` / `yearly_aggregate` | 11 | NOT_RUN | 域算术（decimal 仿射/精确求和）属算法迁移，不在 codec 切片 |

## 2. 关键边界（诚实声明）

- `jcs()` 是窄子集：覆盖完整 JSON 值域，键按 UTF-16 码元排序，非有限数与 bigint 拒绝；每条输出都与 `canonicalize` 库交叉验证。**未**把 `JSON.stringify` 当作 RFC8785。
- 整数安全：适配层内部一律 BigInt；golden 比较经 JCS（对象键序无关）。
- `parseDecimal` 词法子集 `[+-]?digits[.digits]`：rust_decimal `from_str` 接受的指数形式未覆盖（无向量需要，已记录）。
- `serdeJsonBytes` 仅在本输入域（ASCII 安全字面量 + BTreeMap 排序键）与 serde_json 字节等价，由 legacy golden 摘要相等实证。
- 不主张算法等价；Node 更快与否不作断言。

## 3. 依赖

`canonicalize@2.1.0`（2025-03 发布，纯 JS，仅作 JCS 交叉校验）；`better-sqlite3@11.10.0`（NM-006 装入，本探针未使用）。两包均已精确锁定于 package-lock.json。

## 4. 验证记录

| 命令 | 退出码 | 结果 |
|---|---|---|
| `node scripts/node_migration_p1/codec_probe.mjs .` | 0 | 51 行，39 PASS / 12 NOT_RUN / 0 FAIL |
| `python3 -m json.tool results.json`、逐行 json.loads | 0 | 合法 |
| `sha256sum -c SHA256SUMS` | 0 | OK |
| `git diff --check` | 0 | 干净 |

## 5. 范围声明

仅新增 `scripts/node_migration_p1/codec_probe.mjs`、`docs/tasks/node-p1-codec-validation.md`、`evidence/node-migration/p1/codec/` 及 package 元数据（canonicalize 依赖）。未改 Rust 源码/测试/迁移；无生产 DB/ECS/push/merge/deploy。
