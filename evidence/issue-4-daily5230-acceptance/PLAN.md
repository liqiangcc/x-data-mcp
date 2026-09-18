# Issue #4 子切片 — `market.daily` 全目录首次采集验收（freeze plan）

状态：**冻结计划（Phase A）**——先冻结后运行。本文件不含运行结果；
Phase B 在新分支按本文件执行，不得在冻结前跑 5224 采集。

父 Issue：#4（行情数据：持续更新并补齐缺失行情）；目录侧输入依赖
#194 v2 已验收发布的 catalog（5224 成员）；共享解码通道 #198 已合入
（`MaterializeSession`，单次 gbbq 解码服务整批 symbol）。

## 0. 范围与排除

- 仅 `market.daily/v1` `start_collection` 在 retained gen-2 官方快照上
  的全目录首次采集这一个 bounded vertical slice。
- 排除：update/backfill 全目录、yearly 全目录、P1/P2 语义变更、部署、
  新数据源、二代快照、性能优化变更。不得改 `src/`。
- 门禁：`NOT_PRODUCTION_READY` 与 `PRODUCTION_BLOCKERS` 原样保留；
  本验收不解除任何生产门禁（#3 仍 OPEN）。

## 1. 受理路径裁定点（代码事实，先于运行冻结）

`WriteSelection::Catalog{version_id}` 经
`CollectionContract::resolve`（`src/data_supply/collect.rs`）解析为全部
5224 个实体后，命中统一上限
`entities.len() > 100 → "resolved selection budget 1..100"`（受理
verbatim `INVALID_COMMAND: resolved selection budget 1..100`）：
**单请求 Catalog 选择无法承载 5224 items，受理即拒绝**。
`WriteSelection::Default` 对 `start_collection` 同样拒绝
（`"default selection is only implemented for market.daily update/backfill"`）。

裁定：Catalog 选择路径记 **BLOCKED_ON_IMPL**（如实记录 verbatim 拒绝，
不放宽上限、不改代码）。可行路径为 **分片 explicit selection**：
53 个 `start_collection` 请求，每片 ≤100 securities
（52 片 ×100 + 1 片 ×24），逐片受理 + `run_worker_once` 排空。
分片 explicit 的 per-item `frozen_inputs` 不含 catalog 绑定（代码事实：
explicit 分支写 `json!({})`）；catalog version/digest 绑定由运行脚本
在 results.json 运行级证据中如实记录，不伪造到 item 冻结输入里。

## 2. 固定输入

快照目录 `/home/box/acceptance-run-issue3/snapshot`，manifest
`evidence/issue-3-acceptance-run/snapshot-manifest.json`
（sha256 `b8a0d7b9aec1a270549d1e53ba26c0ab04842a23c05d1d1f7e75dd44a9f444d3`）。

archive sha256（本计划起草时实测 == 既有 pin）：

| archive | sha256 |
|---|---|
| `base.zip` | `dcdf6b49898926675698263535021aed7ad92b34be82a1a9e8a2a8982bd68bc6` |
| `gbbq.zip` | `daad6290c4f1cd478d7436b1e7077260a3e3e52cefd7eae58e4f24878fa2d0e9` |
| `hsjday.zip` | `c82f852a7bc2e52c7e89347a1e0d1878b01ce997946c2745f7af323752270892` |

scratch 目录 `/home/box/acceptance-run-issue4/`（`db.sqlite` 一个全新
库 + `db-r1.sqlite` env-absent 腿）。服务入口：`task_sqlite init` +
`mcp_data` stdio + env `X_DATA_MCP_OFFICIAL_MANIFEST`/
`X_DATA_MCP_OFFICIAL_DIR`；`TMPDIR=/dev/shm cargo build --locked --bins`；
`run_worker_once` 手工循环，非生产部署。

## 3. 机械期望（retained 字节独立实测，起草时完成）

成员枚举与交集（python 独立重算，不经被测实现）：

| 量 | 冻结值 |
|---|---|
| `hsjday.zip` 成员总数 | 12411（全部 `*\lday\*` 命名） |
| 标准 `sh|sz`+六位 `.day` member | 11801（sh 5945 + sz 5856） |
| 非标准 member（`bj*`/`sz200*` 等） | 610 |
| tdxhy scope 成员（f0∈{0,1} ∧ f2≠ε ∧ f5≠ε） | 5230（沿用 #194 §2） |
| catalog 发布成员（5224 = 5230 − 6 缺名） | 5224（沿用 #194 v2） |
| catalog ∩ `.day` member | **5219** |
| catalog 中无 `.day` member（item 预期失败） | 恰 5：`sh601091, sh688837, sz001246, sz301686, sz301716` |

spot-check 冻结值（3 symbol，覆盖 sz+sh；member sha256 与首/末 QFQ
bar 由被测管线在 retained 字节上导出，Phase B 脚本逐字段断言）：

| symbol | member | member_bytes | member_sha256 | qfq_bars |
|---|---|---|---|---|
| `sz.000002` | `sz\lday\sz000002.day` | 271328 | `6f168cf053ca561fedde1aa97c3b38d71876854ade3dae5f1a2c9ba9f6b63826` | 8479 |
| `sh.600006` | `sh\lday\sh600006.day` | 206944 | `77ecaea34c1cc5b08572c4816962b2f8d97d082b545c6034e86e99ecf126655a` | 6467 |
| `sh.600004` | `sh\lday\sh600004.day` | 180160 | `49656073176fea18fca2e8131e7159a0c91952cdbe9994856d2b60f42471b7eb` | 5630 |

冻结首/末 QFQ bar（`open/high/low/close`，volume/amount 恒 null）：

| symbol | first | last |
|---|---|---|
| sz.000002 | 1991-01-29 `-8.83/-8.83/-8.83/-8.83` | 2026-09-14 `3.05/3.07/3.03/3.05` |
| sh.600006 | 1999-07-27 `1.16/1.17/0.92/0.94` | 2026-09-14 `5.19/5.39/5.19/5.36` |
| sh.600004 | 2003-04-28 `0.96/1.56/0.94/1.55` | 2026-09-14 `7.71/7.79/7.70/7.76` |

sz.000002 冻结锚点 bar（既有 oracle）：2000-01-10
`-8.14/-8.06/-8.15/-8.06`。

## 4. 冻结序列（S0–S8，单个 scratch 库）

- **S0** 环境留痕：`git rev-parse HEAD`、`cargo --version`、二进制
  sha256、manifest+三 archive sha256 实测 == §2 pin。
- **S1** catalog 先行发布（v2 已验收路径）：
  `start_collection` `securities.catalog` → `run_worker_once` →
  `catalog_members`=5224、`catalog_versions.source`==
  `official-file:b8a0d7b9aec1a270`。catalog version_id 为运行期值，
  本计划不冻结字面值；断言其 digest/身份语义与行数。
- **S2** Catalog 选择拒绝腿（BLOCKED_ON_IMPL 证据）：
  `start_collection` `{"selection":{"kind":"catalog","version_id":<S1 发布值>}}`
  → 受理拒绝 verbatim `INVALID_COMMAND: resolved selection budget
  1..100`，不产生 task。
- **S3** 53 片 explicit `start_collection`：catalog 成员排序后按
  ≤100 切片（顺序冻结为排序序），request_id `daily-000..daily-052`
  逐一受理，task_id/item_ids verbatim 留痕。
- **S4** `run_worker_once` 循环至 `idle:true`（每调用处理一个 task；
  预期恰 53 次非 idle），逐 task 结果 verbatim。
- **S5** 幂等：`daily-000` 同 request_id 重发 → `reused:true`、同
  task；新 request_id 同 selection 受理出新 task，执行时逐 item 失败
  verbatim `INVALID_MARKET_COMMAND: existing daily head requires the
  update_data range path`（首次采集契约：已有 head 不重复发布），无
  新版本写入。
- **S6** 读回：`query_data`/`get_data_availability`/`list_data_versions`
  对 spot-check symbol verbatim 入档；`data_versions` 计数 == 5219。
- **S7** 拒绝腿（独立 `db-r1.sqlite`）：无 env → `start_collection`
  受理照常，item 执行 verbatim `SOURCE_NOT_VERIFIED`，零版本零泄漏。
- **S8** 门禁留痕：受理 `blocking_reasons` verbatim；
  `NOT_PRODUCTION_READY` 不变。

## 5. 断言清单（D 系列；全部入 results.json）

| # | 断言 | 冻结口径 |
|---|---|---|
| D1 | 输入完整性 | §2 全部 sha256 实测 == pin |
| D2 | catalog 前置 | members=5224、source 绑定 verbatim、head 指向该 version |
| D3 | Catalog 选择上限 | S2 verbatim 拒绝（BLOCKED_ON_IMPL 如实记录） |
| D4 | 受理分片 | 恰 53 task；每 task items ≤100；合计 items 恰 5224 |
| D5 | item 结果 | 恰 5219 `success:true` + 恰 5 失败（§3 五个 symbol，error 含 `member absent or ambiguous` verbatim）；无其他失败 |
| D6 | 版本与 head | `data_versions`=5219；每 symbol `effective_heads` 恰一条指向其版本 |
| D7 | provenance 绑定 | 抽全部版本 `snapshot_inputs`：day/gbbq archive_sha256 == §2 pin，member_sha256 逐版本如实（spot-check 三 member == §3 表，其余断言结构形如存在且非空） |
| D8 | spot-check 内容 | 三 symbol published batch bars 与 §3 冻结 bars 数+首末 bar 逐字段相等；sz.000002 2000-01-10 bar == oracle 锚点 |
| D9 | 幂等/二次采集 | 同 request_id `reused:true`；新 request_id 新 task 逐 item verbatim `existing daily head requires the update_data range path`，`data_versions` 计数不变 |
| D10 | env-absent | R1 verbatim `SOURCE_NOT_VERIFIED`，零泄漏 |
| D11 | 门禁保留 | blocking_reasons verbatim；`NOT_PRODUCTION_READY` 不变 |
| D12 | 读回 | query_data spot-check 行数 == 各 symbol qfq_bars（分页合计）；availability/versions verbatim |

## 6. 时间预算与如实标注

- 实测（df063f5，本机）：`materialize` release ≈ 51ms/symbol（`.day`
  + QFQ，gbbq 解码一次 ~70ms）；debug ≈ 123ms/symbol（gbbq ~1.9s）。
  5219 symbol 物化下限 ≈ release 4.5min / debug 10.8min。
- **未测**：每 symbol 的 publish/校验/SQLite IO 开销——5224 量级
  端到端时间未知，本次运行即测它；若单 task 全量超时/中断，
  观察留痕后先修 PLAN 再重跑。
- 每 item 失败（无 member）不阻塞其他 item——`TdxDailyRunner`
  per-item outcome 语义，断言逐 item 而非整 task。
- 若实现观察到结构性偏离（item 数 ≠5224、成功数 ≠5219、出现非预期
  失败类别）：停跑、先修 PLAN、再干净重跑。

## 7. 产物

`evidence/issue-4-daily5230-acceptance/` 下 Phase B 追加：
`run_daily5230_acceptance.py`、`run-log.jsonl`（verbatim）、
`results.json`（D1–D12 + blocked_paths + 全部 id/hash）、`README.md`。

## 8. 修订 v2 — 首跑中止与 release 重跑（#204 之后）

**首跑中止（留痕，不纳入断言）**:debug build 首跑于
`2b7a0c4` 运行 ~16.2h 至 3289/5224(62.7%）后中止。运行即测出
§6"未测"项：publish/校验段 ~17-18s/item，逐段插桩定位为
`validate_and_deduplicate` O(N²) × 每 item 6 遍全量校验（~93%)。
中止记录：`/home/box/acceptance-run-issue4/ABORTED.md`;
首跑 `run-log.jsonl` 保留。3 个 partially_failed 分片恰为 5 只
无 `.day` member 证券所在片，符合预期失败类别。

**修订项**（其余断言、机械期望、fail-closed 序列不变）:

- 构建改为 `TMPDIR=/dev/shm cargo build --locked --release --bins`。
  动机如实记录：修复后 debug ~2.8s/item(≈4h）仍在长窗口边缘，
  release ~0.66s/item（实测）使验收在常规窗口内完成；**这不是
  性能声明**，部署性能结论仍以实测为准。
- 重跑代码 = 合并 #204 修复后的 HEAD(S0 留痕记录实际值，
  本计划不冻结字面值）。#204 为纯等价变更：dedup 冲突语义、
  重复计数、排序输出、发布内容逐字节不变，等价性由既有断言
  与新增单测强制。
- scratch 库全新重建（`db.sqlite` 重新 init)；首跑库文件不迁移、
  不复用，仅作中止证据保留于 scratch 目录。
- 时间预期更新：物化 ~51ms/symbol(release)+ 实测发布段
  ~0.5s/item → 端到端预期 ~0.5h（外推，本次运行实测之）。
