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
source_store register-generation-member --store <dir> \
  --generation <manifest sha256> --generation-dir <installed generation dir>
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
`risk_warnings` 自 PR-10 起可重建 `risk_warning_intervals`（选择与日更一致：
同指纹取最新回答、简称表须为完整页集，XDM-154），`security_names`
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
登记（深交所简称表是整市场快照，不是追加日志；重放旧页集会把同一条变更链重复
一遍）。选择与日更 item 一致（XDM-154，设计 §5.5 AC-M3-9）：

- **每个请求指纹取最新回答**：同一个 `origin`（页号是指纹的一部分）被回答两次，
  就是来源修订了这个窗口，只取 `acquired_at` 最新的那一份；并列时按 `sha256`
  定序，结果确定。旧回答仍然留在清单里，但不参与重放。
- **必须是完整页集**（§5.4）：`1..=pagecount` 的每一页都有选中快照、各页
  `pagecount` 一致、`recordcount` 一致且等于各页行数之和。任一条不成立就是
  `snapshot_incomplete`（fail-closed），不写库、上一版仍可读。这一条替代了原先
  「清单没有采集完整性字段、半套页集无法识别」的限制：半套页集不是更短的表，
  不能写成更短的名史。
- **残留限制**：清单同样无法标识「一次采集」——重新下载到相同字节的页按
  `(sha256, source_id, origin)` 去重，一次抓取不会在库里留下同一 `acquired_at`
  的一组页。所以重放只能按请求指纹组装「最新页集」，并用上面的计数校验发现中途
  停止的抓取；它分不出「页 1 已改、页 2 尚未重抓」这种计数仍自洽的混合集。组身份
  由刷新 run 记录提供（§5.4 R2 的 `source_refresh_runs`），届时 `--verify-only`
  可改比为 run 记录里那一组。

写入走 `derived:security_names`
的同一写入器：一个事务里整表替换 `security_name_history`、登记
`derived_dataset_versions`、绑定 generation。

**ETF 观测名称行（D-76 Q1，`etf-name-search` PR-2）。** 同一个 scope 还从清单里的
`sse_etf_list`、`szse_etf_list` 快照重建 ETF 名称行（`source_kind='exchange_snapshot'`）。表里有两个互不相交
的子集：A 股行（`source_kind <> 'exchange_snapshot'`）和 ETF 行，写入时各自替换，共用同一条
`security_names`/`a_share` 登记。ETF 部分是**只依赖清单**的纯函数（`etf_list::replay_names`）：
不读时钟、不读库、不读 refresh run，与日更 item 共用同一个扫描和同一个装配函数
（`etf_list` 的 `assemble_names`），所以同一个来源库上两条路径的行逐行相同
（`live_and_replay_etf_name_rows_are_identical`）。

- `--scope all` 包含它：只依赖清单，fresh init 之后现行 ETF 名称立即可用。
- 报告多一个 `etf_rows`（`snapshots` 含 ETF 行）；`--verify-only` 的摘要和 `drift` 列表包含 ETF 行。
- 清单里没有任何 ETF 名单：ETF 部分不动，已写入的 ETF 行保留，`--verify-only` 只比 A 股子集。
- 只有一份名单、SZSE 名单行数低于下界、名单一个可用名称都没有（`etf_names_empty`）：整个 scope
  fail-closed，`security_names_rebuild_failed`，**A 股行也不写**，不把半个市场当作重建结果。
- 清单里没有 `szse_short_name` 记录：与以前一样是 `store_empty` / `nothing_selected`，不碰库。
- SSE 内容回到旧值（A→B→A）不追加清单行，链停在 B；replay 与日更都写这条链，不补段。

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
- **fresh-init**：库还没有主表/日历 → fail-closed，表不动；**不写空表冒充成功**。
  单独跑本 scope 时状态与 reason 码都是 `suspension_replay_not_ready`（退出 1）。
  在 `--scope all` 里见下一条。主表与日历由日更或对应写入路径建好，之后本 scope
  才会写表；也可以直接跑日更 `derived:suspensions`。
- **确定性**：`summary` 是已写行的 `suspensions-v1` 摘要；同一来源库 + 同一库状态
  重放两次逐位相同。库状态前进会合法地改变结果：三样输入哈希成 `inputs_digest`
  并写进登记证据，`--verify-only` 因此能报 `inputs_match=false` / `drift_cause=inputs_changed`
  （不是 `source_store_corrupt`）；输入未变而行不同才报 `rows_changed`。
- **在 `--scope all` 里（T13）**：`all` 现在是六个 scope，`suspensions` 排最后。
  部署钩子在 fresh-init、服务启动前运行 `all`，那时三样输入必然不存在，所以
  可写的 `all` 把这个 scope 记为 `status=not_ready`，`not_ready_reason` 写明缺的是主表还是
  日历，不写表，整次运行仍是 `ok`、退出 0。判断顺序是先看库里的输入，再解析冻结页，
  所以库没就绪时冻结页的问题不会拖垮部署。`--verify-only` / `verify` 没有比较任何
  东西，因此 `suspensions` 仍是 `not_ready`，但整次运行状态是 `suspension_replay_not_ready`
  （退出 1），单独跑 `--scope suspensions` 也一样。来源库为空时是 `store_empty`，
  不打开数据库。
- **`inputs_changed` 的原因与不放松的理由**：`inputs_digest` 覆盖主表版本、交易日历
  和**每个实体的全部不复权 bar 日**。首采 `sh_sz_a_listed_ever`（338 只退市股）写入
  新序列后，bar 日变了，停牌 gap 的结果也会变，所以摘要变是对的，不是摘要多算。
  摘要只给 drift 贴原因标签，判决始终来自行内容比较（`match` / `drift`），所以不会
  漏掉真实变化；摘要变了而行没变时仍是 `match`（只是 `inputs_match=false`）。
  因此没有改摘要，也不该改：让它不覆盖 bar 日会把「库里的 bar 变了」说成「输入没变」。
  `inputs_changed` 的消除靠日更重算（它会用最新输入重写数据集与摘要），不靠改摘要。

`--scope suspensions` 的 `--db` 写入仍需服务停止（重放取数据库租约），与其它写库
scope 相同。

### 4.3 `register-generation-member`：遗留代补登记

在来源库出现之前采集的 generation，成员字节可能已经在库里（`archive_import`，
sha 与重算一致），但清单里没有 `origin=generation:<manifest sha>` 的行，
退役门禁因此拒绝（[M3 设计](tasks/issue-446-m3-delisted-industry.md) §8.3、
XDM-146 报告 §3/§4.3 方案 A）。`import` 补不上：它的 origin 固定为
`<batch>:<container>!<suffix>`。用 XDM-150 的子命令补：

```bash
source_store register-generation-member --store <dir> \
  --generation <manifest sha256> --generation-dir <generations/generations/<sha>>
```

- `--generation` 必须等于 `<generation-dir>/snapshot-manifest.json` 字节的 sha256
  （generation id 的定义）；不符 → `generation_manifest_mismatch`，退出码 1。
- 成员清单与日更 item 同源：按 manifest 的角色绑定调
  `TdxMarketSource::industry_facts` 抽 `base.zip!tdxhy.cfg` 与
  `gbbq.zip!zhb.zip!incon.dat`，再用 `industry::register_generation_members`
  的同一实现登记。`base.zip`/`gbbq.zip` 按 manifest 的 `bytes`+`sha256` 重算比对
  （不符 → `generation_container_mismatch`）；不读 `hsjday.zip`，因为这里不登记
  日线字节。
- 写入走 `SourceStore::register`：flock、torn-tail 检查、
  `(sha256, source_id, origin)` 幂等键不变；blob 已在库时只追加清单行并复用已有
  blob（不重写），否则按 §3 的写序落 blob。**只追加**，不改写、不删除已有行，重复
  执行不新增行。
- 该代没有 `tdxhy.cfg` → `generation_has_no_industry_member`，退出码 1（不报 `ok`）。
- 输出单个 JSON：`origin`（`generation:<sha>`）与每个成员的
  `role`/`source_id`/`member`/`sha256`/`size_bytes`/`added`/`blob_reused`。
- 只写来源库、不写市场库；仍需把 `x-data-mcp-tunnel`、`x-data-mcp-acquire`、
  `x-data-mcp-data` 都停掉（data/acquire 持有来源库与市场库的锁）。停的顺序是
  **tunnel → acquire → data**，起服务反过来 **data → acquire → tunnel**：tunnel 单元带
  `Wants=`/`After=x-data-mcp-data.service` 和 `Restart=always`（`RestartSec=10`），只停
  data+acquire 时它会在 10 s 后重启并把 data 拉起来（2026-11-01T10:22Z 实测，正在跑的
  replay 报 `database service already active`）。停完确认三者 `inactive`，再动手写库。

补登记后跑 `source_store replay --scope generation_facts --db <market db>`，再用
`source_store exempt` 覆盖该代确未写入的行情派生事实（理由写 `source_bindings=0`）；
完整退役序列见 [M3 设计](tasks/issue-446-m3-delisted-industry.md) §8.3。

### 4.4 `generation_facts`：摘要的含义与提交后刷新

`generation_facts.fact_digest` 是**这一代在库里当前仍被引用的派生内容**的摘要：
`market_series`（`daily_series`，含 forward/raw 两个口径）、`capital_events`、
`adjustment_events` 里 `source_binding_digest` 指向该代的那些行。它不是「这一代
写入时那一刻」的快照：日更或前进窗口会把行改挂到新代的 binding（`series_commit.rs`
的 `UPDATE … source_binding_digest`），旧代仍在引用的内容因此变少，摘要随之变小。

- **每次成功提交后刷新所有代**：写入侧在日更提交后调用
  `refresh_derived_generation_facts()`，对「仍持有 binding 的代」与「`generation_facts`
  里已有派生行的代」重算摘要并 upsert（主键 `(generation, role, kind)`，幂等），
  不再只登记刚提交的那一代。成本约为 `3 × 代数` 次摘要查询（另加一次代数集合
  查询和一次已有行查询）。`replay --scope generation_facts --verify-only` 因此在
  任何一次提交之后都应与库内行一致，不需要人工修复。
- **不删除行**：一个代的 binding 全部被改挂走之后，它的事实行保留（退役门禁按
  「行存在」判断该事实类别是否登记），摘要改为**空集摘要**；只要该代曾登记过
  某个派生 kind，重放也为它算出同一空集摘要。
- **不发明行**：该代从未登记过的派生 kind 仍然缺席，门禁照旧按缺失处理，不会
  因为刷新而凭空多出一类事实。

`--verify-only` 的比较口径不变（`facts_summary`，不含 `registered_at`）。

## 5. 退役豁免

退役门禁（[M3 设计](tasks/issue-446-m3-delisted-industry.md) §8.3）默认拒绝：
来源库没有按 sha 读回的证据，或**激活过的代**在 `generation_facts` 缺必需 kind。
从未激活的代不要求派生事实（`#583`，见 [日更文档](daily-sync.md)「Generation 模型」），
这类代只需来源证据通过即可退役，不需要写豁免。确有外部原因时，
owner 用 `source_store exempt` 写一行豁免，再重试退役：

- `--missing` 用门禁报告里的键：`source_store_evidence:<role>:<source_id>` 或
  `derived_fact:<role>:<fact_kind>`。
- `--reason` 与 `--recorded-by` 必须非空；时间由命令写入。
- 来源库证据的豁免必须再加 `--owner-approve-evidence`，否则命令拒绝（退出码 2）。
- 豁免只覆盖列出的键，不覆盖 active/pin/in-flight 检查；退役成功后回复
  `detail=exemptions: ...` 指明用到了哪些豁免。
- 「从未激活」不是豁免：门禁直接跳过派生事实步骤，回复
  `detail=derived facts not required: generation was never activated`；
  该判定只由 acquire 的持久记录得出（激活前的 `state` + active pointer +
  `activate_generation` 请求结果），调用者无法自行声明。
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
`source_store_replay` 字段且不算失败；`suspensions` 在 fresh init 是 `not_ready`（见 §4.2），
同样记入报告，不算失败；非 0 退出（例如 blob sha 读回不符）会让
部署失败，而不是部署一个「探活通过、历史为空」的实例。回滚不动来源库。

## 8. 验证边界

CI 覆盖写序故障注入（fsync/rename 各失败点无悬空清单行）、按 sha 读回不符 →
`source_store_corrupt`、重复导入不增 blob/行、重放两次摘要相同、空库
`store_empty`、含 manifest 不符条目的夹具进 `skipped_unverified`。
`suspensions` scope 另有端到端/单元测试：冻结页 → facts 计数、`--db` 装配与日更
写入逐位一致、`--verify-only` `match`/`drift`（含 `inputs_changed`）、缺主表/日历 →
`suspension_replay_not_ready`、未知 `parser_version` fail-closed、空库 →
`store_empty`/`nothing_selected`。`security_names` scope 另有 6 条端到端测试
（`tests/source_store/security_names.rs`）：
清库后重放得到逐位相同的摘要与登记证据（`evidence_digest` 相同）、
`--verify-only` 为 `match`、篡改 blob → `source_store_corrupt`、未知
`parser_version` → fail-closed、空库/无 `szse_short_name` 记录 →
`store_empty`/`nothing_selected` 且不碰库、全程离线。`risk_warnings` 与
`security_names` 的选择（XDM-154）另有端到端测试
（`tests/source_store/risk_warnings.rs`、`tests/source_store/security_names.rs`）：
多页完整页集与单页快照重放出同一摘要、同指纹两份 sha 只算最新（旧页事件不重复
计入）、缺页或行数与 `recordcount` 不符 → `snapshot_incomplete` 且上一版仍可读、
`--verify-only` 与同一选择比对为 `match`、旧 v2 扁平公告页仍可重放。`generation_facts`
的跨代刷新（XDM-161）在 `src/sqlite_market/generation_facts/tests.rs` 用两代行级夹具
验证：改挂前重放 `drift`、提交后刷新转 `match`、空集摘要保留、退役门禁仍通过、
重复刷新幂等。测试全部使用
临时目录与合成归档，不联网、不依赖本机路径。

真实归档导入、`docs/STATUS.md` 的批次留痕和 fresh-init 后的
`store_empty → import → replay match` 验收，属于后续 PR 的私有栈验收范围。
