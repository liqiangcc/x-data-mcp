# x-data-mcp

基于 Rust 的数据治理与 MCP 项目，主要负责数据接入、质量治理、版本与来源追溯，以及通过 MCP 提供数据能力。

## 项目方向

- 数据是主要关注点：明确证券身份、数据范围、完整性、新鲜度、来源和版本。
- MCP 是外部入口，复用核心用例与能力契约，不成为另一套数据处理实现。
- 使用 Rust；性能目标需在具体用例的验收标准中明确，并通过代表性数据与基准测试验证。
- 当前不建设交易练习界面、订单撮合或自动交易系统。

首期证券范围为当前沪深 A 股、退市证券补齐与交易所发布的指数基准（ETF 放在 M3；北交所、分钟线、财务数据不在首期），行业仍为 TDX 研究行业二级。
**历史（2026-10-26 前的表述，已被上句取代）：** 首期证券范围为当前沪深 A 股及 TDX 研究行业二级。
生产来源目标已收敛为官方可下载文件及本地解析/计算；来源证据、存储交付及性能指标仍需按 Issue 验收，方向确认不表示能力已交付。

## 开发规范

本项目按照 [AI Engineering Conventions](https://github.com/liqiangcc/ai-engineering-conventions) 开发。采用版本、必读专题、适用范围和实践反馈方式统一记录在[规范采用说明](docs/conventions.md)，AI 开发入口为 [AGENTS.md](AGENTS.md)。

研发以 [GitHub Issues](https://github.com/liqiangcc/x-data-mcp/issues) 为任务入口：先明确需求、主要 Use Case、Business Rule 和 Acceptance Criteria，再进行设计、实现与验证。每个任务保留实际提交及验证证据。

## 设计入口

[整体设计](docs/design.md)串联业务流程、模块职责、版本/任务和运行验收；
[MCP tools 契约](docs/mcp-tools.md)记录历史契约与当前 runtime discovery 入口，
[通用工具设计](docs/generic-data-tools.md)定义单动作能力边界，
[每日同步说明](docs/daily-sync.md)记录双 MCP server、generation 状态机和 Devin Skill 编排边界，
[数据目录](docs/data-catalog.md)说明现在能拿到哪些数据、语义与缺口（供选股/因子/回测使用）。
SQLite 专项入口由整体设计统一引用。设计目标不表示实现、来源或部署已经验收。

## 与旧项目的关系

[liqiangcc/x](https://github.com/liqiangcc/x) 是需求线索、历史实现、数据契约与回归场景的参考。旧行为不能替代已确认业务要求；不整体复制旧仓库的数据、运行记录或实现。

是否迁移某项能力或数据，应由对应 Issue 明确来源、范围、兼容性、数据治理责任和验收方式。

## 当前状态

> 部署与运行事实以 [docs/STATUS.md](docs/STATUS.md) 为准，性能与容量目标以 [docs/SLO.md](docs/SLO.md) 为准。
**OWNER-ONLY OPERATIONAL PREVIEW — external distribution not authorized.**

本机以 systemd 运行已部署的 MCP 拓扑：`x-data-mcp-data` **一个进程服务三个 socket**：
`mcp.sock` 的 `ops` 入口（编排入口，15 data tools，含只读 `get_yearly_base_years` 与
`plan_market_update`/`run_market_update`，x 与日更 Skill 的连接不变）、
`research.sock` 的 `research` 入口（只读研究面，工具数 5）、`maint.sock` 的 `maint`
入口（本机运维，工具数 9，不经隧道）；capability revision `generic-data-v8`，
文本模式 `X_DATA_MCP_TEXT_MODE=full`，market 单版本序列：version_id=内容 hex，
latest-only。另有
`x-data-mcp-acquire`（10 acquisition tools）。隧道仍指 `mcp.sock`（改指 `research.sock` 未做）。
部署版本、socket 路径、generation 与数据量见
[docs/STATUS.md](docs/STATUS.md)。
已部署实例为 fresh compact schema 24；代码（main）同为 schema 24（M1 的 schema 22 +
M2 PR-1 的 023 + M2 PR-2 的 024），三个入口合计注册 24 个工具（
research 5 / ops 15 / maint 9），显式 `task_sqlite init` 的新库按代码的 schema 建立；
工具数量不是稳定契约，客户端仍须以运行实例 `tools/list` 与
`get_data_capabilities` 为准。

M2 已上线：6 个解析事实数据集（`market.security_master`、
`market.capital_events`、`market.suspensions`、`market.risk_warnings`、
`market.price_limit_rules`、`market.security_names`）在 `query_data` v2 上可读，
日更 run 的内部 item 维护其中四个全市场数据集；交易所名单、历史 ST（#308）、
停复牌公告与涨跌停规则原文未接入时按 `pending_source` 记录，读取结果按
`absent`/0 行如实返回（详见 [STATUS](docs/STATUS.md)「数据」段的 M2 数据集行）。
同一部署带上 `research-snapshot.v2` 研究快照导出。

已实现并在真实 generation 上执行过：immutable generation 生命周期
（acquire → byte verify → consistency verify → 条件 activate）、
generation-bound daily/yearly update，catalog/daily/yearly/query/verification
数据路径均在本机生产实例运行（编排边界见[每日同步说明](docs/daily-sync.md)）。

历史实测（preview.26–28 生产路径证据，不代表当前部署）：`securities.catalog` 目录
5,227 实体；daily 首次全市场采集约 340s；yearly 全量约 900s；库体积约 831MB；
同代 no-change `update_data` 全量 5,222 items 约 616s（约 10min）；跨代
update envelope 约 37min。

历史性能/容量 gate 沿革（当前目标见 [SLO](docs/SLO.md)）：PASS —— 首采 DB ≤2.5GiB、部署体积 ≤4GiB、首次全市场
采集 ≤20min。原统一的 5min/16MiB 日更门禁已由 2026-09-28 owner decision
supersede，[#259](https://github.com/liqiangcc/x-data-mcp/issues/259) 在
re-baseline 后关闭；现行同代 no-change full-market `update_data` 合同为
hard ≤15min、checkpoint 后 net growth baseline ≈3.6KiB/attempted item、
hard ≤4KiB/item（5,222 items 对应 hard ≈20.4MiB）、且必须 `data_versions +0`；
preview.27/schema20 实测 5,222 items / 616s / +18.5MB / 0 new versions，PASS。
acquisition、initial collection、yearly、cross-generation envelope 与真实
append/rewrite 分开计量：cross-generation ≈37min 仅为 preview.29 measured
baseline、无批准 hard SLO。#320 的 proven prefix append typed metadata 已有本地
retained-generation release harness 实测（c360967a→2e918330、5,222 items、
96.4s、5,165 typed metadata appends、45 envelope attempts：21 published、24
fail-closed，integrity check PASS；测量环境 ecs-node x86_64、2 vCPU、
1.9GiB RAM+4GiB swap、rustc 1.97.1、release profile，代码 6f9f0a4，单进程
cargo test -j1 顺序 ≤100-item 任务，冷/热缓存状态未记录）；该数值是离线
harness 结果，不等于 preview.29 或生产 SLO。真实 append/rewrite、deployed runtime timing 与
unsupported-metadata fallback 仍按
各自证据验收，未部署实例不得宣称已按新路径运行。
[#294](https://github.com/liqiangcc/x-data-mcp/issues/294) 为独立 admission 协议
问题。

`production_ready=false` 表达的是 declared scope（owner 个人研究、本地使用、
不对外暴露，#71 comment-5749934485）：服务正在运行并经过有界验收，但
external distribution is not authorized；它不是“服务未运行”的同义词。

schema 命名澄清：`SCHEMA_VERSION=18` 仅是 legacy upgrade source schema
（既有库显式 upgrade 的来源版本），已部署实例为 compact schema 21，代码的
`COMPACT_SCHEMA_VERSION` 为 24（未部署）。Issue/Epic 中的“v15”是交付规划标签，
不是 schema 编号。

**历史（2026-09 来源收敛口径，已被 [D-20](docs/decisions.md) 修订）：** 唯一目标链路：官方 base/行业文件、hsjday .day、gbbq 快照 → 本地 QFQ daily →
从同一已发布 QFQ daily version 本地聚合 yearly。不存在多源生产 fallback。

- BaoStock、源端直接 QFQ 仅 evidence/交叉验证。
- TdxQuant/TdxAiData/tqserver、东财匿名 datacenter/gzfx 不接入生产。
- Python bridge/eltdx 在线装配已移除，源码与旧测试保留在 `archive/online-tdx/`。
- 源端直接年线不再是生产契约；MCP `year` 表示本地派生输出。
- 既有 `evidence/`、历史文档与 Final Gate 裁定原样保留；历史结论不外推为当前生产状态。
- 每个新 generation 必须重新建立字节与跨文件一致性证据，不能继承旧代 PASS。

**2026-10-26 修订（[D-20](docs/decisions.md)）：** 允许引入一手官方来源——交易所与证监会的官方披露、
TDX 官方文件；每个新来源按 [#308](https://github.com/liqiangcc/x-data-mcp/issues/308) 的「离线输入准入」
模式过证据门禁，不接入任何第三方数据商或聚合源，具体来源逐个由 owner 裁决。
BaoStock 等第三方仍只能作交叉验证证据。

生产来源决策见 [Issue #55](https://github.com/liqiangcc/x-data-mcp/issues/55) 与 [D-20](docs/decisions.md)，
完整边界见[官方文件架构](docs/production-source.md)。

## 本地入口

`cargo run --locked --bin mcp_server` 是旧 stdio 原型，使用 `X_DATA_MCP_STORE`（默认 `.x-data-mcp`）；
其历史八工具契约不代表当前长期服务。旧 `X_DATA_MCP_TDX_PYTHON` 不再读取，不需要 Python/eltdx/Key。

当前 Linux 长期服务入口为 `data_service <initialized-db> <private-socket>`，客户端通过 `mcp_connect`
访问；它注册通用查询/任务/核实/计算工具并运行后台 worker。准确工具集合必须从该实例 `tools/list` 与
`get_data_capabilities` 发现，不能依据 README 静态列表。网络采集位于独立 `x-data-acquire` stdio server，
不会加入 `data_service`。部署、generation 和每日编排边界见[每日同步说明](docs/daily-sync.md)。

年线没有已发布 daily 时继续 fail closed；任何入口存在都不等于来源、generation、覆盖或整体生产认证已经通过。

## SQLite 任务开发入口（#78）

[PR #82](https://github.com/liqiangcc/x-data-mcp/pull/82) 已合并独立任务存储。
显式初始化用 `cargo run --locked --bin task_sqlite -- init <新库路径>`；
既有受支持 schema 的升级用 `task_sqlite upgrade <库路径>`。`task_sqlite check <库路径>` 只读、可在服务运行时执行，对整个任务登记表做全量体检（任务/请求/条目计数/占用/owner 与 stage/请求摘要的全部历史），发现问题时非零退出并逐条输出原因。受理事务内只校验在途任务与占用；终态历史的损坏改为在服务启动时（`data_service` 启动即做一次同样的全库体检，失败则拒绝启动）、`task_sqlite check` 或记录被读取时（`read_request`/`validate_snapshot`）检出，在途部分的保护不变。初始化不覆盖既有文件，普通读取不建库、不迁移、不回退 JSON。
`cargo run --locked --bin mcp_sqlite_dev -- <已初始化库路径>` 是独立开发入口，仅提供六个旧任务控制工具；
其同步 worker 只记录来源拒绝，不写行情。默认 `mcp_server` 的八工具及 JSON 装配保持。
两种存储没有跨库原子发布保证，同库行情发布属于 #79。

真实临时 SQLite 的受理、并发、进程退出、状态及故障测试随 `cargo test --locked` 运行；
准确代码 SHA、Actions 与未验证项见 [#78 实施记录](docs/tasks/issue-78.md)。
服务器磁盘受限，本轮完整编译和测试使用 Actions，本机仅格式与差异检查。

## SQLite 行情 Stage B（#79，已合并）

[PR #83](https://github.com/liqiangcc/x-data-mcp/pull/83) 提供库内 fixture/dry-run 能力：
结构化 daily/yearly、完整 legacy JSON/digest 核验、固定版本 SQL 范围读取，以及同库条件发布和任务输出关联。
Stage B 引入 schema v3；当前显式 task_sqlite init/upgrade 使用 schema v9；不自动迁移真实行情。默认两个 stdio 入口均未切换到行情写入。
历史固定 daily 可派生 immutable yearly，但不推进 yearly head；current-head 同时核对 daily/yearly head、任务修订与占用。
实现、完整性扫描成本、测试入口及未验证项见 [Stage B 记录](docs/tasks/issue-79.md)。
当前仍 NOT_PRODUCTION_READY；该切片不解除来源门禁（#54/#46 后经 FG 验收关闭，#6/#71 等仍 OPEN）。

## 验证

当前稳定入口与 CI 一致：

```sh
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked
git diff --check
```

`./scripts/verify` 尚未实现，不调用不存在的命令。

开发迭代（#272）：编辑循环用 `cargo check --all-targets`（本机约 2.7s）而非 `cargo build`（约 4.4s）；
`cargo test --no-run` 会重链 8 个域测试 target，迭代期用 `cargo test --test <名称>` 按需编译；
当前基线以 main 最近一次 CI 为准：`cargo nextest run --locked --no-fail-fast` 在 run 40118202118
（`37383ab`，2026-10-24）为 809 tests：809 passed、5 skipped（来源：该 run 的 nextest Summary 行）。
历史（2026-09-25 本机实测）：`cargo test --locked` 722 tests：712 passed、10 ignored（ignored 未执行）。
完整 `cargo test --locked` 仍是提交前与 CI 门禁，check 不替代测试。dev profile 使用
`split-debuginfo = "unpacked"` 将调试信息拆为 .dwo；本机对 mold/lld 链接器无显著收益，未固化。
计时数字为 2026-09-25 实测；其当时环境标注（本机 WSL2、8 核 / 15GiB、cargo 1.96.0）
未核实，可能与当前现场不同——当前现场见 [AGENTS.md](AGENTS.md)。详情与基线见
[#272](https://github.com/liqiangcc/x-data-mcp/issues/272)。

测试覆盖核心、发布门禁及真实 stdio 二进制的离线拒绝行为；Fake 不证明真实 parser/QFQ/yearly。
归档在线测试不进入 Cargo；官方来源与全市场的当前验收状态见上文“当前状态”。
预览包本机离线部署检查已 PASS；用户于2026-09-12报告 ChatGPT 当时会话能调用 13 工具，普通库没有已发布数据。
这不证明真实采集/发布闭环完成，见[闭环门禁与实施顺序](docs/source-closure.md)。
行情发布已改为[batch与version单点提交](docs/publish-storage.md)；[任务受理](docs/task-storage.md)也以单一快照提交绑定、执行状态和范围占用；当前仍要求单写者运行。

Issue #54 已 Final Gate PASS（[判定](https://github.com/liqiangcc/x-data-mcp/issues/54#issuecomment-5738653950)）：C1=每10股派现/C2=配股价/C4=每10股配股数，2dp HALF_UP 归一化 + 分位取整参考价 + 仿射 QFQ 链，94 事件零未解释；`GBBQ_QFQ_NOT_VERIFIED` 已退休（PR #252）。

## 通用 SQLite 数据入口（#80）

`cargo run --locked --bin mcp_data -- <已初始化 SQLite 路径>` 提供独立数据入口；
普通调用不建库、不升级、不导入 fixture。schema v9 升级仍由 `task_sqlite upgrade` 显式执行。
原 `mcp_server` / `mcp_sqlite_dev` 兼容入口保留。

新增 `get_data_capabilities`、`query_data`、`list_data_versions`、`get_data_availability`、
`list_tasks`、`get_task_progress`；控制工具接受互斥的旧参数与显式 v1 参数。
能力发现提供分层 schema/示例，读取固定版本与有界快照，行情批量任何已有版本损坏整体拒绝。
新 daily/catalog/yearly 请求真实受理到 SQLite；source readiness 仍 blocked，受控 worker 记录未验证失败，
不生成假行情。yearly 受理要求显式已发布 daily 及自然年；catalog fixture 只能显式测试装配。

本地 stdio 使用本机所有者上下文；应用层权限过滤测试不代表远程认证已交付。
已有版本的覆盖证据未明确证明请求范围时 availability=unknown，不从日期连续性推断完整。
[Stage C 边界与验收](docs/tasks/issue-80.md)记录预算、固定输入及测试。

`mcp_data` 同时提供 `compute_metrics`：一个固定 QFQ yearly version、明确年份窗口、
`consecutive_direction` 有界 inline 计算。支持 completed_only 与 provisional；
精确比较，累计变化按整数有理数一次 half-even 舍入至 12 位。
缺少年度覆盖证据返回 `not_computable`，不会把 year_unfinished=false 当成完整性证据。
当前完整性扩展仅供显式 fixture 验证，不解除真实来源门禁，见[Stage D](docs/tasks/issue-81.md)。

## 预发布包与本机检查

`mcp_data --version` 显示构建 SHA。预发布包包含 manifest.json、SHA256SUMS 和部署检查脚本，
明确标记 NOT_PRODUCTION_READY。适用 Linux x86_64 / glibc 2.39+；尚未验证其他平台。

解包后先核对 SHA256SUMS；`bin/task_sqlite init <新库路径>` 显式建空库，然后用
`bin/mcp_data <该库路径>` 配置本地 MCP stdio 客户端。没有 HTTP 监听或匿名远程入口。

`bin/create_demo <全新测试库路径>` 仅创建合成演示数据，不覆盖既有文件，正常服务不会自动调用。
`python3 check.py --bin-dir bin --report smoke-report.json` 在临时新库运行真实部署检查并写报告。
检查通过只证明当前二进制和 fixture 功能，不证明官方来源、真实行情或全部未来产品功能已完成。

`run_worker_once` 保留 Stage A 受控开发语义，并登记在 get_data_capabilities：会修改任务状态/占用，当前执行器仅记录来源拒绝，不能下载或发布行情；接口 ready 不代表生产 worker 就绪。

## 更新/补采开发中（#92，PR #93）

应用层已有 daily 显式区间合并判定及 SQLite 条件提交：有变化使用新版本，无变化记录独立复用结果；两者均与任务完成/占用释放同事务。schema v5 只增加更新结果表，显式 init/upgrade 才生效，不升级已部署预览库。
segment 完整不能代替合并全范围覆盖证据，旧 head/revision/固定版本变更均拒绝，不自动 rebase。当前仅 fixture 验证过发布链。MCP 已接入显式 daily 的 update_data/backfill_data 受理（共 15 工具），readiness=blocked；默认选择的 catalog 前置已有 bounded 官方快照目录发布验收（#194 v2），自动缺口发现及 changed-generation catalog 更新仍未完成；update/backfill 有界真实文件执行已落地并经 #186/#192 验收；yearly 显式分支见下文，不能通过提供证据字符串解除生产门禁。见 [#92 实施记录](docs/tasks/issue-92.md)。

更新/补采调用的 v1 输入为 request_id、dataset=market.daily、contract_version=v1、items；每项显式提供 entity、base_version_id、start_date、end_date。能力发现按 operation 返回准确 schema。受理响应的 contract_version 标识持久执行契约 explicit-range-task-v1；env 未装配官方快照时 worker 只记录 SOURCE_NOT_VERIFIED 不发布行情，装配后经 `TdxRangeProvider` 有界真实源执行。已部署 preview.2 的 13 工具与本开发分支不同，本段不是重新部署完成声明。

显式 daily 更新的 RangeRunner 已接入应用 worker：先查收据，running 无可靠结果不重采，已知来源拒绝记录失败。fixture 测试可显式注入可信 provider 完成原子发布及查询；生产 MCP 不能选择它：env 装配官方快照时用 `TdxRangeProvider`（显式 update）/`TdxBackfillProvider`（default frozen-gap backfill），否则 `OfficialFilePipeline` 拒绝。get_task_progress 同时返回 frozen_daily_ranges 与核验后的 range_results，包含原/结果版本、无变化标记和实际采集时间。

固定 daily 到 yearly 的应用候选构造在 [#94 切片](docs/tasks/issue-94.md)中实现：核验 base/证据后本地聚合，保留逐年 lineage，并复用现有双 head 条件发布。已接入显式 yearly 更新/补采工具和受控 worker；bounded 官方快照链路的生产 `OfficialYearlyVerifier` 已实现并经 FG 有界验收（见上文裁定），env 未装配时仍拒绝，不代表默认全目录来源通过。

yearly 内部任务契约现已冻结年度、base digest 与发布意图；schema v6 新增 yearly 执行结果，无变化复用已有版本、变化原子发布。两者均保留本次执行审计，不重写旧版本。显式 task_sqlite upgrade 才升级；线上预览库未迁移。公开 yearly 分支逐项传入 entity、base_daily_version_id、years、publication；mode 为 historical_fixed_base（可指定 comparison_version_id）或 current_head（expected_yearly_head 可空）。get_task_progress 返回 frozen_yearly_inputs/yearly_results，缺证据或篡改拒绝；生产 readiness 仍 blocked。

显式 daily 更新/补采的新请求按服务器 Asia/Shanghai 当日拒绝未来日期；批量任一越界整体拒绝并列明范围，不裁剪、不预约、不留下 request/task/占用。已受理同请求重放仍返回原冻结事实。

## preview.4 发布候选（#102）

此候选集成 16 工具、schema v7 与显式 verify_stopped_writes。关联 [#99](docs/tasks/issue-99.md) 与[从未执行证明](docs/tasks/issue-99-never-started.md)。
独立核实受理、审计、条件原子收尾、只读进度和既有结果重放已实现。
默认仅能核实受支持契约的从未开始任务：revision=0、无 owner/执行结果及发布关联，
收尾事务排除旧执行器后来领取。已开始或未知执行仍 STOP_EVIDENCE_NOT_VERIFIED，保留占用。
能力总体 blocked，具体适用边界见工具描述；检查 succeeded 不表示原任务已停止，须读取 stopped_write_check 结论。
公共参数不能提供“已停止”声明或强制解锁，其他核实/恢复类型仍待实现。

候选准备时线上为 preview.3 / schema v6 / 15 工具；本段不宣称已经部署。
发布须校验包与精确 SHA、隔离 synthetic smoke、停目标服务后备份、显式 v6→v7 升级再切换。
旧库/旧包/旧 demo 保留，普通库不导入演示数据。实际部署证据维护在 Issue #102。
真实来源仍 blocked，NOT_PRODUCTION_READY；#54/#6/#46/#71/文件一致性门禁不解除。
新版 ChatGPT 消费侧调用在实际验证前为 NOT_RUN，本机 stdio 或 tunnel health 不能替代。

已合并的 #104 / PR #105 增加发布核实证据读取、独立受理与条件审计（显式 schema v8）。
核实任务记录检查结论，不修改原任务或行情/head；缺收据不等于未发布。
开发分支现接入第 17 个工具 verify_publication_result、类型化 worker 分派与 publication_check 进度。
该核实接口 ready 仅表示可检查已有 SQLite 发布证据；缺收据为 insufficient_evidence，损坏/异常检查保留保护。
已知 all_recorded 可绑定新请求且不新建检查；调用者不能提供证据/强制结论。完整回归与审核仍在进行。
线上 preview.4/schema v7 不会自动升级；完整能力与部署以 Issue #104 的实际验证为准。


## 异常核实任务有界检查（#106 / PR #107）

本开发分支的 Linux mcp_data 有 18 个工具、显式 schema v9。新增 `inspect_verification_task` 仅供所有者：参数为 contract_version=v1、request_id、target_task_id（核实任务）。它不递归创建任务；每次可靠结果（包括 insufficient_evidence）都消费 request_id。同 ID 返回历史结果，新 ID 才重查。`get_request` 可读该无任务终态；请求结束不代表目标结束。

当前核实 worker 在 Claim 同事务登记实际 Linux 本地文件 guard，并持锁覆盖整个执行。检查分别核验执行已结束与持久收尾事实；足够证据时只中断核实自身、释放核实串行保护，不改原业务任务、行情或 head。未登记旧执行、缺失/替换 guard、内核或文件系统边界不符均不能产生停止证明；已有合法结果不回滚。正常运行不强制中断。条件冲突至多重新检查一次，未知提交先读持久请求，不盲目重试。

本切片的 Actions 验证及执行者自查按准确 HEAD 记录，结果见 [#106 记录](docs/tasks/issue-106.md) 和 PR。线上仍为已部署 preview.4/schema7/16 工具，本段不是新版部署声明。真实来源、#53 四股及新版 ChatGPT 消费侧 NOT_RUN；生产仍 NOT_PRODUCTION_READY，来源门禁未解除。


## preview.5 发布准备（#108）

本版本集成已合并 #104/#106：18 工具、显式 schema v9，包含发布结果核实与异常核实任务的有界处置。运行期 guard 仅适用于已登记的 Linux 本地执行；不为旧未登记执行补造停止证明。

发布准备基线为 main ec50623dc385b873339273e5d8c97ad9dda12ee5。当前线上仍 preview.4/schema7；本节不是部署完成声明。正式切换前须验证发布 SHA 对应包/manifest/binary、隔离 synthetic smoke，暂停目标服务后保留库/配置备份，显式升级 schema7→9，再切换同一 tunnel。普通库不导入 demo，旧包/备份保留。

准确 tag、SHA、升级与 stdio/tunnel 验收记录见 [#108](https://github.com/liqiangcc/x-data-mcp/issues/108) 和[发布操作记录](docs/tasks/issue-108.md)。ChatGPT 消费侧在实际调用前为 NOT_RUN；来源/#53 四股仍 NOT_RUN，#6/#46 仍 NOT_VERIFIED，NOT_PRODUCTION_READY（#54/#71/文件一致性已按 FG 裁定退休阻塞串）。其他恢复、目录和真实数据闭环继续未完成。

## 长期服务与本机 IPC（#110）

新增 Linux `data_service <已初始化库> <私有目录/socket> [--poll-ms <10..60000>]` 与
`mcp_connect <私有目录/socket>` 独立入口。后者只代理协议，服务持有共同启动身份及单 worker；
旧非终态不自动续跑。socket 目录需本人所有且0700，库文件需0600；不自动修改权限、建库或升级。
默认 worker 轮询100ms，只执行显式受理任务。读取及取消可使用其他会话，手动 worker 与后台串行，
忙时返回 `worker_busy`，停止中返回 `worker_stopping`。工程预算与实际证据见 [#110](docs/tasks/issue-110.md)。
IPC正常空闲不会结束会话；30秒读取预算从收到未完整帧的第一字节起计算，修复见 [#112](docs/tasks/issue-112.md)。
preview.6 已将实际 tunnel profile 接到 `mcp_connect`；旧 `mcp_data` 等兼容开发入口不与服务同库混用。
仍 NOT_PRODUCTION_READY，来源门禁不变。源码/fixture 不代表真实来源或新版 ChatGPT 调用通过。

## 当前预览部署与空闲修复（#108/#112）

2026-09-13 已部署 [v0.2.0-preview.6](https://github.com/liqiangcc/x-data-mcp/releases/tag/v0.2.0-preview.6)，
二进制代码 `7e37d27a8d85e921d8162d7e49377ad1700d6ada`，普通库schema9、独立worker及18工具。
同一实际代理stdin保持打开32秒后再次调用工具通过；实际tunnel profile已由旧式 `mcp_data` 改接
`mcp_connect`，数据服务保持同一进程，tunnel后端代理及控制面轮询检查通过。
单独修改mcp.json不足以证明tunnel装配改变，必须检查其实际profile与进程。

preview.5空闲退出、preview.6首次检查发现profile仍指向旧入口的失败记录均保留。
准确CI、包摘要、备份、调用与运行边界见[部署结果](docs/tasks/preview6-results.md)。
本节更新此前各版本准备段落中的历史线上状态；旧段落不是当前版本声明。
普通库仍为空，来源门禁未解除，ChatGPT本次消费侧NOT_RUN。
NOT_PRODUCTION_READY：本机部署检查通过不代表真实数据闭环或完整目标已完成。

2026-09-13 已将主线 `2b92fa71884283bc8176511b7b8b7d886b7fc2a2` 发布为 [v0.2.0-preview.7](https://github.com/liqiangcc/x-data-mcp/releases/tag/v0.2.0-preview.7) 并切换预览服务；运行时和隧道验收通过，普通库仍为空，真实来源与 ChatGPT 消费侧均 `NOT_RUN`。详见[preview.7 部署结果](docs/tasks/preview7-results.md)。这不解除 #54/#6/#46/#71 或文件一致性门禁，系统继续 `NOT_PRODUCTION_READY`。

随后主线 `1f62813d45223f0926457aff07e5344c419f39f1` 已发布为 [v0.2.0-preview.8](https://github.com/liqiangcc/x-data-mcp/releases/tag/v0.2.0-preview.8) 并切换预览服务；本版本增加官方快照角色—路径绑定，运行时和隧道验收通过，普通库仍为空。详见 [preview.8 部署结果](docs/tasks/preview8-results.md)。真实来源、ChatGPT 消费侧和所有 QFQ/目录/授权门禁仍未解除，系统继续 `NOT_PRODUCTION_READY`。

随后主线 `fca27058d6608b2d5ae4af87798cebba43cccb58` 已发布为 [v0.2.0-preview.9](https://github.com/liqiangcc/x-data-mcp/releases/tag/v0.2.0-preview.9) 并切换预览服务；本版本再增加官方快照角色—文件名绑定，运行时和隧道验收通过，普通库仍为空。详见 [preview.9 部署结果](docs/tasks/preview9-results.md)。真实来源、ChatGPT 消费侧和所有 QFQ/目录/授权门禁仍未解除，系统继续 `NOT_PRODUCTION_READY`。

随后主线 `22c2255210c68b49d08ffd24a6cfb3468274f07a` 已发布为 [v0.2.0-preview.10](https://github.com/liqiangcc/x-data-mcp/releases/tag/v0.2.0-preview.10) 并切换预览服务；普通库按显式 `task_sqlite upgrade` 由 schema9 升级到 schema10（无回填），运行时和隧道验收通过，普通库仍为空。

随后主线 `feca5e2448dc8bd7da7d8a8e530d239fc358712a` 已发布为 [v0.2.0-preview.11](https://github.com/liqiangcc/x-data-mcp/releases/tag/v0.2.0-preview.11)。自本版本起发布走 #140 流水线：向 main 上的提交推送 `v0.2.0-preview.N` tag 即触发 `release.yml` 自动构建、校验并创建 prerelease（tag 版本与 `Cargo.toml` 不一致或 tag 不在 main 上时拒绝），同 workflow 把验证过的包推到 `refs/dist/<tag>`；部署使用 `scripts/deploy_preview.sh <tag>`（`--force-git-transport` 经 `refs/dist` git 取包），完成一致备份、显式 schema 检查、原子 `current` 切换、data/tunnel 重启与五份报告，失败自动回滚并写 `FAILED_AT_*` 报告。preview.11 部署与 `--from-report` 回滚均已实操通过。详见 [preview.11 部署结果](docs/tasks/preview11-results.md)。真实来源、ChatGPT 消费侧和所有 QFQ/目录/授权门禁仍未解除，系统继续 `NOT_PRODUCTION_READY`。

以上 `deploy_preview.sh` 段落是 preview.11 的单 data-service + tunnel 历史部署证据。
该脚本未安装/configure 当前 `acquire_service` 与 immutable generation root，不能用于
#262 双 MCP 拓扑或作为 generation/full-market DoD 证据。
