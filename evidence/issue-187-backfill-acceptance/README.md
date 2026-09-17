# Issue #187 — 官方缺口回填验收运行（Phase D）

按冻结计划 `PLAN.md` 在真实 `mcp_data` stdio 服务上执行的全量验收记录。
运行身份：分支 `issue-187-backfill-acceptance`，构建 head `e4f7430`
（= `ee38cab` + 本分支 PLAN 修订提交），`cargo 1.98.1`，
`TMPDIR=/dev/shm cargo build --locked --bins`。
本机一次性手工 `run_worker_once` 调度，非生产部署。

注：运行后分支按协调要求 rebase 到 `607cc46`（#190 test-only 增量，
无行为变更）；`results.json.run_identity.head` 记录的是运行时点树
`e4f7430`，与 rebase 后树内容一致（仅新增测试与本文档说明）。

## 固定输入（B1 机械复核一致）

| 输入 | sha256 |
|---|---|
| `official-base.zip`（快照 `base.zip`）| `dcdf6b49898926675698263535021aed7ad92b34be82a1a9e8a2a8982bd68bc6` |
| `official-gbbq.zip` | `daad6290c4f1cd478d7436b1e7077260a3e3e52cefd7eae58e4f24878fa2d0e9` |
| `official-hsjday.zip` | `c82f852a7bc2e52c7e89347a1e0d1878b01ce997946c2745f7af323752270892` |
| `snapshot-manifest.json` | `b8a0d7b9aec1a270549d1e53ba26c0ab04842a23c05d1d1f7e75dd44a9f444d3` |

快照目录 `/home/box/acceptance-run-issue3/snapshot`；manifest 复用
`evidence/issue-3-acceptance-run/snapshot-manifest.json`；
服务 env `X_DATA_MCP_OFFICIAL_MANIFEST` + `X_DATA_MCP_OFFICIAL_DIR`。

scratch 目录 `/home/box/acceptance-run-issue187/`：`db-main.sqlite` +
`db-r1/r2/r3/r5/r6/r7.sqlite`（各拒绝腿独立库）。

## 执行序列与结果（db-main）

1. **seed_gap_subject**（acceptance-only 工具，真实官方快照 materialize
   − 恰删 2000-01-10 一条）→ subject v1
   `collection-version-cc167980ab4084142e293cf78aee2bdaa5b42879b96d2e94353ac05da0833dfd`
   = 8478 bars（1991-01-29 → 2026-09-14），digest
   `077e48174030c482f730baab226edb865f84f6f11fdbfbf20817f4c953bb0226`；
   `coverage.reason` verbatim:
   `ACCEPTANCE-LOCAL-GAP-SUBJECT: official sz.000002 QFQ bars minus frozen
   target 2000-01-10; complete only for this declared scratch coverage,
   not complete source history`；catalog 为 `fixture:acceptance-187`
   最小 fixture（如实标签）。
2. **verify_data_gap** `g1`（target `2000-01-10..2000-01-10`）→ 受理
   frozen subject=v1/digest；`run_worker_once` `success:true` →
   decision `task-612f40e7787f315fd1921baf9759c0c1:sz.000002`
   `confirmed_missing_range`（`basis_kind=source_gap_verifier`）+
   proven_gap `4687ef2300a98e4659324a3563b2d3ee29969fdbe1520277d4b11056f351fd74`
   `[2000-01-10,2000-01-10]`，evidence_digest `4b682a2886…d97939`。
3. **get_data_availability** → 恰一条 open confirmed gap（range=
   2000-01-10..2000-01-10）。
4. **backfill_data** `b1` `selection={"kind":"default"}` → 受理恰一 item，
   frozen `base_version_id`=v1、`proven_gap_ranges` 恰含该 gap、
   无 `refresh_range`；`run_worker_once` `success:true` →
   `market_update_results` `range-result-57d1d859…e366`
   `no_change=0`、`base_version_id`=v1、`result_version_id`=
   `range-version-57d1d8592c53b9380ab809230387b43e963016035ffc5eeabb8d06b0c83fe366`。
5. **结果**：新版本 8479 bars；effective head 前进到 v2，v1 仍
   `published` 保留；`gap_coverings` 绑定 gap→v2
   （2026-09-17T04:48:26Z）；availability open gap 消失，decision/
   proven_gap 历史行保留。
6. **补回值**：`2000-01-10` 行逐字段 ==
   `{open:-8.14, high:-8.06, low:-8.15, close:-8.06, volume:null,
   amount:null}`（冻结官方 QFQ bar）；`query_data` 服务读回与表读回
   一致；范围外 8478 行与 v1 canonical hash 逐行一致。
7. **幂等（S8）**：`g1`/`b1` 重发均 `reused:true`；新 `g2` 同参数
   verify → 第二条 decision `task-c6cbb284…:sz.000002`
   `no_local_gap`，**绑定新 head v2**（受理时点 head 已前进，v2 含
   target 日，结论如实）——无新 proven_gap/covering/version；新 `b2`
   default → verbatim `INVALID_COMMAND: EMPTY_SELECTION`；worker
   再跑无新产出。最终计数：version=2、decision=2、proven_gap=1、
   covering=1。
8. **provenance（S9）**：v2 `snapshot_inputs` 的 archive sha 命中
   retained hsjday/gbbq 实测，gbbq member sha
   `4a3e9b862247a8a3fec406cc25b9a5a6ee3303f740780822519c99decaa09d77`
   命中 MANIFEST pin；decision `basis_ref`（artifact_identity）含
   manifest sha + day member sha + gbbq member sha，`basis_sha256`
   自洽。

## 拒绝路径 verbatim（各独立 scratch 库）

| 腿 | verbatim 结果 |
|---|---|
| R1 env-absent | `GAP_BASIS_NOT_VERIFIED: no installed trusted source-gap verifier (#54/#46/#71)`；decision/gap 表空 |
| R2 uninspectable manifest | 同上 verbatim；decision/gap 表空 |
| R3 source-limitation | `2000-01-08`（周六，不在 .day member）→ `success:true` + decision `source_limitation`；**无 proven_gap** |
| R4 no-local-gap | `2000-01-11`（在 .day 且 full subject 含）→ `success:true` + decision `no_local_gap`；**无 proven_gap** |
| R5 subject 身份不符 | `--source fixture` 构造的 subject → `GAP_BASIS_NOT_VERIFIED: subject is not a pinned official-file QFQ daily version`；无 decision/gap |
| R6 stale-head | verify 受理后 seed v2 移 head → `MARKET_INTEGRITY: gap decision subject head moved`；无 decision/gap |
| R7 backfill 无 gap | full subject 无 open gap → `INVALID_COMMAND: EMPTY_SELECTION`；无新版本 |
| R8 explicit backfill | explicit-range item 仍走阻塞 provider → `INVALID_MARKET_COMMAND: SOURCE_NOT_VERIFIED: NOT_PRODUCTION_READY; publish blocked; <五条 blockers>`；无新版本 |

## 断言结果

`results.json`：**21/21 PASS**（B1–B20、B22；B21 与 B11 合并留痕 —
第二次 default backfill `EMPTY_SELECTION` verbatim + 无新
covering/version）。

## PLAN 偏差（一处，先改计划后执行）

冻结 PLAN 的 S8/B11 预期"新 request_id 同参数 verify → decision
replay no-op"。首次运行观察到：`subject_for` 在**受理时点**绑定
effective head；backfill 使 head 前进到 v2 后，同参数新 verify 请求
冻结 subject=v2（含 target 日）→ worker 如实写第二条
`no_local_gap` decision（绑定 v2 digest，decision_id 按 task 派生），
不产生新 gap/covering/version。该行为比"replay"更诚实（对已覆盖
状态给出新结论而非静默跳过）。已先修订 PLAN §S8/B11（commit
`e4f7430`）再以修订后口径断言通过；gap/covering/version 计数不变
这一关键不变量原样成立。

## 如实保留的限制

- **partial-coverage-不覆盖腿 `NOT_OBSERVABLE_VIA_REAL_SOURCE`**：
  default backfill committed range 构造上恒等于冻结 gap 并集，真实
  源路径无法造出"committed 不覆盖 gap"的状态；该不变量由
  `ensure_default_coverings` 单元测试覆盖。
- **changed-generation update `NOT_RUN`**：仅一代 pinned 官方快照
  （gen-1 字节永久不可得），不伪造不 synthetic。
- **五条 `PRODUCTION_BLOCKERS` 未解除**：publish 路径受理 verbatim
  列出全部五条（见 R8）；`NOT_PRODUCTION_READY` 原样保留在
  `adjustment_evidence.reason`。verify 受理按操作相关性仅列
  `GAP_BASIS_NOT_VERIFIED`（env 缺失时 worker 端 verbatim 拒绝）。
- 非 cat-1 GBBQ 记录按既有口径排除未解释（76 条）；`volume`/`amount`
  保持 NULL 未提升；多 gap/批量场景出范围。

## 产物

- `run_backfill_acceptance.py` — 验收驱动（唯一执行入口）
- `run-log.jsonl` — 全部 JSON-RPC 请求/响应、seed/子进程 stdout、
  DB 快照行 verbatim
- `results.json` — 断言明细 + blocked_paths verbatim + run 身份
- `PLAN.md` — 冻结计划（含执行前修订）
