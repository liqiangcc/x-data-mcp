# 来源库（source store）

来源库保存 fresh-init 无法再生的原始字节：历史 `tdxhy.cfg` / `incon.dat`
抽取物、冻结的交易所响应，以及一次性导入的 owner 归档成员。设计见
[issue-446 M3 设计](tasks/issue-446-m3-delisted-industry.md) §9.4；本文只记录
PR-1a 落地的库格式、入口与当前边界。

## 1. 权威关系

- **blob + `manifest.jsonl` 是事实源**。SQLite 里的 `industry_snapshots`、
  `exchange_source_snapshots`、`generation_facts` 是索引，可由来源库重建。
  这三张索引表与 `industry_assignments`/`retire_exemptions` 由 schema 25（M3 PR-1）建出；
  写入器在 PR-2/PR-6/PR-8。PR-8 起 `generation_facts` 由行业 item、日更派生路径登记，
  并可由 `replay --scope generation_facts` 重建；`retire_exemptions` 只由
  `source_store exempt` 写入，退役路径不会自豁免。
- 写入顺序固定：blob 落盘、`fsync`、清单追加、`fsync`，之后才允许写索引行。
  「blob 在、清单在、索引缺」是允许的中间态；「索引有行、blob 不可读」不可接受。
- 读面、重放和退役门禁一律按 sha256 读回校验；不符即
  `source_store_corrupt`，fail-closed，不自动重抓（重抓得到的是新事实）。
- 清单末行缺换行即 `manifest_torn_tail`；不猜、不截断，等 owner 显式修复。
- 只增不删、不覆盖。按内容去重；同一字节来自新的证据出处时只追加一条清单行。

## 2. 位置与布局

位置由部署配置/命令参数给出：部署状态目录下、与 generation 根同级，例如
`<deploy-state>/source-store/`。代码与文档都不写本机绝对路径；CLI 用
`--store <dir>` 显式传入，部署脚本传 `$DEPLOY_ROOT/state/source-store`。

```text
<store>/
  blobs/<sha256>       原始字节，内容寻址；只增不删、不覆盖
  manifest.jsonl       只追加清单，一行一条登记
  tmp/                 写入暂存（fsync 后发布进 blobs/）
  .lock                flock 锁文件（写者排他，读清单共享）
```

发布用「临时文件 → `fsync(file)` → hard link 到 `blobs/<sha256>` →
`fsync(dir)` → 追加清单 → `fsync(manifest)`」。hard link 在目标已存在时失败，
因此不会覆盖既有 blob；临时名随后删除。任一步失败都不写索引行。首次创建
`blobs/`、`tmp/`、`manifest.jsonl` 后还会对来源库根目录 `fsync`，保证新目录项
落盘。

**并发**：`register` 全程持有 `<store>/.lock` 的 `flock` 排他锁（查重、发布
blob、追加清单与两次 fsync 都在锁内），因此多个写者不会交错写入，也不会把
同一 `(sha256, source_id, origin)` 登记两次；`replay`/`verify`/`import` 读清单
时取共享锁。清单行把「JSON + `\n`」拼成一个缓冲、一次 `write_all`（`O_APPEND`），
单行不会被另一个写者插入。

**残尾（torn tail）**：崩溃可能留下没有换行结尾的末行。读路径识别为
`manifest_torn_tail`（含行号）并 fail-closed，写路径在取得排他锁后遇到同一
情况也拒绝写入；**不自动截断**，修复是 owner 的显式操作（与 §9.4.5 的损坏处理
一致）。

## 3. 清单字段

| 字段 | 语义 |
|---|---|
| `sha256` | 字节内容 sha256（hex），即 blob 文件名 |
| `source_id` | 逻辑来源，如 `tdx.base.zip!tdxhy.cfg`、`sse_listing` |
| `store_kind` | `generation_member` / `http_fetch` / `archive_import` / `repo_evidence` |
| `origin` | 证据出处：generation manifest sha / 请求指纹 / 批次 id + 条目路径 |
| `acquired_at` | 首次拿到该字节的时间（UTC） |
| `publisher_last_modified` | 发布方 `last_modified`（可空） |
| `evidence_grade` | 行业证据等级 `A`/`B`/`C`（交易所来源留空） |
| `basis_status` | 交易所来源分档（行业来源留空） |
| `parser_version` | 重放必须使用的解析器版本；未知版本 fail-closed |
| `size_bytes` | 字节数 |
| `container_sha256` | 抽取物所属容器的 sha256（可空） |
| `import_batch` | 一次性导入批次 id（可空） |
| `imported_at` | 该字节**首次进入本系统**的导入时间（可空；只在一次性导入时写，见 §5 的 `known_at`） |
| `http_etag` | 交易所响应的 `ETag`（可空；`Last-Modified` 复用 `publisher_last_modified`，见 [交易所来源入口](exchange-source.md) §5） |

去重键是 `(sha256, source_id, origin)`：重复执行同一导入报 `already_present`；
同一字节换了出处则追加一行、blob 不重复。

## 4. 入口形态：CLI（PR-1a 选定）

PR-1a 在 maint socket 工具与等价 CLI 之间选择了 **CLI**
（`source_store` 二进制）：

- maint 入口是只读运维/取证面，`data_service` 不为它开启写路径；来源库写入
  属于离线运维动作，不该绕过单进程写协调者塞进 MCP 工具。
- 导入是对 owner 私有归档的一次性操作，本来就在服务之外执行；部署钩子也
  直接运行 release 里的二进制。
- 不改变三个静态 allowlist、capability revision 与文档里的工具数事实。

```text
source_store replay --store <dir> \
  [--scope industry|exchange|generation_facts|risk_warnings|security_names|suspensions|all] \
  [--db <market.sqlite>] [--verify-only]
source_store verify --store <dir> \
  [--scope industry|exchange|generation_facts|risk_warnings|security_names|suspensions|all] \
  [--db <market.sqlite>]
source_store import --store <dir> --index <archive-index.jsonl> --blobs <dir> \
  [--tiers A,B] [--kind industry|exchange] [--batch <id>] [--dry-run]
source_store exempt --db <market.sqlite> --generation <manifest sha256> \
  --missing <key> --reason <text> --recorded-by <who> [--owner-approve-evidence]
```

输出为单个 JSON 对象；退出码 0 表示 `ok` 或 `store_empty`，1 表示
`source_store_corrupt` / `unknown_parser` / `drift` / 重建失败，2 表示用法错误。

`replay`/`verify` 按 scope 选择清单记录、逐 sha 读回校验，并输出按固定字段
排序后的规范化摘要。scope 的写入器到位情况：`exchange` 自 PR-2 起可从清单
重建 `exchange_source_snapshots`（见 [交易所来源入口](exchange-source.md) §4），
`industry` 自 PR-6 起可重建 `industry_snapshots`/`industry_assignments`，
`generation_facts` 自 PR-8 起重建设备退役门禁索引（行业成员 + 已绑定的派生事实），
`risk_warnings` 自 PR-10 起可重建 `risk_warning_intervals`，`security_names`
自 PR-12 起可重建 `security_name_history` 与它的 `derived_dataset_versions`
登记（整表替换，走日更 item 的同一写入器）；`suspensions`（XDM-138）可重放
停牌数据集，但它是**条件可重放**：装配还依赖该库的主表、交易日历和 bar_days，
详见 §4.2。
`rebuilt` 取值：
`nothing_selected`（该 scope 在清单里没有匹配记录，什么都没算、没写）、
`skeleton_no_op`、`computed_no_db`（算出了摘要但没有 `--db`）、`rebuilt`（已写库）、
`not_rebuilt`（条件 scope 的库内输入缺失，fail-closed，表不动）。
空库返回 `store_empty`，不算失败。

`--db` 是「只算」与「重建」的分界：不给数据库时只计算并报告摘要，不打开也
不创建数据库；给了以后 scope 在单个事务里替换自己的索引表，写后回读并与
重算摘要比较，`match` 才算成功，否则输出 `drift` 与前 20 条差异主键。
`--verify-only` 只重算并比较、不写。重放在服务启动前执行，因此直接取数据库
租约，不和在线写入竞争。`generation_facts` 的报告多一个 `facts` 计数（重放
后索引的总行数），`security_names` 多一个 `as_of`（沪/北 `NOT_AVAILABLE` 窗口的
结束日 = 冻结表最后一个变更日，由字节算出）；摘要不含登记时间，清空表后重放应
得到同一 `summary`。

`industry` 的时间线只由清单里**带发布方 `last_modified` 的登记**构成观测（见
[行业快照设计](tasks/issue-446-m3-delisted-industry.md) §5.6.1）：相邻且内容相同的
观测折叠为一段区间，内容变化（包括变回旧内容，A → B → A）就开新区间；没有
`last_modified` 的登记只是同一份字节的副本，不占时间线位置，只有整份内容都没有
发布方时间时才取最早获取时间作为唯一一次回退边界。同一内容的多条登记合并成一个
`industry_snapshots` 行（§5.3），时间与等级取该内容全部登记的最优值。

### 4.1 `security_names`：可重放的 as-of

`security_names` 只选 `store_kind=http_fetch` 且 `source_id=szse_short_name` 的
登记，并且只重放**最近一次采集**的那组页（深交所简称表是整市场快照，不是追加
日志；重放旧页集会把同一条变更链重复一遍）。一次多页抓取的所有页共用同一个
`acquired_at`（PR-2 的 `freeze_bytes(fetched_at)`），所以一组就是一套完整页集。
已知限制：清单没有「采集是否完整」字段，多页抓取在后面的页失败时，已冻结的前几页
仍留在库里（先冻结后解析）且带同一个 `acquired_at`，重放无法把它与完整页集区分；
日更写入端不会写这种半套结果。写入走 `derived:security_names`
的同一写入器：一个事务里整表替换 `security_name_history`、登记
`derived_dataset_versions`、绑定 generation。

覆盖窗口（`coverage`）是登记证据的一部分，所以它必须由字节算出，不能由
**当天日期**算出：深/沪/北三档里沪、北是 `NOT_AVAILABLE` 的未观测窗口，如果把
它的 `to` 写成写入当天，同一份冻结字节每重放一次就产生新的证据摘要。现在写入端
与重放端共用同一个组装函数，把沪/北未观测窗口的结束日固定为**该表最后一个变更
日**（报告的 `as_of` 字段就是这一天）；采集日仍是来源库清单的事实
（`acquired_at`），不属于数据集证据。`--verify-only` 重算行 **与覆盖窗口** 并
与库内登记比较，覆盖窗口被重新盖章也算 `drift`。

两类「没有数据」都不写成「空表已重建」：清单里没有 `szse_short_name` 记录时
scope 状态是 `store_empty`、`rebuilt=nothing_selected`，不碰数据库；有记录但
冻结表没有任何 A 股变更行时 fail-closed（`security_names_rebuild_failed`，
reason 码 `empty_name_set`），不写库、上一版仍可读。

### 4.2 `suspensions`：条件可重放

`source_store replay --scope suspensions` 只选 `store_kind=http_fetch` 且
`source_id=sse_suspension` 的冻结页（A-3），用与 `--scope exchange` 相同的清单 →
索引重建再解析 facts。confirmed 行本身来自来源库，但整表装配还需要**该库**的
`security_master`、已提交交易日历和每实体 bar_days——它们是行情数据，不是来源库
字节，所以这个 scope 不是无条件可重放：

- **无 `--db`**：只解析冻结页并报告 `facts` 计数（`rebuilt=computed_no_db`），
  不装配、不打开也不创建数据库。
- **有 `--db`**：从库读出三样输入，与日更 `derived:suspensions` 调用**同一个**
  `assemble_suspensions`、**同一个** `commit_suspensions` 整表替换并登记
  `derived_dataset_versions`；generation binding 直接沿用 `security_master` 的，
  确保停牌 gap 与它所依据的 bars 属于同一代。
- **fresh-init**：库还没有主表/日历 → fail-closed，状态与 reason 码都是
  `suspension_replay_not_ready`，表不动；**不写空表冒充成功**。正确顺序是先重建
  主表与日历（日更或对应写入路径），再显式运行本 scope；或直接跑日更
  `derived:suspensions`。
- **确定性**：`summary` 是已写行的 `suspensions-v1` 摘要；同一来源库 + 同一库状态
  重放两次逐位相同。库状态前进会合法地改变结果：三样输入哈希成 `inputs_digest`
  并写进登记证据，`--verify-only` 因此能报 `inputs_match=false` / `drift_cause=inputs_changed`
  （不是 `source_store_corrupt`）；输入未变而行不同才报 `rows_changed`。
- **不进入 `--scope all`**：all 在 fresh-init、服务启动前运行，那时三样输入必然
  不存在；把条件 scope 放进 all 会让每次 fresh-init 都失败。`all` 只覆盖不依赖
  M2 库内输入的 scope。

`--scope suspensions` 的 `--db` 写入仍需服务停止（重放取数据库租约），与其它写库
scope 相同。

## 5. 退役豁免

退役门禁（[M3 设计](tasks/issue-446-m3-delisted-industry.md) §8.3）默认拒绝：
来源库没有按 sha 读回的证据，或 `generation_facts` 缺必需 kind。确有外部原因时，
owner 用 `source_store exempt` 写一行豁免，再重试退役：

- `--missing` 用门禁报告里的键：`source_store_evidence:<role>:<source_id>` 或
  `derived_fact:<role>:<fact_kind>`。
- `--reason` 与 `--recorded-by` 必须非空；时间由命令写入。
- 来源库证据的豁免必须再加 `--owner-approve-evidence`，否则命令拒绝（退出码 2）。
- 豁免只覆盖列出的键，不覆盖 active/pin/in-flight 检查；退役成功后回复
  `detail=exemptions: ...` 指明用到了哪些豁免。
- 门禁本身绝不写豁免；Skill/自动流程也不得代为豁免（见
  [日更文档](daily-sync.md)）。

## 6. 一次性导入（归档索引）

`--index` 是 JSONL，一行一份归档批次；`--blobs` 是容器所在目录，索引里的
`source_path` 必须是其下的相对路径。每行示例（字段含义见代码
`src/source_store/import.rs`）：

```json
{"batch":"owner-archive-1","kind":"industry","tier":"A",
 "source_path":"2026-10-24/base.zip","sha256":"<base.zip sha256>",
 "tdxhy_sha256":"<member sha256>","manifest_sha256":"<snapshot manifest sha256>",
 "manifest_match":true,"last_modified":"2026-10-24T01:00:00Z","etag":"\"...\"",
 "acquisition_completed_at":"2026-10-24T02:00:00Z",
 "gbbq_source_path":"2026-10-24/gbbq.zip","gbbq_sha256":"<gbbq.zip sha256>",
 "gbbq_last_modified":"2026-10-24T01:00:00Z","incon_sha256":"<incon.dat sha256>"}
```

只存抽取物（`base.zip!tdxhy.cfg`、`gbbq.zip!zhb.zip!incon.dat`），不存整个容器；
容器 sha256 留在清单里作证据。校验顺序：tier/kind 过滤 → 容器 sha256 →
A 档 manifest 绑定 → 成员 sha256 → 完整解析（`tdxhy` / `incon` 解析器）。
不通过就进 `skipped_unverified`，逐条给出 `sha_mismatch` /
`manifest_mismatch` / `parse_failed` / `unknown_parser` / `member_missing` /
`invalid_path` 等原因；索引 JSON 语法错误是硬错误，不静默跳过。
`--dry-run` 执行全部校验但不写任何文件。

导入同时写 `imported_at`（本次导入时间）。行业区间回填的 `known_at` 取它而不是
归档的历史获取时间——否则等于声称当时已经用上了这份数据（设计 §5.2）；重复导入
命中 `already_present`，清单里保留第一次的 `imported_at`，重放因此稳定。

## 7. 部署钩子

`scripts/deploy_phase_g.sh` 在 fresh init 之后、**服务启动之前**调用一次
`source_store replay --store <state>/source-store --db <market db> --scope all`
（PR-6 起重放会写库，所以要拿数据库租约；原来的位置在服务就绪之后，会与服务
抢租约）。`store_empty` 记入报告的
`source_store_replay` 字段且不算失败；非 0 退出（例如 blob sha 读回不符）会让
部署失败，而不是部署一个「探活通过、历史为空」的实例。回滚不动来源库。

## 8. 验证边界

CI 覆盖写序故障注入（fsync/rename 各失败点无悬空清单行）、按 sha 读回不符 →
`source_store_corrupt`、重复导入不增 blob/行、重放两次摘要相同、空库
`store_empty`、含 manifest 不符条目的夹具进 `skipped_unverified`。
`suspensions` scope 另有端到端/单元测试：冻结页 → facts 计数、`--db` 装配与日更
写入逐位一致、`--verify-only` `match`/`drift`（含 `inputs_changed`）、缺日历 →
`suspension_replay_not_ready`、未知 `parser_version` fail-closed、空库 →
`store_empty`/`nothing_selected`。`security_names` scope 另有 6 条端到端测试
（`tests/source_store/security_names.rs`）：
清库后重放得到逐位相同的摘要与登记证据（`evidence_digest` 相同）、
`--verify-only` 为 `match`、篡改 blob → `source_store_corrupt`、未知
`parser_version` → fail-closed、空库/无 `szse_short_name` 记录 →
`store_empty`/`nothing_selected` 且不碰库、全程离线。测试全部使用
临时目录与合成归档，不联网、不依赖本机路径。

真实归档导入、`docs/STATUS.md` 的批次留痕和 fresh-init 后的
`store_empty → import → replay match` 验收，属于后续 PR 的私有栈验收范围。
