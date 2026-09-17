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
