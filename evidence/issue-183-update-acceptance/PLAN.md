# Issue #183 Phase A — `update_data` official-file 真实执行验收计划（freeze）

**状态：PLAN FROZEN — 先计划后观察。** 本文件冻结输入、证券、服务入口、
执行序列、断言与拒绝路径；不含运行产物。运行脚本与结果在后续执行切片
提交；运行不得偏离本文件，如需变更先改本计划再跑。

## 0. 冻结时状态与前置

- 冻结基线：main `90ac99f85b492c5a4041908c8062773d0b889c16`。
- 冻结时 `OfficialRangeProvider`（`src/market_update/runner.rs`）先调
  `OfficialFilePipeline::materialize_daily`——其恒返回
  `SourceError::NotVerified("NOT_PRODUCTION_READY; publish blocked; <五条
  PRODUCTION_BLOCKERS>")`——再返回 `NotVerified("range evidence verifier
  and combined coverage are not implemented")`。即 **update 的
  official-file 真实执行尚未落地（Phase B 未完成）**。
- 因此：本计划 S5–S9 的"successful no-change"期望在 Phase B 落地后才可
  验证。若执行时实现仍未落地，item 结果为
  `NOT_PRODUCTION_READY; publish blocked; …LOCAL_PIPELINE_NOT_IMPLEMENTED…`
  verbatim 留痕，`results.json` 对受影响断言标 `BLOCKED_ON_IMPL`，
  不伪造 PASS。
- #92/#147/#155 已实现的更新范围、幂等、fail-closed 规则不在本计划
  重新验证其业务正确性；本计划只验证官方文件源真实执行接线与
  no-change 语义。
- 排除项（#183 明确）：backfill/`verify_data_gap`/proven-gap、5230 全
  目录、部署、定时调度、新数据源、QFQ/GBBQ 再研究。

## 1. 固定输入

| 输入 | 固定值 |
|---|---|
| 快照根 | `/home/box/acceptance-run-issue3/snapshot/`（retained 硬链接） |
| manifest | `evidence/issue-3-acceptance-run/snapshot-manifest.json`，sha256 `b8a0d7b9aec1a270549d1e53ba26c0ab04842a23c05d1d1f7e75dd44a9f444d3`（#178 已入库，复用不重造） |
| base.zip | sha256 `dcdf6b49898926675698263535021aed7ad92b34be82a1a9e8a2a8982bd68bc6` |
| gbbq.zip | sha256 `daad6290c4f1cd478d7436b1e7077260a3e3e52cefd7eae58e4f24878fa2d0e9` |
| hsjday.zip | sha256 `c82f852a7bc2e52c7e89347a1e0d1878b01ce997946c2745f7af323752270892` |
| 服务入口 | `task_sqlite init <db>` + `mcp_data <db>` JSON-RPC stdio |
| env | `X_DATA_MCP_OFFICIAL_MANIFEST` + `X_DATA_MCP_OFFICIAL_DIR` |
| 调度 | 一次性手工 `run_worker_once`，无守护无重试 |
| 构建 | `cargo build --locked --bins`，rustup stable 1.98.1；记录 `--version`、head sha、UTC 时间 |

不下载新文件；只读 retained gen-2。

## 2. 固定证券与选择器

- **sz.000002**：base daily 发布 + update 主目标。
- **sh.600006**：base daily 发布，仅用于 §6-R3b 跨证券 base 身份不符
  拒绝（其版本 id 作为 sz.000002 请求的 `base_version_id`）。
- 禁止扩到全目录；不使用 default selector——`selection.kind=default`
  依赖有效 catalog 成员 ∩ 执行起点标记，而 catalog 在 P1 裁定未决下
  无法发布（`reject_candidate` 拒绝 6 缺名成员），default 解析注定
  `EMPTY_SELECTION`，无法构成本 slice 的 bounded update 输入。
  **故冻结 `update_data` explicit-range contract（`explicit-range-task-v1`）**，
  wire 形如：

```json
{"request_id":"<rid>","dataset":"market.daily","contract_version":"v1",
 "items":[{"entity":{"exchange":"sz","code":"000002"},
           "base_version_id":"<本运行 sz.000002 day head>",
           "start_date":"1991-01-29","end_date":"<acceptance date>"}]}
```

- `start_date` 冻结为 base `first_date` = `1991-01-29`——镜像已合并的
  default-selector refresh 语义 `refresh_range=[base.first_date, today]`
  （#147），对固定快照做全范围重取，no-change 证据最强且无时钟依赖下
  界。
- `end_date` = 运行当日 Asia/Shanghai 日期（`shanghai_today`），逐字记入
  run-log；必须 ≥ `2026-09-14` 且 ≤ 运行当日（未来日守卫）。若运行日
  恰使 `end_date < start_date`（实际不可能，最早 2026-09-16+），记失败
  不调整。

## 3. 期望值口径（观察前写死）

- **no-change**：对同一 pinned 快照重取同一证券同一范围，产出 batch 与
  base 版本逐 bar 一致 → `Decision::NoChange{version_id=base}` →
  receipt `version_id` == base version id；`data_versions` 该证券
  period=day 仍恰 1 行；`effective_heads` 不变；item `success:true`。
- **changed-version update**：仓库中不存在第二个已钉死且同合同可比较的
  官方 daily snapshot generation（gen-1 永久不可得）→ 本 slice
  **NOT_RUN**，如实记录，不伪造不 synthetic 冒充。
- **读回不变**：update 前后 `query_data` 读回逐字段一致；两条 oracle
  spot bar 冻结（#178 已对 retained gen-2 独立验证）：
  - sz.000002 `2000-01-10`：open `-8.14` high `-8.06` low `-8.15`
    close `-8.06`
  - sz.000002 `2026-09-14`（末 bar）：读回值与 base 版本读回逐字段一致
    （具体值取自本运行 S3 读回并记录，断言为前后不变而非外部常数）。
- **volume/amount**：全 NULL 不提升。
- **拒绝**：§6 三/四类各 verbatim 入 `results.json.blocked_paths` 与
  run-log，且各 scratch 不得新增任何 market.daily 版本。

## 4. 固定执行序列（主 scratch 库 `db-main`）

- **S0** 记录环境：`git rev-parse HEAD`、`mcp_data --version`/构建信息、
  UTC 起止、三 zip 实测 sha256 复核 manifest、manifest sha256。
- **S1** `task_sqlite init db-main`；`mcp_data` 注入两 env；`initialize`。
- **S2** `start_collection` market.daily v1 explicit selection
  `[sz.000002, sh.600006]` → `run_worker_once` → 两 item `success:true`；
  记录 task/item/publication receipt/`data_versions` 行（version_id、
  record_count、first/last_date、provenance.snapshot_inputs）。
  期望（观察前冻结）：sz.000002 8479 bars（1991-01-29→2026-09-14）、
  sh.600006 6467 bars（1999-07-27→2026-09-14）。
- **S3** `query_data`/SQLite 直读记 base 快照：effective_heads day
  version_id、全量 bar 读回哈希（sha256 over canonical rows）、
  `data_versions`/`publication_receipts` 全行。
- **S4** `update_data`（§2 wire，`request_id=u1`）→ 记录受理响应
  （task_id、item frozen_inputs=`{base_version_id,base_integrity_sha256,
  range}` 逐字）。
- **S5** `run_worker_once` → item 终态；记录 `task_items` outcome、
  `publication_receipts`、`data_versions` 增量。
- **S6** 同 `request_id=u1` 同参数重发 → `reused:true` + 同 task_id。
- **S7** 新 `request_id=u2`、同 frozen inputs（同 items 数组）→
  `run_worker_once` → 仍 no-change。
- **S8** 读回复核：`query_data` 两 oracle bar、`list_data_versions`、
  `effective_heads`、bar 读回哈希 —— 与 S3 快照逐项一致。
- **S9** provenance 复核：base 版本 `snapshot_inputs` archive/member
  sha256 与 retained 实测一致；update receipt 的 `version_id` 指向 base。

## 5. 固定断言清单（编号；全部入 `results.json`）

| # | 断言 | 期望（冻结口径） |
|---|---|---|
| A1 | 输入完整性 | 三 zip + manifest sha256 实测 == §1 pin；不一致即整体前置失败 |
| A2 | base 发布 | S2 两 item `success:true`；bar 数 8479/6467、首末日与冻结值一致；provenance snapshot sha == retained 实测 |
| A3 | update 受理 | S4 返回 task_id；item `frozen_inputs` 恰含 `base_version_id`（=S3 head）、`base_integrity_sha256`（=base raw digest）、`range={start:1991-01-29,end:<run date>}` |
| A4 | update 执行成功 | S5 item outcome `success:true`；若 Phase B 未落地 → `BLOCKED_ON_IMPL`，verbatim 记录拒绝串，A5–A9 同标 |
| A5 | no-change 复用版本 | update receipt `version_id` == base version id；该证券 `data_versions` day 行数仍 == 1（无第二版本） |
| A6 | execution/result 留痕 | `publication_receipts` 新增一行绑定 task_id/item_id/run_id/version_id=base |
| A7 | 同 request_id replay | S6 `reused:true`，task_id == S4 task_id，无新 item 执行 |
| A8 | 新 request_id 幂等 | S7 仍 `success:true` + receipt `version_id`==base；版本行数仍 == 1 |
| A9 | 读回不变 | S8：两 oracle bar 逐字段 == §3；`effective_heads` version_id 不变；bar 读回哈希 == S3 |
| A10 | provenance 可追溯 | 版本 `snapshot_inputs`（archive+member sha256）== §1 retained 实测；manifest sha == pin |
| A11 | env-absent 拒绝 | §6-R1 verbatim 入档；scratch 无新 market.daily 版本 |
| A12 | uninspectable manifest 拒绝 | §6-R2 verbatim 入档；无新版本 |
| A13 | base 不存在拒绝 | §6-R3a verbatim 入档；accept 即拒绝，无 task/版本 |
| A14 | base 身份不符拒绝 | §6-R3b verbatim 入档；accept 即拒绝，无 task/版本 |
| A15 | 门禁保留 | 受理/执行证据中 `PRODUCTION_BLOCKERS` 五条与 `NOT_PRODUCTION_READY` 原样保留；本 slice 不解除任何 gate |

## 6. 拒绝路径（各独立 scratch，verbatim 留痕）

- **R1 env-absent**：scratch `db-r1`——先以 env 启动服务 `start_collection`
  发布 sz.000002 base（同 S2），停服务；**不注入 env** 重启 `mcp_data`，
  `update_data` 同 §2 wire（base_version_id 用已发布 head）→
  `run_worker_once` → item 失败 verbatim 记录（当前实现期望
  `NOT_PRODUCTION_READY; publish blocked; …`；Phase B 后期望官方源未
  安装的等价 SOURCE_NOT_VERIFIED 拒绝——**两口径均接受，以 verbatim
  实际输出为准，仅断言"无新版本+非空拒绝串"**，不一致如实报告）。
- **R2 uninspectable-manifest**：scratch `db-r2`，同 R1 流程但重启时
  `X_DATA_MCP_OFFICIAL_MANIFEST` 指向 `{"files":[]}` 畸形 manifest →
  `from_env` 不安装 → 同 R1 断言。
- **R3a base 不存在**：scratch `db-r3`，env 正常，直接 `update_data`
  `base_version_id="collection-version-"+64×"0"`（不存在）→ accept 期
  拒绝 verbatim（当前代码路径：`fixed_many` 短少 →
  `incomplete fixed base lookup`，或 NotFound →
  `explicit published daily base unavailable`；以实际 verbatim 为准），
  无 task/版本入库。
- **R3b base 身份不符**：同 `db-r3`，`update_data` entity=sz.000002 但
  `base_version_id`=sh.600006 的已发布版本 → accept 期
  `fixed daily base identity mismatch` verbatim；无 task/版本。
- R3a/R3b 前需在 `db-r3` 发布 sh.600006 base（R3b 依赖）；R3a 不依赖。

## 7. 产物

`evidence/issue-183-update-acceptance/`：本 PLAN、`run_update_acceptance.py`
（执行切片）、`run-log.jsonl`（每步请求/响应/时间戳 verbatim）、
`results.json`（A1–A15 逐条 + blocked_paths + 全部 id/hash）、
`README.md`（运行身份、序列结果、断言汇总、BLOCKED_ON_IMPL/NOT_RUN 如实
标注）。

## 8. 纪律

- 断言期望 §3/§5 在首次观察运行输出前冻结；运行中观察到的任何偏离
  （包括 Phase B 实现的具体拒绝措辞、receipt 字段名差异）只如实记录，
  不回改期望——需要结构性变更时先修订本 PLAN 再重跑。
- 不改 src；不动 gate；不使用 retained 之外的输入；不伪造 changed
  update；不发布 catalog；不触碰 backfill。
- 全部 market.daily 版本计数断言以 `data_versions` 表为准，
  `effective_heads` 以表读回为准，`query_data` 为服务读回旁证。
