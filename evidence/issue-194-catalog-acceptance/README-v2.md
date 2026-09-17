# Issue #194 — `securities.catalog` 验收运行 v2（P1 裁定后契约）

按 PLAN-v2（`PLAN.md` 追加节，#6 产品裁定 `5710586446`：缺名成员
= 受治理 `reject_candidate` 排除，具名成员照常发布）在真实
`mcp_data` stdio 服务上执行的验收记录，无偏差、无计划修订。
v1 运行记录（`README.md`/`results.json`/`run-log.jsonl`）保持原样。

运行身份：分支 `issue-194-catalog-acceptance-v2`，构建 head 见
`results-v2.json.run_identity.head`（main `df063f5` 含实现合并
c196823）；`cargo 1.98.1`；`TMPDIR=/dev/shm cargo build --locked
--bins`。scratch：`/home/box/acceptance-run-issue194/` 下
`db2-main.sqlite` + `db2-r1..r4.sqlite` + `snap2-r3`/`snap2-r4`
（与 v1 的 `db-*.sqlite`/`snap-r*` 隔离）。本机一次性手工
`run_worker_once`，非生产部署。

## 固定输入（V1/V2 实测一致，同 v1 pin）

manifest `b8a0d7b9…f444d3`；archive `base.zip dcdf6b49…`、
`gbbq.zip daad6290…`；四 member sha256 == PLAN §1 表。

## 机械期望复核（V3，retained 字节独立重算）

5659→scope 5230（m0=2906/m1=2324）+bj 348+b_share 78+anomalous 3；
ex.code 5571；TDXRSHY 474/TDXNHY 145；nameless 恰 6 verbatim；
**published=5224，published_missing_industry=0，
published_t_code_residuals=0**（唯一 residual `1:688688→T00` 属
被排除成员）。

## 执行结果（db2-main）

- `start_collection` `c1` → task `task-6e83b49a…f87`，item
  `catalog:current_sh_sz_a`；`run_worker_once` → item
  `success:true`，证据 verbatim（`task_items.outcome_json`）：
  `catalog_version_id=catalog-1789633175682388426`，
  `published_members=5224`，`reject_candidate_exclusions` 恰六条
  `0:001235,0:300728,1:601206,1:603302,1:603361,1:688688`
  （tdxhy 行序），reason 均为
  `reject_candidate: no name row in infoharbor_ex.code`；
  `missing_industry=[]`、`t_code_residuals=[]`、
  `t_dictionary_checked=true`、`scope_partition` verbatim。
- 发布：`catalog_versions` 恰 1 行，digest `21781f976c2b…2890`，
  `source` == `official-file:b8a0d7b9aec1a270` verbatim（V6 命中
  冻结值）；`catalog_members`=5224，全部 `tdxhy:<m>:<code>`；
  `catalog_head` 指向该 version。
- 幂等（V7/V8）：`c1` 重发 `reused:true`，版本数不变；`c2`（新
  request_id 同 selection）→ 新 task `task-1365f9b2…` 成功，第二
  version `catalog-1789633187158611988` digest `e4c851a9…fc`，成员
  投影与 v1 **逐行一致**（5224/5224），head 前进至 v2，v1 digest
  不变；worker 再跑 `idle:true`。
- 读回（V9）：`get_data_availability` → `availability:"available"`、
  `record_count:5224`、`version_ref` 绑定 v2 digest（verbatim 入
  run-log；`coverage_evidence` 字段如实为 `"validated fixture
  catalog snapshot"` 投影文案）；`query_data` catalog 分页 6 页
  合计恰 5224 行，行集合 == `catalog_members` 投影；
  `list_data_versions` 两 version verbatim。

## 拒绝路径 verbatim（独立 scratch 库，均零泄漏）

| 腿 | error verbatim |
|---|---|
| R1 env-absent | `SOURCE_NOT_VERIFIED: CATALOG_PUBLISH_NOT_VERIFIED (…)` + 其余三条 blocker 全文（见 `results-v2.json.blocked_paths`） |
| R2 uninspectable manifest | `CATALOG_SOURCE_FAILURE: snapshot inspection failed: manifest JSON invalid: missing field \`schema\` at line 1 column 12` |
| R3 成员缺失（snap2-r3 无 base.zip） | `CATALOG_SOURCE_FAILURE: snapshot inspection failed: local input unavailable: No such file or directory (os error 2)` |
| R4 成员损坏（snap2-r4 base.zip offset 1000 翻字节） | `CATALOG_SOURCE_FAILURE: snapshot inspection failed: artifact bytes do not match manifest: BaseIndustry` |

## 断言结果

`results-v2.json`：**12/12 PASS**（V1–V12，对应 PLAN-v2 §v2.2）。

## 如实标注

- `NOT_PRODUCTION_READY` 不变；四条 `PRODUCTION_BLOCKERS` 原样
  （`CATALOG_PUBLISH_NOT_VERIFIED` 理由文本按裁定更新为
  "ruled contract … #194 v2 acceptance not run"——本次运行即该
  验收，裁定后门禁状态是否解除属 FG 裁定，不在本切片）。
- `coverage_evidence` 可用性投影文案沿用既有 `"validated fixture
  catalog snapshot"` 字样（verbatim 留痕；文案是否调整不在本
  切片范围）。
- market.daily 5230 逐证券采集、部署、二代快照：出范围，NOT_RUN。

## 产物（v2）

`run_catalog_acceptance_v2.py`、`run-log-v2.jsonl`（verbatim）、
`results-v2.json`（V1–V12 + blocked_paths + 全部 id/hash）、
`README-v2.md`；v1 产物未改动。
