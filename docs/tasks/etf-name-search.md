# ETF 按名称查询：设计

Refs [#525](https://github.com/liqiangcc/x-data-mcp/issues/525)、[#509](https://github.com/liqiangcc/x-data-mcp/issues/509)。日期 2026-11-05，代码基线 main `08c1ef8`（线上 preview.45 / `generic-data-v10` / schema 27）。
只出设计，不改代码，不访问交易所和生产机。

**状态（2026-11-05）**：§7 的 Q1–Q9 已由 owner 全部按推荐批准，记为 D-76。PR-0 取证已完成，结果回填在 §8，召回测量在 §8.1。下文 §0–§5 是批准前写的设计，其中 Q2 的字段取舍已按 §8.1 落定：SSE `name` 取 `secNameFull`，`fundAbbr` 不进 `name`。§5.1 里「SSE `fundAbbr`」作 `name` 的写法以 D-76 为准。

**PR-3 读面（已实现，代码面未部署）**：`find_securities` 加可选 `kind`（`stock` 缺省、`etf`），按 §5.3 的 (b) 形态实现为 `market.security_names` 上的内部窄过滤 `Filter::FindEtf`，不放开 `universe_scope`。`research` 的 `tools/list` +103 B。契约见 [generic-data-v11](../contracts/generic-data-v11.md) §7，工具说明见 [mcp-tools](../mcp-tools.md)。与 §5.3 草案的差别：没有 `kind=all`，也没有 `include_delisted`，`kind=etf` 一次返回现行和退市，用 `status` 区分；返回列用 `name_observed_from` 表示最新名称的首次观测日；沿用 PR-2 的“不补段”，搜索只看最新名称。

**证据分级**：「代码核对」= 本任务读了仓库代码和合成夹具；「既有实测」= 引自
[M3-2 设计](issue-509-m3-2-etf.md) 2026-11-03 的真实字节取证，本任务没有复核；「推断」不是结论；
「未验证」见 §8。本机不能访问生产来源库，所以**冻结字节里到底有什么，这里一律没有亲眼看过**。

## 0. 结论

1. 缺口的原因：两份 ETF 名单的字节里有名称，但解析器一个名称字段都没读（`EtfListRow` 没有 name 字段），
   也没有任何表存 ETF 名称。D-70 Q11(a) 写的是「首期只入身份/名称/上市日」，PR-A 只落了身份和上市日，名称漏了。
2. 历史只能从第一份冻结快照开始积累，往前没有来源。快照序列的粒度是「首次观测日」，不是改名生效日。
3. 推荐方案 B：ETF 名称写进现有 `security_name_history`（`source_kind='exchange_snapshot'`），**不需要 schema 迁移**，
   可以随只换二进制的 preview.46 上线。代价是要 owner 确认放宽 D-61 里「当前名称不转成区间」的限制（仅 ETF，Q1）。
   如果 owner 不接受，退到方案 C（单独读模型），需要迁移 028 和一次 fresh init。
4. 退市 ETF 的名称，目前没有已核实的来源。候选是已冻结的公告索引字节（字段未核实）和 D-72 式的一次性 PDF 提取。
   查不到就写入残余缺口（§3），不用代码段或当前名称反推。

## 1. 现状

### 1.1 名称在哪，解析器读没读（代码核对 + 既有实测）

| 来源 | 字节里的名称字段 | 解析器读取 | 位置 |
|---|---|---|---|
| `sse_etf_list`（`FUND_LIST`） | `secNameFull`、`fundAbbr`、`INDEX_NAME`（既有实测：947 行，另有 `companyName`、`fundManager` 等） | 只读 `fundCode`、`listingDate`、`subClass`、`INDEX_CODE`；名称一个都没读 | `src/official_security/etf_list.rs:446-473` |
| `szse_etf_list`（`1945`） | 证券简称、拟合指数（既有实测：770 行） | 只按表头找「证券代码」一列，其余列不读 | 同文件 `:494`，行构造 `:526-532` |

- `EtfListRow`（`etf_list.rs:145`）没有名称字段，`security_master` 也没有名称列（`migrations/tasks/023_security_master.sql:80`）。
- 合成夹具印证了形状：`tests/fixtures/exchange-etf-list/sse_etf_list_synthetic.json` 含 `secNameFull`/`fundAbbr`/`INDEX_NAME`；
  `szse_etf_list_synthetic.xlsx` 表头为 `证券代码、证券简称、拟合指数、当前规模(份)、基金管理人`（本任务读出）。
- 清单里的 `parser_version` 是**信封**版本（`exchange-sse-page-v2` 等）；业务版本 `etf-list-v1` 只出现在 item 证据里。
  所以读新字段只需要新业务解析版本，不碰已冻结字节，也不改清单。
- `fundAbbr` 与交易所「证券简称」、扩位简称是不是同一个东西：**未验证**，决定搜索召回，见 Q2 和 §6 的 PR-0。

### 1.2 历史变更能不能得到（代码核对 + 既有实测）

- 发布方没有历史：SSE `FUND_LIST` 与 SZSE `1945` 都「无历史版本、无 `last_modified`」（M3-2 §3.2）。
  第一份冻结快照之前的名称不可得。
- 快照怎么积累：`refresh_exchange_sources` 的 `tail` 和 `full` 都整表重取两份名单（`PlanClass::Table`，`src/source_refresh/plan.rs:158-171`）。
  来源库按 `(sha256, source_id, origin)` 去重（`src/source_store/mod.rs:515`），字节没变就不追加清单行。
  因此清单里的序列是「每个不同字节一行，时间 = 首次获取」。
- 粒度（推断）：SZSE 表含「当前规模(份)」，字节很可能每天都变，序列接近日频；SSE 表只含静态字段，只在内容变化时出新行。
- 现成的扫描器：`etf_list::scan_history`（`:592`）已经读出全部历史快照做成员并集，只是丢掉了名称。
  名称链可以用同一遍扫描得到：每个代码一串 `(首次观测日, 名称)`。
- 三个限制：
  1. 区间起点是「首次观测到该名称的日子」，真正的改名日在上一次观测之后、这一天之前。这与 M3-2 对身份行的处理一致
     （`effective_from = 首个快照 fetched_at`，`basis=fetch_time`，M3-2 §3.3、§15.9）。
  2. 去重会吞掉「A→B→A」：第三份字节与第一份相同，不追加清单行（代码核对：去重键 `(sha256, source_id, origin)`，`origin` 是请求指纹）。
     SSE 静态表会受影响。日更读面用 refresh run 的成员记录选「当前名单」，能看到当前是 A；
     replay 在没有 run 记录时只能取清单里最新的一行，会得到 B。两条路径在这种情形下会得出不同的末段名称，
     和 `risk_warnings` 的漂移同类。这只在真实序列里出现过 A→B→A 时才发生（未验证，PR-0 第 5 项）；
     若发生，要么接受并在 `limitations` 写明，要么另案让冻结在重复观测时也追加一行。本设计不改冻结行为。
  3. 清单看不出「没刷新」和「没变化」的区别。区间默认两次观测之间名称不变，这是一个假设，写进 `limitations`。
- 生产来源库里目前有几份名单快照、最早一份是哪天：**未核实**。核实方法：在有来源库的机器上读 `manifest.jsonl`，
  筛 `source_id` 为 `sse_etf_list`/`szse_etf_list` 的行，数行数并取 `acquired_at` 最小值。
  M3-2 的取证快照是 2026-11-03，所以序列最早不早于这一天（推断）。

### 1.3 现有读面为什么答不了

- `find_securities` 映射到 `securities.catalog` + `current_sh_sz_a`（[#525 §4.1](issue-525-read-tools.md)），
  用 `name_query` 对 `catalog_members.name` 做子串匹配（`src/sqlite_supply.rs:858`）。目录只含现行 A 股。
- `market.security_names` 要先给出实体列表才能读（`src/sqlite_supply/m2.rs:306-315`），没有按名称找实体的入口。
- `security_name_history` 里 ETF 为 0 行，因为写入器和校验都只认深市 A 股：`validate_set` 对非 SZ 行直接 `Error::Market`
  （`src/security_master/names.rs:344`）。

## 2. 方案比较

三个方案都从同一份冻结字节出发；差别是名称放在哪。

| | A. 扩展 `securities.catalog` | B. 写入 `security_name_history` | C. 单独的 ETF 名称读模型 |
|---|---|---|---|
| 做法 | ETF 进 `catalog_members` | ETF 行用 `source_kind='exchange_snapshot'` 写入现有名称历史表 | 新表 + 新 dataset_kind + 新 item/scope |
| 正确性 | **最差**（第 1 名最好：C） | **次优**：快照派生值与来源给出的变更日混在一张表里，靠 `source_kind`/`certainty`/`known_at` 区分 | **最好**：表和语义都明说是「观测」，不借用 D-61 的区间含义 |
| 简单 | 最差：schema + head + 默认 universe + 目录构造都要动 | **最好**：无新表、无新 dataset | 次之：新表、新迁移、新 scope、新读面 |
| 性能（推断） | 略差：目录从约 5.2 千涨到约 7 千，每次 A 股 universe 都多读 | 约 1.8 千行 ETF 名称，内存子串匹配，毫秒级 | 同 B |
| 迁移 | 要。`catalog_members` 无 `instrument_class` 列，`catalog_head` 是单例 | **不要**（见下） | 要。`derived_dataset_versions.dataset_kind` 是封闭 CHECK，新 kind 必须像 023 那样重建该表 |
| 能否不迁移 | 否 | 能 | 否 |

综合排序（正确性 > 简单 > 性能）：严格按正确性排是 C > B > A。推荐 B，理由是 C 的正确性优势主要在语义标注，
B 用 `source_kind` 与文档就能达到可接受的程度，而 C 的代价是 schema 27→28 与一次 fresh init（见下）。
这是有意的取舍，不是疏忽；两者的数据都能由冻结字节纯函数重放，所以日后从 B 换到 C 只是 fresh init + replay，不是单向门。

### 2.1 A：扩展 `securities.catalog`（不推荐）

- 已被否决过：D-70 Q5 批准「ETF 不进 `securities.catalog`」（M3-2 §5.3）。理由仍然成立：
  默认 `universe` 取目录版本，ETF 一进目录，A 股股票池和 `get_market_snapshot a_current` 的成员集就变了，违反 AC-M3-2-2。
- `catalog_members` 的 `industry_status` 是 NOT NULL，ETF 没有行业归属（M3-2 §13 偏差 10），只能硬塞 `missing`。
- 目录是「当前」快照，覆盖不了退市 ETF，也没有历史。
- `find_securities`：不用改工具，但 ETF 会混进每一次 A 股搜索，无法排除。
- replay：目录来自 generation 文件，不来自来源库；ETF 名称要来自来源库，目录构造会反向依赖来源库。
- 迁移：要（`catalog_members` 加品种列、`catalog_head` 改为每类一个 head），然后 fresh init。

### 2.2 B：写入 `security_name_history`（推荐）

**为什么不用迁移**（代码核对）：
- 表的 `source_kind` CHECK 已经包含 `exchange_snapshot`（023:143），代码里 `NameSourceKind::ExchangeSnapshot` 也已存在但没人用；
  M2 设计 §4.4 当初就是为「交易所名单的当前简称」预留的。
- `exchange IN ('sh','sz','bj')`、`certainty IN ('interval','unbounded')`、主键 `(exchange, code, effective_from)` 都能装下 ETF 行。
- `derived_dataset_versions` 里 `security_names` 这个 kind 已存在。

**必须改的代码（不是迁移）**：
1. `names::validate_set` 的 SZ-only 守卫（`names.rs:344`）：改成按 `source_kind` 判断。`exchange_snapshot` 允许 sh/sz，
   `szse_name_history` 仍只许 sz。这是对 D-61 的明确放宽，要 owner 批准（Q1）。
2. 写入器是整表替换（`src/sqlite_market/security_names.rs:67` 的 `DELETE FROM security_name_history`）。
   ETF 来源失败时不能把 ETF 行删掉，也不能让 ETF 失败拖垮深市简称。需要「按 `source_kind` 子集替换，失败时保留上一版」，
   语义照搬 `derived:security_master` 处理 ETF 的方式（`merge_etf_identity`：A 股照写，ETF 失败则 item 报部分失败）。
3. 登记只能有一条：`read_kind_version` 对同一 kind 出现两个 `dataset_key` 会报错拒绝选择（`src/sqlite_market/derived.rs:205-240`），
   所以 ETF 行并入现有 `security_names`/`a_share` 这一条登记，`dataset_digest` 覆盖两类行。
   后果：首次部署后，`security_names` 的 `version_id`（内容 hex）会变；验收用「深市行逐行不变」，不用「版本号不变」。
   `a_share` 这个 key 名字不再准确，保留（改名要迁移），在证据里写明含 ETF。
4. 不新增 derived item。`DATASETS` 固定 5 项并有测试（`src/task_control/derived.rs:58`），不传 `scope` 的 A 股计划必须保持字节不变
   （PR-C2 的验收）。ETF 分支放进已有的 `derived:security_names` item。

**对 `find_securities`**：目录里没有 ETF，需要新分支，见 §5.3。
**对 `source_store replay`**：`--scope security_names` 要同时重建 ETF 行，见 §5.4。
**对导出**：`security_names.parquet` 没有品种列，ETF 行一并进去会改变该文件的字节（`src/research/read.rs:929` 读全表）。
本批排除 `exchange_snapshot` 行，保持文件不变（Q7）。

**已知短板（不迁移的代价）**：
- 一个时间点只有一个 `name`，放不下「简称 + 全称 + 跟踪指数名」。简称之外的字段只能留在证据里。
- `source_kind` 词汇里没有「公告」。退市 ETF 若取自公告，只能记 `exchange_snapshot`，`source_record_id` 写 `etf_delist_ann:<url 或 sha>` 区分。
  词汇要精确，等下一次有迁移时再加。

### 2.3 C：单独的 ETF 名称读模型（B 被否决时的退路）

- 新表（观测序列：`exchange, code, observed_from, name, full_name, source_sha256, known_at`）、新 `dataset_kind`、
  新 item 或新 replay scope、新读面。
- 迁移 028：建表，并像 023 一样重建 `derived_dataset_versions`（封闭 CHECK）。`COMPACT_SCHEMA_VERSION` 27→28，
  `scripts/check_current_facts.py` 的期望值、契约文档同步改。
- 部署：#525 §8 把 preview.46 规划为「只换二进制、没有迁移」；有迁移就要走 `deploy_phase_g.sh`（D-08：归档 + fresh init + replay + 首采）。
  上一轮 preview.42 重建的各步合计约一小时量级（STATUS 实测，非本方案测量），期间服务中断。
- `find_securities`/replay 的影响与 B 相同，只是读写的表不同。

### 2.4 不考虑的做法

读路径临时去读来源库字节并解析（零迁移、零写入）。读面依赖来源库的可用性，违反「原始证据 → 解析事实 → 读模型」分层（[design](../design.md)），
也没法给 `known_at`。不采用。

## 3. 退市 ETF 的名称与残余缺口

范围：主表里 `instrument_class=2` 且已退市的 128 只（沪 82、深 46，STATUS 部署后读数）。

| 候选来源 | 有没有名称 | 状态 |
|---|---|---|
| 两份现行名单快照 | 有，但 128 只在第一份快照之前就不在名单里，**一个都没有** | 排除（推断：它们不在 2026-11-03 名单里，这是 D-71 的前提） |
| 公告索引字节（`sse_fund_delist_ann`、`szse_etf_notice_ann`，已冻结，D-71 已准入） | SSE：标题含基金**全称**（`关于XX交易型开放式指数证券投资基金终止上市的公告`，来自解析器与测试夹具）；SZSE：行里可能有 `secName`（只在合成测试里出现，真实字节**未验证**） | 候选。零网络，读已冻结字节即可核实 |
| 终止上市公告 PDF（D-72，已冻结） | 公告正文通常写场内简称（推断，未看过这 128 份 PDF） | 候选。沿用 D-72：一次性离线提取，提取 CSV 加一列 `short_name`，带 `doc_sha256`、页码、原文片段；生产代码不解析 PDF |
| TDX `infoharbor_ex.code` | A 股目录靠它取名；2026-10 的取证是 5,571 行（`evidence/issue-194-catalog-acceptance/PLAN.md:70`），与 A 股 5,230 + 北交所 348 接近，**推断**不含 ETF；当前代未核实 | 即使有也是新来源，要按 D-25 单独准入，且不是交易所口径。不建议 |
| `ebs`、行情页 | 只有当前 | D-70 Q1 已定为报价锚，不是身份来源 |

结论与残余缺口：
1. 公告索引或 PDF 里查到名称，则退市 ETF 只有「公告时点的名称」一个值，没有改名史。区间沿用主表 `effective_from`/`effective_to`。
2. 公告索引字段若没有简称，且 PDF 提取没做：128 只退市 ETF **没有名称**，`find_securities` 只能按代码找到它们，不能按名称。
3. 本来就没有身份的 ETF 也没有名称：沪市公告索引最早 2006-08（更早的终止上市公告不可得）、深市「退市且不在 `hsjday`」的成员未验证（M3-2 §4.1）。
4. 已退市但曾在某份名单快照里出现的 ETF，名称取快照；之后每个新退市的 ETF 在退市前都会进入快照，缺口只影响 2026-11-03 之前退市的存量。
5. 不用代码段、基金公司名或当前名称去猜；对没有名称的退市 ETF 返回 `name: null` 和 `name_basis: unavailable`，不返回空串。

依赖主表：退市名称要先知道谁是退市 ETF（候选 ∩ 公告命中，需要 generation 里的 `hsjday` 成员），所以它不能在 fresh init 的 `--scope all` 里重放，
与 `suspensions` 的处理方式一样：显式、依赖库、`not_ready` 时 fail-closed。这是把退市名称拆成 PR-4 的原因。

## 4. 扩展场景走查

`docs/design.md`「后续数据扩展要求」本身列的是**四类扩展**（同类新增来源、同类新增周期或计算、新增数据类别、新增工具面或入口）；
「六个场景」的枚举在 [M3 设计 §13](issue-446-m3-delisted-industry.md)，它开头写明依据的正是 design.md 的这一节。
这里按 §13 的六个场景走，并在每行标出对应的 design.md 类别，两处都覆盖。

| # | 场景（design.md 类别） | 本需求是否触及 | 结论 |
|---|---|---|---|
| 1 | 新增证券类型（新增数据类别） | 否。ETF 已是 `instrument_class=2`。名称是已有类型的新字段 | 不新增类型，不动 `instrument_class` 与 scope 词汇 |
| 2 | 新增官方来源（同类数据新增来源） | 现行名单：否，两个来源已准入（D-70 Q1），只多读字段。退市名称：公告索引已准入（D-71），多读字段；若用 TDX 名称才是新来源 | 字段级准入是否已含「名称」要 owner 确认（Q1）；TDX 名称不推荐 |
| 3 | 新增字段或数据集（新增数据类别） | 是，但是**字段**。B 复用 `security_names` 数据集；C 才需要新数据集 | design.md 说新类别要独立类型化表和迁移。ETF 名称与 A 股简称是同一类事实（按实体 + 区间的简称），键是 `(exchange, code)`，不是新类别。B 成立；若 owner 认为「观测名称」与「变更名称」是两类事实，走 C |
| 4 | 新增频率 | 否 | — |
| 5 | 交易规则变更 | 否。名称不参与任何派生（M2 设计 §4.4），涨跌停和股票池不读名称 | 写进读模型：`universe` 不因名称缺失剔除 ETF |
| 6 | 新增使用方/入口（新增工具面或入口） | 是。`find_securities` 属于 `research` 入口 | 优先加参数、不加工具：#525 估算 `research` 的 `tools/list` 约 18,300 B，预算 18,600 B（D-75），只剩约 300 B，装不下一个新工具（约 450-760 B，#525 §7）。加参数要算入 v11 |

design.md「扩展契约」逐条核对：
- **来源快照可冻结、可重放、不属于 generation artifact**：满足，名称取自来源库已冻结字节，纯函数重放（PR-2 必须带 live/replay 等价测试，§5.4）。
- **回填行 `known_at` 不得早于入库时间**：满足。ETF 名称行 `known_at` = 该快照首次入库的上海日期（沿用 `resolve_known_at` 的方式，`risk_warnings.rs:481`），不是改名日。
- **0 行不是证据**：ETF 名称 0 行时读面回 `absent`/`name: null`，不回「没有名称」。名单为空沿用 `etf_list_empty` fail-closed。
- **时点证券主表按生效区间，不强套目录「当前」语义**：B 把名称区间对齐主表区间（末段在主表 `effective_to` 闭合）。
- **时点参数是契约保留名**：`as_of`/`known_at` 对 ETF 名称行按观测基础解释，`source_kind='exchange_snapshot'` 在每行里可见；`limitations` 写明「起点是首次观测日」。
- **SQLite 新类别用独立表与迁移**：见场景 3，B 不认为这是新类别。
- **成员集与目录快照分离**：ETF 继续不进目录（D-70 Q5），B/C 都不改它。

## 5. 推荐方案（B）

### 5.1 数据

- 解析：`etf-list-v2`（业务版本）。`EtfListRow` 增加 `name`（SSE `fundAbbr`，SZSE 证券简称）和 `full_name`（SSE `secNameFull`，SZSE 无）。
  字段选择以 PR-0 的取证为准（Q2）。名称做 `trim` 与内部空白压缩；是否复用 `names::normalize_name` 的 XD/XR/DR 前缀剥离
  要用真实字节测试后再定，名单是静态简称，剥前缀对 ETF 没有收益，反而可能误伤以这些字母开头的名称。
- 名称链：`scan_history` 为每个代码返回按观测日排序的 `(日, name)`，相邻重复合并。**不补段。** 当前名单的名称与链末项不同时（SSE 的 A→B→A，§1.2 限制 2），写入链本身（末段是 B），把这些代码的数量和前若干个样本写进 item 证据（`names_behind_current`），读面在 `limitations` 里说明。
  早先这里写的是「用当前名单观测日补一段」，不采用：能用的日期只有 refresh run 的 `verified_through`，它每次都变，补出来的行内容会随时钟变化；replay 没有 run 记录，live 与 replay 逐行相同的等价就不成立。A→B→A 在 SSE 的真实序列里还没有观测到（取证 §5，SSE 只有 1 份快照），这里说的是「没观测到」，不是「不会发生」。
- 行：
  - `effective_from` = 该名称首次观测日；`effective_to` = 下一名称的 `effective_from`，`certainty='interval'`；末段无后继则 `unbounded`。
    已移出名单的代码，末段在主表 `effective_to` 闭合。
  - `known_at` = 首次入库的上海日期；`source_kind='exchange_snapshot'`；`source_record_id` = `sse_etf_list:<sha>` 或 `szse_etf_list:<sha>`。
  - 首次观测日之前没有行。`as_of` 早于它时回 0 行，不补当前名称。

### 5.2 写入与失败语义

- 在 `derived:security_names` item 里加 ETF 分支，用 `etf_list::run_source`。深市简称成功而 ETF 失败：深市照写，保留上一版 ETF 行，
  item 报部分失败（`etf_source.status=failed`，与 `derived:security_master` 一致），run 为 `partially_failed`。
- 来源库未配置：沿用 `Optional` → `pending_source`，`Required` → `source_store_unconfigured`。
- ETF 名单未冻结过：`pending_source`，不当失败（`etf_list::run_source` 已有此分支）。
- 同一轮 run 里 master item 已经读过名单；names item 再读一遍并重新扫历史。`scan_history` 对每个冻结快照读取并解析一次，
  SZSE 日频时每年约 250 份、每份 36 KB 量级（既有实测的单份大小，其余为推断）。先按「各扫一次」实现；若实测耗时可见，再共用 `source_cache`。

### 5.3 读面

`find_securities` 要在不改变默认行为的前提下找到 ETF：

- 推荐：给 `find_securities` 加可选参数 `kind: "stock"|"etf"|"all"`，缺省 `stock`（行为与今天逐字节相同）。
  `etf` 查现行 ETF（主表 `sh_sz_etf_current` ∩ 名称行 `effective_to IS NULL`），`all` 另加 `scope`/`include_delisted` 之类的开关由 PR-3 定，不在此设计。
- 底层需要一个按名称找实体的读函数（`security_names` 目前只能按实体读）。建议放在 `sqlite_supply/m2.rs` 里，不动 `securities.catalog`；
  wire 形状在 PR-3 里二选一：(a) `security_names` + `selection.kind=universe`（放开 `universe_scope` 对 ETF scope 的 market.daily-only 限制，`src/data_supply/query.rs:287`）+ 新 `name_query` 过滤；
  (b) 单独的窄过滤。无论哪种，工具 schema 的增长要在 PR-3 里实测并记入契约。
- 查询归一化：去掉全部空白、ASCII 字母不分大小写、全角数字字母转半角。`沪深 300 ETF` 与 `沪深300etf` 命中同一名称。
  不做拼音、不做模糊匹配。
- 返回字段：`entity`、`name`、`name_basis`（`observed_from_first_snapshot` | `announcement` | `unavailable`）、`status`（`current`|`delisted`）、
  `listing_date`、`delisting_date`、`name_observed_from`。
- `get_security_state`/`get_security_history` 的 `security_names` 分支（#525 §4.6 要加的 `limitations`）要按品种说话：
  沪市 A 股「名称历史只覆盖深市」；ETF 「名称自首次观测日起，不含更早改名」。

### 5.4 replay

- `source_store replay --scope security_names` 同时重建深市行和 ETF 行。ETF 部分是**只依赖清单**的纯函数：
  新增 `etf_list::replay(store, ...)`，与 `security_names::replay(store, pages)` 同形，不读时钟、不读库、不套 14 天陈旧闸门。
- `--verify-only` 的摘要要包含 ETF 行，否则漂移看不出来。
- **live 与 replay 必须共用一个选择与排序函数，并带等价测试**：同一个来源库、同一个 `parser_version`，两条路径逐行相同。
  理由是 `risk_warnings` 刚在 vm2 上出现过这种漂移（replay 与日更选了不同快照，105 行 `source_document_sha256` 不同，2026-11-05 核实，另案处理）。
- 只依赖清单的部分能进 `--scope all`（fresh init 在服务启动前跑），所以 fresh init 之后现行 ETF 名称立即可用。
  退市名称（PR-4）依赖主表，显式 scope，不进 `all`。
- 更新 `NOTE` 常量、`src/bin/source_store.rs` 的用法注释和 [来源库](../source-store.md) 里的 scope 描述。

### 5.5 验收（建议 AC）

| AC | 判据 |
|---|---|
| ETF-NAME-1 | 现行名单内的 ETF（2026-11-03 取证日为 1,717 只）每只至少一行名称，或在证据里列出缺名称的代码（不静默丢） |
| ETF-NAME-2 | 深市简称行逐行不变（`exchange='sz' AND source_kind='szse_name_history'` 的行集前后相同）；A 股计划的 `plan_id`/item 字节不变 |
| ETF-NAME-3 | 同一来源库下，日更 item 与 `replay` 的 ETF 行逐行相同；`--verify-only` = match |
| ETF-NAME-4 | ETF 来源失败时，深市行写入成功、ETF 旧行保留、item 报部分失败 |
| ETF-NAME-5 | `find_securities` 缺省参数的响应与改动前逐字节相同；`kind=etf` 能按名称找到合成夹具里的 ETF，找不到时回空列表且带 `limitations` |
| ETF-NAME-6 | `security_names.parquet` 本批字节不变；`tools/list` 增量实测并记入契约 |

## 6. PR 拆分

| PR | 内容 | 依赖 |
|---|---|---|
| PR-0 取证（不改代码）**（已完成，2026-11-05，见 §8）** | 在有来源库的机器上离线读已冻结字节，不访问网络：(1) 两份名单快照的数量与最早日期；(2) `fundAbbr`/`secNameFull`/证券简称各是什么样；(3) 用 `沪深300ETF`、`300ETF` 在 10-20 只已知 ETF 上试召回；(4) 公告索引字节有无简称字段；(5) A→B→A 是否发生过。结果补回本文件 §8 | owner 批准 Q1 |
| PR-1 解析 | `etf-list-v2` 读名称；合成夹具；`scan_history` 返回名称链。无行为变化 | PR-0 |
| PR-2 写入与重放 | 域层 `assemble_etf_names`、放宽 `validate_set`、子集替换、names item 的 ETF 分支、replay 与等价测试 | PR-1 |
| PR-3 读面 | 按名称找实体的读函数、`find_securities` 的 `kind`、`limitations`、契约（并入 #525 的 v11 草案）、`tools/list` 实测 | PR-2；与 #525 PR-1 排期对齐 |
| PR-4 退市名称（可选） | 公告索引字段或 D-72 提取 CSV 加 `short_name`；显式 replay scope | PR-0 结论、Q4 |
| PR-5 导出列（可选） | `security_names.parquet` 加 `instrument_class` 并收录 ETF 行 | 看 Q7 |

部署：PR-1…3 无 schema 变化，随只换二进制的 preview.46 上线；二进制切换后，下一次日更的 `derived:security_names` item 写入 ETF 行，
不需要停服务做 replay。`research` 的 `tools/list` 预算归 #525 的 PR-1 处理，本设计不单独改 CI。

## 7. 需要 owner 决定的问题（已批准）

owner 2026-11-05 对 Q1–Q9「全部按推荐」，记为 D-76。下表「推荐」列即批准值。Q2 的推荐写明「以 PR-0 的召回试验为准」，落定结果：SSE `name` 取 `secNameFull`、SZSE 取证券简称，`fundAbbr` 不进 `name`（§8.1）。Q4 落定：深市用公告索引 `secName`，沪市 82 只没有字段，用 D-72 的方式一次性离线提取终止上市 PDF 的场内简称。

| Q | 问题 | 选项 | 推荐 |
|---|---|---|---|
| Q1 | D-70 Q11(a) 的「名称」是否已经覆盖读取 `fundAbbr`/证券简称并用于搜索？同时，是否放宽 D-61 里「当前名称不转成区间」的限制，仅限 ETF、以首次观测日起算、标 `exchange_snapshot` | (a) 是，放宽；(b) 否，走方案 C | **(a)**。与 M3-2 身份行同一基准（`basis=fetch_time`）；不放宽就要迁移与 fresh init。批准后记为新的 D 条目 |
| Q2 | 存哪个名称、搜哪些字段 | (a) 只存简称进 `name`，全称与跟踪指数名留在证据里（D-70 Q11 的「留证」）；(b) 另存全称、指数名供搜索（需要方案 C 或后续迁移） | **(a)，以 PR-0 的召回试验为准**。若 `沪深300ETF` 召回不到，再决定是否做 (b) 或把指数名搜索作为 Q11(b) 另开 |
| Q3 | 区间起点用「首次观测日」，并在读面标明 | 是/否 | **是**。没有更早的来源，也不编造改名日 |
| Q4 | 退市 ETF 名称怎么取 | (a) 先看公告索引字节有无简称，有就用；(b) (a) 没有时，D-72 式一次性离线提取 PDF，CSV 加 `short_name` 列；(c) 不做，写入残余缺口 | **(a) 先行，(a) 不行再 (b)**。（c）是可接受的兜底，但搜不到 2026-11-03 前退市的 128 只 |
| Q5 | 公告来源的名称在 `source_kind` 里记什么 | (a) `exchange_snapshot` + `source_record_id` 前缀区分；(b) 等有迁移时加 `exchange_announcement` | **(a)**，不为一个枚举值做迁移 |
| Q6 | 读面形态 | (a) `find_securities` 加 `kind` 参数；(b) 新工具 | **(a)**，预算原因见 §4 场景 6；是否并入 #525 的 v11 由 owner 排期 |
| Q7 | `security_names.parquet` 要不要收录 ETF 行 | (a) 本批不收录，保持字节不变；(b) 加 `instrument_class` 列并收录 | **(a)**，导出是否要名称不在本需求范围 |
| Q8 | 查询归一化 | 空白/大小写/全半角；不做拼音与模糊 | **如左** |
| Q9 | #525 §9 的 10 题基线评测里没有「ETF 名称」题。是否加第 11 题「沪深 300 ETF 的代码是什么」 | 是/否 | **是**。基线在 2026-11-09 之后开始，现在加不改变已定的口径，之后再加就没有基线可比 |

## 8. PR-0 取证结果与未验证项

取证日期 2026-11-05，只读、离线，读来源库清单与冻结字节、`preview.sqlite`（`mode=ro`）、当前代 `base.zip`；没有访问网络、没有写库。原始字节不进仓库，下表只写统计。取证报告在仓库外。

| # | 原未验证项 | 状态 | 结果 |
|---|---|---|---|
| 1 | `fundAbbr`、`secNameFull`、证券简称的内容和差异 | 已验证 | `fundAbbr` 是 2–8 字的场内简称（`300ETF`、`科创50`）；SSE `secNameFull`（6–15 字，`沪深300ETF华泰柏瑞`）与 SZSE 证券简称同一形态（题材 + `ETF` + 基金公司简称），可放进同一个搜索字段；`fundAbbr` 与 SZSE 简称不是同一种。`fundAbbr` 与终止上市 PDF 的「场内简称」形态一致，没有同一只基金两边对照，所以写「一致」不写「证实」 |
| 2 | 名单快照数量、最早日期、是否逐日变化 | 部分 | `sse_etf_list` 1 份、`szse_etf_list` 2 份，都在 2026-11-05。SZSE 的 xlsx 内嵌生成时间，同内容也换 sha，所以每次刷新都追加一行；SSE 是确定性 JSON，内容不变就去重。SSE 是否逐日变、SZSE 规模是否逐日变：仍未验证（只有一天） |
| 3 | `沪深300ETF` 等查询的召回 | 已验证 | 见 §8.1。只存 `fundAbbr` 时 `沪深300ETF` 只命中 6 只、全在深市，510300 找不到 |
| 4 | 公告索引有没有简称字段 | 已验证 | SSE `sse_fund_delist_ann` 6 个字段，没有简称，标题含基金全称。SZSE `szse_etf_notice_ann` 有 `secName`，82,852 条全部非空。128 只退市 ETF：深 46/46 有 `secName`；沪 0/82 有字段，82/82 有公告标题，其中 55 条能用正则抽到基金全称（启发式） |
| 5 | 128 份退市 PDF 是否写了场内简称 | 部分 | 抽样 5 份（沪 3、深 2）5/5 有「场内简称」，不能外推到 128 份，没有全量提取 |
| 6 | TDX `infoharbor_ex.code` 当前代是否含 ETF | 已验证 | 当前代 5,581 行，与现行 ETF 代码交集 0，不含 ETF |
| 7 | A→B→A 是否发生 | 部分 | SSE 只有 1 份快照，无法判断；两份 SZSE 快照之间无改名。SZSE 公告 `secName` 序列（次要证据）显示 894 个代码里 503 个改过名，13 个出现 A→B→A；多为交易所简称口径调整（2021-08、2026 两批集中），不全是基金改名 |
| 8 | `etf_list` 全历史扫描耗时 | 部分 | Python 解析 3 份名单 0.025–0.030 s，1,752 页公告约 0.3 s；Rust 未测 |
| 9 | 搜索性能 | 未做 | 约 1.8–1.9 千行内存匹配，仍是推断 |
| 10 | 方案 C 的 fresh init 耗时 | 未做 | 方案 B 已批准，不需要 |

其他实测：当前主表 `instrument_class=2` 且未退市 1,718 只，等于 SSE 947 + SZSE 771，两侧集合一致（§5.5 里写的 1,717 是 11-03 的数）。SZSE 的拟合指数列是「代码 名称」混合，538 行尾部带空格，做搜索字段要先拆列和 trim；SSE `INDEX_NAME` 空 32 行。

### 8.1 召回测量（Claude，2026-11-05，vm2）

只读读取冻结的 `sse_etf_list`，947 行。归一化同 §5.3：NFKC、去全部空白、casefold，子串匹配。

| 查询 | 只搜 `secNameFull` | 加上 `fundAbbr` |
|---|---|---|
| 沪深300ETF | 19 | 19 |
| 300ETF | 21 | 22 |
| 科创50 | 20 | 21 |
| 科创板50 | 0 | 1 |
| 黄金ETF | 4 | 4 |
| 上证50 | 12 | 12 |
| 红利 | 56 | 57 |
| 纳指 | 5 | 5 |
| 券商 | 2 | 2 |

- 加上 `fundAbbr` 最多多 1 只，`name` 因此取 `secNameFull`。
- **已知限制**：644 行的 `fundAbbr` 不是 `secNameFull` 的子串（如 `华夏300`、`广发500`），这类短写法搜不到。
- 样本是一个日期、9 个查询，没有评估排序。取证报告里的另一组测量（5 个查询，含深市）显示：加入指数名对这 5 个查询没有增量。

### 8.2 仍未验证

1. SSE 名单内容是否逐日变化，SZSE 规模列是否逐日变化（只有 2026-11-05 一天的快照）。
2. SSE 序列里 A→B→A 是否发生过。
3. 128 份退市 PDF 全量的场内简称（沪市 82 只靠它）。
4. Rust 实现下的扫描与搜索耗时。

## 9. PR-4 退市 ETF 名称：Operator 运行步骤

PR-4 为 128 只退市 ETF 提供名称（沪 82、深 46）。深市从公告索引读 `secName`，沪市需要 operator 从 PDF 提取场内简称。

### 9.1 深市（46 只）

深市退市 ETF 的名称直接从 `szse_etf_notice_ann` 公告索引的 `secName` 字段读取。无需 operator 额外操作，代码自动处理：

- 取证据显示 46/46 有 `secName`
- 取终止上市公告对应的那个值（按 D-72 的 main-announcement 规则：去重、优先正式公告、取最新日）
- `source_record_id` 格式：`szse_etf_notice_ann:<URL>`

### 9.2 沪市（82 只）— Operator 提取步骤

沪市公告索引没有简称字段。operator 需要离线提取 PDF 里的场内简称，结果通过 CSV 导入来源库。

**前提条件**：
- 82 份终止上市 PDF 已按 D-72 冻结进来源库（`source_id=etf_delist_pdf`）
- 已有 D-72 的 `etf_delist_extract` CSV（含 `delisting_date` 等字段）

**提取脚本要求**（由 operator 实现）：

```python
#!/usr/bin/env python3
# 提取 SSE 退市 ETF 的场内简称，追加到现有 extraction CSV

import csv
import subprocess
import sys
from pathlib import Path

# 读取现有 extraction CSV
input_csv = Path(sys.argv[1])  # etf_delist_extract_v1.csv
output_csv = Path(sys.argv[2])  # etf_delist_extract_v1_with_names.csv
pdfs_dir = Path(sys.argv[3])  # 包含 PDF 的目录

with input_csv.open('r', encoding='utf-8') as infile:
    reader = csv.DictReader(infile)
    fieldnames = reader.fieldnames
    if 'short_name' not in fieldnames:
        fieldnames.append('short_name')
    
    rows = list(reader)

for row in rows:
    # 只处理 SSE 行（沪市）
    if row['source_id'] != 'sse_fund_delist_ann':
        continue
    
    pdf_sha = row['doc_sha256']
    pdf_path = pdfs_dir / f"{pdf_sha}.pdf"
    
    if not pdf_path.exists():
        print(f"Warning: {row['entity']} PDF not found: {pdf_sha}", file=sys.stderr)
        row['short_name'] = ''  # 提取失败留空
        continue
    
    # 用 pdftotext -layout 提取第 1 页
    result = subprocess.run(
        ['pdftotext', '-layout', '-f', '1', '-l', '1', str(pdf_path), '-'],
        capture_output=True, text=True, encoding='utf-8'
    )
    
    if result.returncode != 0:
        print(f"Warning: {row['entity']} pdftotext failed", file=sys.stderr)
        row['short_name'] = ''
        continue
    
    # 正则提取：场内简称：XXX（可能含空格）
    import re
    match = re.search(r'场内简称[：:]]\s*([^\n]{1,20})', result.stdout)
    if match:
        name = match.group(1).strip()
        # 去掉 ETF 和数字间的空格："民企 ETF" -> "民企ETF"
        name = re.sub(r'\s+', '', name)
        row['short_name'] = name
        print(f"{row['entity']}: {name}")
    else:
        print(f"Warning: {row['entity']} no short_name found in PDF", file=sys.stderr)
        row['short_name'] = ''

with output_csv.open('w', encoding='utf-8', newline='') as outfile:
    writer = csv.DictWriter(outfile, fieldnames=fieldnames)
    writer.writeheader()
    writer.writerows(rows)

print(f"\nDone. Output: {output_csv}", file=sys.stderr)
```

**运行步骤**：

1. 在有来源库与 PDF 的机器（vm2）上运行提取脚本：
   ```bash
   python3 extract_etf_short_names.py \
       /path/to/current/etf_delist_extract_v1.csv \
       /path/to/etf_delist_extract_v1_with_names.csv \
       /path/to/pdf_blobs/
   ```

2. 检查输出：
   - 82 个沪市实体应该都有 `short_name`
   - 提不到的（正则没匹配上）留空，代码会写 `name: null` + `name_basis: unavailable`
   - **不用公告标题里的基金全称代替**，也不猜

3. 导入新 CSV 到来源库（替换旧的 extraction）：
   ```bash
   source_store import --store /path/to/source-store \
       --index etf_delist_extract_v1_with_names.csv \
       --blobs /path/to/pdf_blobs/ \
       --format etf-delisting
   ```

4. 验证导入：检查清单里 `source_id=etf_delist_extract`、`parser_version=etf-delist-extract-v1` 的行包含新 sha

**CSV 格式**：

新增 `short_name` 列（可选，空值表示未提取到）：

```csv
entity,delisting_date,announcement_date,source_id,doc_url,doc_sha256,page_no,snippet,extractor,extracted_at,confidence,last_operating_day,registration_date,short_name
sh.510070,2020-11-20,2020-11-13,sse_fund_delist_ann,http://...,862c9ae6...,1,"...",pdftotext-v1,2026-11-05T12:00:00Z,high,2020-11-19,,民企ETF
sh.512780,2021-08-27,2021-08-20,sse_fund_delist_ann,http://...,df063400...,1,"...",pdftotext-v1,2026-11-05T12:00:00Z,high,,,京津冀基
```

### 9.3 代码处理逻辑

- **SZSE**: `secName` 从 announcement 读取，无需 CSV 字段
- **SSE**: `short_name` 从 extraction CSV 读取
- 都没有时：`name: null`、`name_basis: unavailable`（不是空串）
- `source_record_id`: `{source_label}:{URL}`（如 `sse_fund_delist_ann:http://...`）
- 区间：`effective_from` = `announcement_date`，`effective_to` = 主表的 `effective_to`（退市日）

### 9.4 测试

合成 CSV 应包含：
- 一个有 `short_name` 的沪市行
- 一个 `short_name` 为空的沪市行
- 一个深市行（`short_name` 列可选）

生产 CSV 位置：按 D-72，提取结果只进来源库，不提交公开仓库。测试用合成记录。
