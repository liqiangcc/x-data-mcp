# Issue #194 — `securities.catalog` 官方文件全量采集验收运行（Phase B/C）

按冻结计划 `PLAN.md`（main `a988f9b` 合入版）在真实 `mcp_data` stdio
服务上执行的验收记录，无偏差、无计划修订。

运行身份：分支 `issue-194-catalog-acceptance`，构建 head 见
`results.json.run_identity.head`；`cargo 1.98.1`；
`TMPDIR=/dev/shm cargo build --locked --bins`。
scratch 目录 `/home/box/acceptance-run-issue194/`（`db-main.sqlite` +
`db-r1..r4.sqlite`，R3/R4 另含 `snap-r3`/`snap-r4` 快照目录变体）。
本机一次性手工 `run_worker_once` 调度，非生产部署。

## 固定输入（C1/C2 实测一致）

- manifest `b8a0d7b9aec1a270549d1e53ba26c0ab04842a23c05d1d1f7e75dd44a9f444d3`
- archive：`base.zip` `dcdf6b49…8bc6`、`gbbq.zip` `daad6290…d0e9`
- member：`tdxhy.cfg` `df7cb705…d8ea6`、`infoharbor_ex.code`
  `79eed0f6…5281`、`zhb.zip` `e1ded566…8350`、`incon.dat`
  `86f758b2…6f51`（全部实测 == PLAN §1 pin）

## 机械期望复核（C3，retained 字节独立重算）

`run_catalog_acceptance.py::mechanical_recompute` 按 PLAN §2 锚定
格式独立解析（不调用被测实现）：tdxhy.cfg 5659 行 → scope 5230
（market0=2906/market1=2324）+ bj 348 + b_share 78 + anomalous 3；
ex.code 5571 名；TDXRSHY 474 / TDXNHY 145；nameless 恰 6；
missing_industry 0；t_code_residuals `[1:688688→T00]`。
全部与冻结值一致。

## 执行结果（db-main，S1–S5）

- `start_collection` `c1` → `task-bf198a679c610f352588b0014a3a79ac`，
  item `catalog:current_sh_sz_a`，受理 readiness=blocked，
  blocking_reasons 四条 verbatim 入 `results.json.production_blockers`。
- `run_worker_once` → item `success:false`，error **逐字** ==
  `CATALOG_REJECTED: nameless scope members: 0:001235,0:300728,1:601206,1:603302,1:603361,1:688688`
  ——P1 `reject_candidate` 契约正确行为；`catalog_versions`/
  `catalog_members`/`catalog_head` 全空（C5 零泄漏）。
- 幂等（S4/C6）：`c1` 重发 `reused:true`；`c2`（新 request_id 同
  selection）→ `task-2c1e92a5…` 同 verbatim 拒绝；worker 再跑
  `idle:true`；catalog 表计数不变。
- 读回（S5/C7）：`get_data_availability` catalog →
  `availability:"absent"`、`coverage_evidence:"no published
  version"`、`version_ref:null`、`related_tasks` 如实列出两条
  failed 采集 task；`list_data_versions` catalog → `items:[]`。

## 拒绝路径 verbatim（独立 scratch 库，均零泄漏）

| 腿 | error verbatim |
|---|---|
| R1 env-absent | `SOURCE_NOT_VERIFIED: CATALOG_PUBLISH_NOT_VERIFIED (…); GBBQ_QFQ_NOT_VERIFIED (…); FILE_ACCESS_USE_NOT_VERIFIED (…); FILE_SET_CONSISTENCY_NOT_VERIFIED (…)`（四条 blocker 全文见 `results.json.blocked_paths.R1_env_absent`） |
| R2 uninspectable manifest | `CATALOG_SOURCE_FAILURE: snapshot inspection failed: manifest JSON invalid: missing field \`schema\` at line 1 column 12` |
| R3 成员缺失（snap-r3 无 base.zip） | `CATALOG_SOURCE_FAILURE: snapshot inspection failed: local input unavailable: No such file or directory (os error 2)` |
| R4 成员损坏（snap-r4 base.zip 翻转 offset 1000 一字节） | `CATALOG_SOURCE_FAILURE: snapshot inspection failed: artifact bytes do not match manifest: BaseIndustry` |

## 断言结果

`results.json`：**12/12 PASS**（C1–C12）。

## 如实标注

- 目录版本 immutable/receipt、provenance member-sha 绑定、
  missing_industry/t_code_residuals 持久化计数：
  `NOT_APPLICABLE_ON_REJECT_PATH`——reject 路径不产生 catalog
  version，诊断集合不持久化；机械期望值已冻结于 PLAN §2 备查。
- `NOT_PRODUCTION_READY` 不变；四条 `PRODUCTION_BLOCKERS` 原样
  （`LOCAL_PIPELINE_NOT_IMPLEMENTED` 已按 #187 FG 裁定退休，
  blocker 理由文本按 FG 5700226878 更新）。
- market.daily 5230 逐证券采集、部署、二代快照：出范围，NOT_RUN。

## 产物

- `run_catalog_acceptance.py` — 唯一执行入口
- `run-log.jsonl` — 全部 JSON-RPC 请求/响应、init/子进程输出、机械
  重算明细 verbatim
- `results.json` — C1–C12 断言 + blocked_paths + run 身份
- `PLAN.md` — 冻结计划（main `a988f9b` 合入版，无修订）
