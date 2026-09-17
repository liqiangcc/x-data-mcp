# Issue #187 Phase A — proven-gap → official-file `backfill_data` 验收计划（freeze）

**状态：PLAN FROZEN — 先计划后观察。** 冻结输入、证券/target、受控
local-gap subject 构造规则、执行序列、断言与拒绝路径；不含运行产物、
不改 src。运行不得偏离本文件；需变更先改本计划再跑。

## 0. 冻结时状态与前置

- 冻结基线：main `00d190d2375a82cee36c3743dbc39b72c135b360`
  （含 #185 `TdxRangeProvider`、#155 `verify_data_gap` producer、
  #149–#154 proven-gap/covering/default-resolver 全套）。
- 冻结时 worker dispatch（`src/data_supply/worker.rs`）：
  `verify_data_gap` 恒用 `UnverifiedGapVerifier`（`GAP_BASIS_NOT_VERIFIED`
  fail-closed）；`backfill_data` 恒用 `OfficialRangeProvider`
  （`NOT_PRODUCTION_READY…` fail-closed）。Phase B/C 实现未落地。
- 因此本计划 §5 主序列的 decision/backfill 成功断言在 Phase B/C
  落地后才可验证；未落地则对应断言标 `BLOCKED_ON_IMPL`，verbatim
  记录拒绝串，不伪造 PASS。
- 排除项（#187 明确）：全目录、changed-generation update、多 gap/
  批量、部署、定时调度、新数据源、QFQ/GBBQ/Windows 再研究。

## 1. 固定输入

| 输入 | 固定值 |
|---|---|
| 快照根 | `/home/box/acceptance-run-issue3/snapshot/`（retained 硬链接） |
| manifest | `evidence/issue-3-acceptance-run/snapshot-manifest.json`，sha256 `b8a0d7b9aec1a270549d1e53ba26c0ab04842a23c05d1d1f7e75dd44a9f444d3` |
| base.zip | sha256 `dcdf6b49898926675698263535021aed7ad92b34be82a1a9e8a2a8982bd68bc6` |
| gbbq.zip | sha256 `daad6290c4f1cd478d7436b1e7077260a3e3e52cefd7eae58e4f24878fa2d0e9` |
| hsjday.zip | sha256 `c82f852a7bc2e52c7e89347a1e0d1878b01ce997946c2745f7af323752270892` |
| 服务入口 | `task_sqlite init` + `mcp_data` stdio + `X_DATA_MCP_OFFICIAL_MANIFEST`/`X_DATA_MCP_OFFICIAL_DIR` |
| 调度 | 一次性手工 `run_worker_once` |
| 构建 | `cargo build --locked --bins`，rustup stable 1.98.1 |

## 2. 固定证券与 target 日（已独立核实 retained 字节）

- 证券：`sz.000002` 唯一。
- **target = 2000-01-10**（`start_date=end_date`）。已在 retained
  `hsjday.zip!sz\lday\sz000002.day`（8479 条记录）中独立定位原始记录，
  record hex：
  `6e2d31012c040000780400001f0400007804000098e1174d6052d90000000000`
  （raw：date 20000110，O=1068/H=1144/L=1055/C=1144 分，
  amount f32 `159259008.0`、volume `14242400`——unit 未锚定不提升）。
- **冻结官方 QFQ bar**（观察前写死）：`O=-8.14 H=-8.06 L=-8.15
  C=-8.06`，`volume=null`，`amount=null`。来源链：raw 字节（上）+
  #169 已提交契约回归（000002 仿射链 24 事件逐字段 MATCH）+
  #178/#181 已发布版本读回三方一致。
- 对照日：`2000-01-08`（Saturday，确认不在 .day → source-limitation
  target）；`2000-01-11`（Tuesday，在 .day → no-local-gap target）。

## 3. 受控 local-gap subject 构造规则（冻结）

**判定：需要一个小 support 工具**——现有 wire/二进制均无法发布一个
"官方 bars 减一日"的部分覆盖版本（`start_collection` 恒物化全历史；
`create_demo` 只发静态 fixture；禁止直接改 SQLite 行）。计划指定新增
**acceptance-only seed 入口**（建议 `task_sqlite seed-gap-subject`
子命令或等价独立 bin，命名可在实现期调整，语义不可漂移）：

1. `TdxMarketSource::from_env()` 必须成功（env 不在 → 直接失败）；
   `materialize_daily("sz.000002")` 得真实官方 QFQ batch；
2. 从 records 中**恰删除 target 日 2000-01-10 一条**；不得改动任何
   其他 bar 的任何字段；不得伪造源端数值；
3. `coverage.status` 必须保持 `Complete`（`compat::check`/`read_record`
   读回契约要求，否则 subject 无法被 `freeze_subject_on` 接受），但
   `coverage.reason` 必须逐字披露：
   `"ACCEPTANCE-LOCAL-GAP-SUBJECT: official sz.000002 QFQ bars minus
   frozen target 2000-01-10; complete only for this declared scratch
   coverage, not complete source history"`（实现期可加日期参数化，
   披露语不得弱化）；
4. `provenance.source` 保持 `tdx-official-file-snapshot`、`snapshot_inputs`
   逐字保留（bars 确来自该快照；如实），`provenance.symbol`=
   `sz.000002`、`source_adjustment=qfq`、`source_period=day`；
   `published_at`/`acquired_at` 记真实时间；
5. 组装 `EffectiveVersion` + `raw["batch"]` + `legacy_digest`，
   `stage` → **真实受理一个 collection task**（`LegacyCollection`
   family，`start_collection/legacy_market_compound/task-acceptance-v1`，
   item scope `["market","sz.000002","day"]`——同时为 default selector
   提供 ever-started marker）→ Claim/StartItem →
   `Publisher::prepare`（`PublishIntent::Daily{expected_head:None}`）→
   `commit`。**不绕过 Publisher/task 治理**；产生的版本 digest 对自身
   （减后）内容自洽，`read_record` 可完整复核；
6. 同一 seed 调用还须 `publish_fixture` 一个**最小 fixture catalog**
   （source `fixture:acceptance-187`，仅成员 sz.000002：
   `SecurityRecord{symbol:"000002", exchange:Shenzhen, …}`，
   industry `Missing{reason:"acceptance fixture"}`）——default
   selector 需要 catalog ∩ ever-started 交集，official catalog 在
   P1 未决下无法发布；fixture 标签如实，不冒充官方目录；
7. seed 工具打印 subject `version_id`/`integrity_sha256`/bar 数
   （8478 = 8479−1）供 run-log 留痕。
8. 支持 `--drop-date <date>` 参数化（stale-head 拒绝腿需要再 seed 一个
   移动 head 的 subject-v2，见 §6-R6）。

**若实现期发现上述路径无法在不破坏 digest/治理的前提下构造，运行切片
必须 `BLOCKED_ON_TEST_SUBJECT_CONTRACT` 如实收尾。**

## 4. 固定执行序列（主 scratch `db-main`）

- **S0** 留痕：head sha、cargo --version、二进制 sha256、UTC 起止、
  三 zip 实测 sha256 复核 manifest。
- **S1** `task_sqlite init db-main`；env 注入；`initialize`。
- **S2** seed-gap-subject（§3）→ subject v1：`version_id`、digest、
  8478 bars、head 就位、ever-started marker + fixture catalog 就位。
- **S3** `verify_data_gap` `{request_id:"g1", target:{symbol:"sz.000002",
  period:"day", start_date:"2000-01-10", end_date:"2000-01-10"}}` →
  受理（subject v1 冻结进 frozen_inputs）→ `run_worker_once` →
  decision + proven_gap 原子提交（Phase B `TdxGapVerifier` 下期望
  `confirmed_missing_range`）。
- **S4** `get_data_availability`（single sz.000002）→ open
  confirmed gap `[2000-01-10,2000-01-10]` 可见；记录
  `gap_verification_decisions`/`proven_gaps` 全行 verbatim。
- **S5** `backfill_data` `{request_id:"b1", selection:{kind:"default"}}`
  → 首次受理冻结**恰好一个** item：base=subject v1，
  `proven_gap_ranges`=[该 gap]、无 `refresh_range` →
  `run_worker_once` → Phase C provider 真实补回。
- **S6** 断言发布：新 daily version（8479 bars）、旧 subject v1 保留、
  effective head 前进；补回 bar 逐字段 == §2 冻结官方 QFQ bar；
  范围外 bar canonical 读回哈希 == subject v1（除补回日行外）。
- **S7** `gap_coverings` 追加绑定新 version；`get_data_availability`
  open gap 消失；`proven_gaps`/`gap_verification_decisions` 历史行
  保留可审计。
- **S8** 幂等：同 `request_id:"g1"` 重发 verify → `reused`；同
  `request_id:"b1"` 重发 backfill → `reused`；新 request_id 同参数
  verify → decision replay no-op（同 decision_id 不再写）；新
  request_id `backfill_data` default → 无 open gap →
  `EMPTY_SELECTION` verbatim 拒绝，无新版本无新 covering。
- **S9** provenance：新版本的 `snapshot_inputs`/base 绑定、
  `market_update_results`（no_change=0）逐字段留痕；全量 bar 读回
  哈希 == retained 全历史参考（与 #178 已发布同证券全量一致）。

## 5. 断言清单（编号；全部入 `results.json`）

| # | 断言 | 期望（冻结口径） |
|---|---|---|
| B1 | 输入完整性 | 三 zip + manifest sha256 == §1 pin |
| B2 | subject 诚实构造 | v1：8478 bars、缺 2000-01-10、digest 自洽 `read_record` 通过、coverage.reason 含 `ACCEPTANCE-LOCAL-GAP-SUBJECT` 披露、provenance.source/snapshot_inputs 真实；catalog=fixture 标签 |
| B3 | verify 受理冻结 | item frozen_inputs 恰含 target + `subject_version_id`/`subject_version_digest`==v1 |
| B4 | decision+gap 原子 | `run_worker_once` 后 item `success:true`；`gap_verification_decisions` 一行 `conclusion=confirmed_missing_range`、`basis_kind=source_gap_verifier`、`subject_version_id`=v1、`basis_sha256` 自洽；`proven_gaps` 一行 `[2000-01-10,2000-01-10]` 绑定 decision evidence sha256。Phase B 未落地 → `BLOCKED_ON_IMPL` |
| B5 | availability 可见 | open confirmed gap 恰一条 |
| B6 | backfill 受理冻结 | `default` 受理恰好一 item；frozen `base_version_id`=v1、`proven_gap_ranges` 恰含该 gap（gap_id/evidence_digest/range verbatim）、无 `refresh_range` |
| B7 | backfill 执行成功 | item `success:true`；新 `collection-version-*`（≠v1）。Phase C 未落地 → `BLOCKED_ON_IMPL` |
| B8 | 补回值 | 新版本 8479 bars；`2000-01-10` 行 == `{-8.14,-8.06,-8.15,-8.06,null,null}` 逐字段；其余各行与 v1 逐字段一致（canonical hash 对比） |
| B9 | head/治理 | effective head==新版本；v1 仍 published 保留；`market_update_results` no_change=0 + result_version_id=新版本 |
| B10 | covering | `gap_coverings` 一行绑定 gap_id+新 version_id；availability open gap 消失；decision/gap 历史行不变 |
| B11 | 幂等 | §8 全部：reused×2、decision replay 不重复、second backfill `EMPTY_SELECTION`、版本数不增 |
| B12 | provenance | 新版本 snapshot_inputs archive/member sha == retained 实测；basis `artifact_sha256` 绑定 day member sha256 |
| B13 | env-absent verify | §6-R1 verbatim；无 decision/gap 行 |
| B14 | uninspectable verify | §6-R2 verbatim；无 decision/gap 行 |
| B15 | source-limitation | §6-R3：decision `conclusion≠confirmed_missing_range`（verbatim），无 proven_gap |
| B16 | no-local-gap | §6-R4：decision 如实（verbatim），无 proven_gap |
| B17 | subject 身份不符 | §6-R5：fail-closed verbatim，无 decision/gap |
| B18 | stale-head | §6-R6：verbatim 拒绝，无 decision/gap |
| B19 | backfill 无 gap | §6-R7：`EMPTY_SELECTION`（或等价 verbatim），无 task/版本 |
| B20 | backfill explicit 不借路 | §6-R8：explicit-range backfill item 仍走阻塞 provider，verbatim，无版本 |
| B21 | gap 已覆盖不再消费 | §8 末腿覆盖：covering 后 default backfill 拒绝，无新 covering/版本（与 B11 合并留痕） |
| B22 | 门禁保留 | 五条 `PRODUCTION_BLOCKERS` + `NOT_PRODUCTION_READY` 原样；`GAP_BASIS_NOT_VERIFIED` 在未安装 env 的服务上仍存在 |

## 6. 拒绝路径（各独立 scratch，verbatim 留痕）

- **R1 env-absent**：scratch `db-r1`：env 下 seed subject v1 → 停服务 →
  无 env 重启 → `verify_data_gap` 受理（subject 冻结是 DB 内读，受理
  不受 env 影响）→ `run_worker_once` → `UnverifiedGapVerifier`
  `GAP_BASIS_NOT_VERIFIED…` verbatim；decision/gap 表空。
- **R2 uninspectable manifest**：scratch `db-r2`，同 R1 但重启时
  manifest=`{"files":[]}` → `from_env` 不安装 → 同 R1 断言。
- **R3 source-limitation**：scratch `db-r3`：env 下 seed **完整**官方
  subject（seed 工具 `--drop-date` 缺省不删 bar 的全量模式——若实现
  不支持则直接 `start_collection` 发布全量亦可，如实记录选用路径）→
  `verify_data_gap` target=`2000-01-08`（.day 无此日）→ decision
  committed 但 conclusion 为 source-limitation 类（verbatim），
  `proven_gaps` 空。
- **R4 no-local-gap**：同 `db-r3`（全量 subject）→ target=`2000-01-11`
  → decision committed，conclusion=no-local-gap 类 verbatim，无 gap。
- **R5 subject 身份不符**：scratch `db-r5`：seed `--source fixture`
  变体（`provenance.source="fixture:acceptance-187"` 的 subject，
  同样缺 2000-01-10）→ `verify_data_gap` target=2000-01-10 →
  verifier 对 source identity/basis 不符 fail-closed verbatim，
  无 decision/gap。
- **R6 stale-head**：scratch `db-r6`：seed subject v1 → `verify_data_gap`
  受理（冻结 v1）→ **同一 DB 再 seed subject v2**（`--drop-date
  2000-01-11` 删另一冻结 bar，head 前进到 v2）→ `run_worker_once` →
  commit 前 stale guard 拒绝 verbatim，无 decision/gap；v1/v2 版本
  均在库可审计。
- **R7 backfill 无 frozen gap**：scratch `db-r7`：env+seed 全量
  subject（无 gap）→ `backfill_data selection=default` → 受理期
  `EMPTY_SELECTION` verbatim。
- **R8 explicit backfill 不借 official provider**：`db-main`（或 db-r7）
  上 `backfill_data` explicit-range（合法 base+range）→ 执行仍走
  `OfficialRangeProvider` → `NOT_PRODUCTION_READY…` verbatim，
  无新版本。
- **"committed range 未整段覆盖 frozen gap → 不写 covering"**：
  如实标注 `NOT_OBSERVABLE_VIA_REAL_SOURCE`——committed range 按
  构造恒等于 frozen union，真实源下无法诚实地产生部分覆盖；该不变量
  由单元测试覆盖（`ensure_default_coverings` 边界），本验收不伪造。

## 7. 产物

`evidence/issue-187-backfill-acceptance/`：本 PLAN、
`run_backfill_acceptance.py`、`run-log.jsonl`（verbatim 全步）、
`results.json`（B1–B22 + blocked_paths + 全部 id/hash）、
`README.md`（运行身份、序列结果、BLOCKED_ON_IMPL/NOT_RUN/
NOT_OBSERVABLE 如实标注）。

## 8. 纪律

- 期望值 §2/§5 在首次观察运行输出前冻结；观察到的偏离只如实记录，
  需结构性变更先修 PLAN 再重跑。
- 不改 src（本切片只交 PLAN）；seed 工具为 Phase B/C/执行切片的新增
  support 代码，必须 acceptance-only、不得成为生产路径或绕过 gate。
- subject 是明确标注的 scratch acceptance 制品；不得冒充完整官方
  历史；不下载/修改 retained 字节；不使用 synthetic source 冒充真实
  backfill 输入。
- 全部版本/decision/gap/covering 计数以 SQLite 表读回为准，
  `query_data`/`get_data_availability`/`list_data_versions` 为服务读回
  旁证。
