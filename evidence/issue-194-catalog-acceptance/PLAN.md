# Issue #194 — `securities.catalog` 官方文件全量采集验收（freeze plan）

状态：**冻结计划（Phase A）**——先冻结后运行。本文件不含运行结果；
Phase B 在新分支按本文件执行。

## 0. 范围与排除

- 仅 `securities.catalog` 官方文件全量采集这一个 bounded vertical
  slice（设计：`docs/tasks/issue-6-catalog-impl.md`，执行器
  `src/catalog_tdx/source.rs`）。
- 排除：market.daily 5230 逐证券采集、P1/P2 语义变更、部署、新数据
  源、二代快照。不得改 `src/`。
- 门禁：`NOT_PRODUCTION_READY` 与其余 `PRODUCTION_BLOCKERS` 原样
  保留；本验收不解除任何生产门禁。

## 1. 固定输入

快照目录 `/home/box/acceptance-run-issue3/snapshot`，manifest
`evidence/issue-3-acceptance-run/snapshot-manifest.json`
（sha256 `b8a0d7b9aec1a270549d1e53ba26c0ab04842a23c05d1d1f7e75dd44a9f444d3`）。

archive sha256（gen-2 retained pin）：

| archive | sha256 |
|---|---|
| `base.zip` | `dcdf6b49898926675698263535021aed7ad92b34be82a1a9e8a2a8982bd68bc6` |
| `gbbq.zip` | `daad6290c4f1cd478d7436b1e7077260a3e3e52cefd7eae58e4f24878fa2d0e9` |

zip 内目录成员 sha256（本计划起草时已从 retained 字节独立实测）：

| member | bytes | sha256 |
|---|---|---|
| `base.zip!tdxhy.cfg` | 150332 | `df7cb7051d0f73a0d1381e9b36eafa33c8f5081d28ddef87cad406dd28ed8ea6` |
| `base.zip!infoharbor_ex.code` | 182145 | `79eed0f6857ffde3d1d3cc8966ceff6e400f45e6e2387224446e453850fb5281` |
| `gbbq.zip!zhb.zip` | 1411229 | `e1ded566dc5269642de760211ec1b67e5ebebd67ab1e6338f70cab7c05048350` |
| `zhb.zip!incon.dat` | 67543 | `86f758b29033632fae23b85a7f002b4a6906fb88a3d2904495865c3b2cfb6f51` |

服务入口：`task_sqlite init` + `mcp_data` stdio + env
`X_DATA_MCP_OFFICIAL_MANIFEST` / `X_DATA_MCP_OFFICIAL_DIR`；
`TMPDIR=/dev/shm cargo build --locked --bins`；一次性手工
`run_worker_once` 调度，非生产部署。

## 2. 机械期望（验收脚本在 retained 字节上独立重算，不抄实现输出）

机械口径（锚定格式，独立于被测实现的 python 重算）：

- `tdxhy.cfg`：ASCII、CRLF、每行 6 个 `|` 字段；f0 市场位
  {0,1,2}，f1 六位代码，f2 `T` 行业码或空，f5 `X` 研究行业码或空。
- 冻结 scope 规则 S：f0∈{0,1} ∧ f2≠ε ∧ f5≠ε；排除分桶
  bj_market(f0=2) / b_share(f0∈{0,1} 且缺码且 200/900 前缀) /
  anomalous(其余缺码)，四桶不重不漏。
- `infoharbor_ex.code`：GB18030、CRLF、`code|name|...`；code→name
  唯一键（重复即整文件拒绝——重算同样要求）。
- `incon.dat`：GB18030、CRLF、`#SECTION` 段 + `code|name[|extra]`；
  `#TDXRSHY`（研究行业二级字典）与 `#TDXNHY`（T 码字典）。
- nameless：scope 成员中 code ∉ ex.code 键集 → 逐一 verbatim。
- missing_industry：X 码 level-2（len5→自身、len7→前 5 字符、其余
  无祖先）∉ TDXRSHY 键集的 scope 成员数。
- t_code_residuals：scope 成员 t_code ∉ TDXNHY 键集。

冻结的机械期望（本计划起草时从 retained gen-2 字节实测）：

| 量 | 冻结值 |
|---|---|
| tdxhy.cfg 总行数 | 5659 |
| scope 成员数 | 5230（market 0=2906，market 1=2324） |
| bj_market | 348 |
| b_share | 78 |
| anomalous | 3 |
| infoharbor_ex.code name 行数 | 5571 |
| TDXRSHY 字典条目 | 474 |
| TDXNHY 字典条目 | 145 |
| nameless（reject_candidate）成员 | 恰 6：`0:001235,0:300728,1:601206,1:603302,1:603361,1:688688`（tdxhy 行序） |
| missing_industry | 0 |
| t_code_residuals | 恰 1：`1:688688` → `T00` |

## 3. 冻结序列（S0–S7，db-main 一个 scratch 库）

- **S0** 环境留痕：`git rev-parse HEAD`、`cargo --version`、
  `mcp_data`/`task_sqlite` sha256、manifest sha256、两个 archive
  sha256、四个 member sha256（脚本实测 == §1 pin）。
- **S1** `start_collection`：
  `{"request_id":"c1","dataset":"securities.catalog","contract_version":"v1","selection":{"scope":"current_sh_sz_a"}}`
  → 受理 task/item id 留痕。
- **S2** `run_worker_once` → item 结果 verbatim。
- **S3** 契约正确结果断言（gen-2 + P1 现行契约）：item
  `success:false`，error 恰为
  `CATALOG_REJECTED: nameless scope members: 0:001235,0:300728,1:601206,1:603302,1:603361,1:688688`；
  `catalog_versions`/`catalog_members`/`catalog_head` 全空——
  无缺名候选被放行，无部分发布。
- **S4** 幂等：同 `request_id:"c1"` 重发 → `reused`；新 request_id
  同 selection → 新 task 同 verbatim 拒绝；再 `run_worker_once` →
  idle；catalog 表计数不变。
- **S5** 读回：`get_data_availability`
  `{"dataset":"securities.catalog","contract_version":"v1","selection":{"kind":"catalog","scope":"current_sh_sz_a"},"filter":{"kind":"catalog"},"version":{"kind":"latest_per_entity"},"page_size":10,"cursor":null}`
  verbatim 留痕（无已发布 catalog → absent/无 head 的如实形态）；
  `list_data_versions` catalog 分支 verbatim。
- **S6** 拒绝路径（各独立 scratch 库）：
  - R1 env-absent：无 `X_DATA_MCP_OFFICIAL_*` → catalog item
    `SOURCE_NOT_VERIFIED` verbatim；
  - R2 uninspectable manifest：`{"files":[]}` → item
    `CATALOG_SOURCE_FAILURE: snapshot inspection failed …` verbatim；
  - R3 成员缺失：scratch 快照目录缺 `base.zip`（manifest 不变）→
    `CATALOG_SOURCE_FAILURE` verbatim（inspect 字节核验 fail-closed）；
  - R4 成员损坏：scratch 快照目录 `base.zip` 截断/改写一字节 →
    `CATALOG_SOURCE_FAILURE` verbatim。
  - 各腿均断言：无 catalog version/member/head 行、无泄漏。
- **S7** 门禁留痕：受理响应 `blocking_reasons` verbatim 入
  results.json；`NOT_PRODUCTION_READY` 不变。

## 4. 断言清单（编号；全部入 results.json）

| # | 断言 | 冻结口径 |
|---|---|---|
| C1 | 输入完整性 | §1 全部 sha256 实测 == pin |
| C2 | member 独立重算 | 四 member sha256 实测 == §1 表 |
| C3 | scope 分桶 | 机械重算 == §2 表（total/scope/bj/b_share/anomalous 逐项） |
| C4 | nameless verbatim | item error 恰含且仅含 §2 六个 `market:code`（顺序=行序） |
| C5 | reject 不泄漏 | `catalog_versions`/`catalog_members`/`catalog_head` 行数=0 |
| C6 | 幂等 | c1 reused；c2 新 task 同 verbatim；worker 再跑 idle；表计数不变 |
| C7 | 读回如实 | availability/versions catalog 分支 verbatim 记录，无 head |
| C8 | env-absent | R1 verbatim `SOURCE_NOT_VERIFIED`，零泄漏 |
| C9 | uninspectable | R2 verbatim `CATALOG_SOURCE_FAILURE`，零泄漏 |
| C10 | 成员缺失 | R3 verbatim `CATALOG_SOURCE_FAILURE`，零泄漏 |
| C11 | 成员损坏 | R4 verbatim `CATALOG_SOURCE_FAILURE`，零泄漏 |
| C12 | 门禁保留 | blocking_reasons verbatim 入档；`NOT_PRODUCTION_READY` 不变 |

## 5. 条件断言与如实标注

- **目录版本 immutable/receipt、provenance member-sha 绑定、
  missing_industry/t_code_residuals 计数**：gen-2 + P1 现行契约下
  reject 路径不产生 catalog version，诊断集合（missing_industry=0、
  t_code_residuals=[1:688688→T00]）不持久化——这些断言记
  `NOT_APPLICABLE_ON_REJECT_PATH`，机械期望值已冻结于 §2 备查；
  若届时实现持久化诊断或 P1 裁定变化，先修 PLAN 再断言。
- **BLOCKED_ON_IMPL**：若 `securities.catalog` 采集链路在当前 main
  上不可达（受理拒绝/dispatch 缺失），相关断言记 BLOCKED_ON_IMPL
  如实收尾，不伪造 PASS。
- 观察与冻结期望结构性偏离时：先停跑、修订 PLAN、再干净重跑。

## 6. 产物

`evidence/issue-194-catalog-acceptance/` 下 Phase B 追加：
`run_catalog_acceptance.py`、`run-log.jsonl`（verbatim）、
`results.json`（C1–C12 + blocked_paths + 全部 id/hash）、`README.md`。
scratch 目录 `/home/box/acceptance-run-issue194/`（db-main +
db-r1..r4）。

---

# PLAN-v2（P1 产品裁定后修订，追加不覆盖上文历史）

依据：#6 评论 `5710586446`（2026-09-17 需求侧裁定）——缺名
scope 成员以 `reject_candidate` **显式排除并 verbatim 留痕**
（成员清单 + 原因，入任务结果证据），其余具名成员正常发布；
这改变需求而非放宽校验，缺名成员仍是受治理排除项。
下方为修订后冻结期望；v1 运行记录（`README.md`/`results.json`/
`run-log.jsonl`，PR #197，12/12 PASS）保持原样。

## v2.0 前提

- 实现变更（PR-B，独立分支）合入 main 后才可运行；未合入时
  v2 运行项整体记 `BLOCKED_ON_IMPL`。
- v1 的 §1 固定输入、§2 机械期望、C1–C3、R1–R4 verbatim 期望
  **不变**；scratch 目录复用 `/home/box/acceptance-run-issue194/`
  （v2 库名 `db2-main.sqlite` / `db2-r1..r4.sqlite`，与 v1 产物
  隔离）。

## v2.1 冻结期望（发布侧）

- item `success:true`；item 结果证据 verbatim 记录排除清单：
  恰六个 `market:code`（`0:001235,0:300728,1:601206,1:603302,
  1:603361,1:688688`，tdxhy 行序）+ 排除原因 `reject_candidate`
  （缺名）。证据字段命名
  由实现决定；断言核对内容与顺序，不核对字段名。
- 发布 `catalog_members` 恰 **5224** 行（5230 − 6）；全部
  `source_security_id = tdxhy:<market>:<code>`。
- `catalog_versions.source` == `official-file:b8a0d7b9aec1a270`
  （manifest sha16 verbatim；manifest → archive sha256 → 成员字节
  链式绑定，member sha256 pin 见 §1）。
- `catalog_head` 指向该 version；`catalog_versions` 恰 1 行
  （c1 执行后）。
- 已发布成员诊断（PLAN-v2 起草时 retained 字节独立实测）：
  `missing_industry = 0`、`t_code_residuals = 0`（唯一 residual
  `1:688688→T00` 在被排除的缺名成员中）。诊断值 verbatim 入
  任务结果证据。
- 幂等：`c1` 同 request_id 重发 `reused:true`；`c2`（新
  request_id 同 selection）按现行 publish 语义产生**第二个**
  version（id 为 `catalog-<publish nanos>`），member 投影与 v1
  逐行一致、head 前进至 v2——冻结"内容无变化、版本递增"的
  如实口径；worker 再跑 `idle:true`。
- 读回：`get_data_availability` catalog → `availability` 非
  `absent`（如实 verbatim 入档）；`query_data` catalog 分页合计
  恰 5224 行；`list_data_versions` 逐条 verbatim。

## v2.2 断言清单（V 系列；C1–C3/R1–R4 沿用 v1 冻结口径）

| # | 断言 | 冻结口径 |
|---|---|---|
| V1 | 输入完整性 | 沿用 C1（§1 全部 sha256 实测 == pin） |
| V2 | member 独立重算 | 沿用 C2（四 member sha256 == §1 表） |
| V3 | scope 分桶 | 沿用 C3（§2 表逐项） |
| V4 | item 成功+排除 verbatim | `success:true`；证据排除清单恰六 verbatim（行序）+ `reject_candidate` 原因；`missing_industry=0`、`t_code_residuals=0` verbatim |
| V5 | 发布行数 | `catalog_members`=5224；`catalog_versions`=1；`catalog_head`==该 version |
| V6 | provenance 绑定 | `catalog_versions.source`==`official-file:b8a0d7b9aec1a270` verbatim；member sha256 pin 入 run 证据 |
| V7 | version immutable | v1 发布后 v2 到来前 version 行/成员投影/digest 不变 |
| V8 | 幂等 | c1 `reused:true`；c2 第二 version 成员投影与 v1 逐行一致、head 前进；worker `idle:true` |
| V9 | 读回 | query_data catalog 分页合计恰 5224 行且与表读回一致；availability/versions verbatim |
| V10 | R1 env-absent | 沿用 C8：`SOURCE_NOT_VERIFIED` verbatim，零泄漏 |
| V11 | R2–R4 source fail-closed | 沿用 C9–C11：`CATALOG_SOURCE_FAILURE` verbatim，零泄漏 |
| V12 | 门禁保留 | blocking_reasons verbatim 入档；`NOT_PRODUCTION_READY` 不变 |

## v2.3 如实标注

- 六缺名成员**不进入目录**——排除是受治理行为，非静默跳过；
  `MemberDisappeared` 语义不受本次变更影响（版本间成员消失仍按
  既有规则）。
- v1 atomic-reject 行为是裁定前契约正确行为；v2 改变的是需求
  契约，v1 证据不回溯修订。
- 若实现观察到结构性偏离（如排除清单不完整、成员数≠5224）：
  停跑、先修 PLAN、再干净重跑。
