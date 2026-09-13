# x-data-mcp

基于 Rust 的数据治理与 MCP 项目，主要负责数据接入、质量治理、版本与来源追溯，以及通过 MCP 提供数据能力。

## 项目方向

- 数据是主要关注点：明确证券身份、数据范围、完整性、新鲜度、来源和版本。
- MCP 是外部入口，复用核心用例与能力契约，不成为另一套数据处理实现。
- 使用 Rust；性能目标需在具体用例的验收标准中明确，并通过代表性数据与基准测试验证。
- 当前不建设交易练习界面、订单撮合或自动交易系统。

首期证券范围为当前沪深 A 股及 TDX 研究行业二级。生产来源目标已收敛为官方可下载文件及本地解析/计算；来源证据、存储交付及性能指标仍需按 Issue 验收，方向确认不表示能力已交付。

## 开发规范

本项目按照 [AI Engineering Conventions](https://github.com/liqiangcc/ai-engineering-conventions) 开发。采用版本、必读专题、适用范围和实践反馈方式统一记录在[规范采用说明](docs/conventions.md)，AI 开发入口为 [AGENTS.md](AGENTS.md)。

研发以 [GitHub Issues](https://github.com/liqiangcc/x-data-mcp/issues) 为任务入口：先明确需求、主要 Use Case、Business Rule 和 Acceptance Criteria，再进行设计、实现与验证。每个任务保留实际提交及验证证据。

## 与旧项目的关系

[liqiangcc/x](https://github.com/liqiangcc/x) 是需求线索、历史实现、数据契约与回归场景的参考。旧行为不能替代已确认业务要求；不整体复制旧仓库的数据、运行记录或实现。

是否迁移某项能力或数据，应由对应 Issue 明确来源、范围、兼容性、数据治理责任和验收方式。

## 当前状态

**NOT_PRODUCTION_READY — production source convergence stage 1。**

已有 Rust 核心、任务受理/取消/重试、发布校验、目录核心和 stdio MCP 原型。
生产来源决策见 [Issue #55](https://github.com/liqiangcc/x-data-mcp/issues/55)，
完整边界见[官方文件架构](docs/production-source.md)。

唯一目标链路：官方 base/行业文件、hsjday .day、gbbq 快照 → 本地 QFQ daily →
从同一已发布 QFQ daily version 本地聚合 yearly。不存在多源生产 fallback。

- BaoStock、源端直接 QFQ 仅 evidence/交叉验证。
- TdxQuant/TdxAiData/tqserver、东财匿名 datacenter/gzfx 不接入生产。
- Python bridge/eltdx 在线装配已移除，源码与旧测试保留在 `archive/online-tdx/`。
- 源端直接年线不再是生产契约；MCP `year` 表示本地派生输出。
- 既有 `evidence/` 和历史文档原样保留；历史结论不等于当前生产状态。

#6/#46 目录与研究行业二级文件能力、#54 GBBQ/QFQ 数值语义、官方文件长期服务器获取/使用边界、
.day/gbbq 独立快照的一致性仍未通过。[.day原始解析器](docs/day-file.md)已实现；GBBQ、QFQ 和真实文件发布闭环尚未实现。本地[年线聚合核心](docs/yearly.md)已实现，尚未接入生产入口。
已提供[本地输入清单核验](docs/snapshot-input.md)，字节匹配不代表跨文件一致或来源授权。
默认装配的 `OfficialFilePipeline` 始终返回 `SOURCE_NOT_VERIFIED`，没有开关可绕过门禁。

## 本地入口

`cargo run --locked --bin mcp_server` 启动 stdio 原型。`X_DATA_MCP_STORE` 设置本地状态目录，
默认 `.x-data-mcp`。旧 `X_DATA_MCP_TDX_PYTHON` 不再读取，不需要 Python/eltdx/Key。

工具为 `start_collection`、`get_task`、`run_worker_once`、`cancel_task`、`retry_task`，
以及只读的 `get_market_data`、`list_market_data_versions`、`get_request`，见[查询契约](docs/published-queries.md)。
受理会写入任务并返回 pending；必须显式运行 worker，当前采集会失败并报告未验证，不能发布新行情。
年线没有已发布 daily 时报告 `PUBLISHED_QFQ_DAILY_REQUIRED`。
此入口不是部署完成或自动后台执行的证明。

## SQLite 任务开发入口（#78）

[PR #82](https://github.com/liqiangcc/x-data-mcp/pull/82) 已合并独立任务存储。
显式初始化用 `cargo run --locked --bin task_sqlite -- init <新库路径>`；
既有受支持 schema 的升级用 `task_sqlite upgrade <库路径>`。初始化不覆盖既有文件，普通读取不建库、不迁移、不回退 JSON。
`cargo run --locked --bin mcp_sqlite_dev -- <已初始化库路径>` 是独立开发入口，仅提供六个旧任务控制工具；
其同步 worker 只记录来源拒绝，不写行情。默认 `mcp_server` 的八工具及 JSON 装配保持。
两种存储没有跨库原子发布保证，同库行情发布属于 #79。

真实临时 SQLite 的受理、并发、进程退出、状态及故障测试随 `cargo test --locked` 运行；
准确代码 SHA、Actions 与未验证项见 [#78 实施记录](docs/tasks/issue-78.md)。
服务器磁盘受限，本轮完整编译和测试使用 Actions，本机仅格式与差异检查。

## SQLite 行情 Stage B（#79，已合并）

[PR #83](https://github.com/liqiangcc/x-data-mcp/pull/83) 提供库内 fixture/dry-run 能力：
结构化 daily/yearly、完整 legacy JSON/digest 核验、固定版本 SQL 范围读取，以及同库条件发布和任务输出关联。
Stage B 引入 schema v3；当前显式 task_sqlite init/upgrade 使用 schema v7；不自动迁移真实行情。默认两个 stdio 入口均未切换到行情写入。
历史固定 daily 可派生 immutable yearly，但不推进 yearly head；current-head 同时核对 daily/yearly head、任务修订与占用。
实现、完整性扫描成本、测试入口及未验证项见 [Stage B 记录](docs/tasks/issue-79.md)。
当前仍 NOT_PRODUCTION_READY；该切片不解除 #54/#6/#46/#71 来源门禁。

## 验证

当前稳定入口与 CI 一致：

```sh
cargo fmt --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked
git diff --check
```

`./scripts/verify` 尚未实现，不调用不存在的命令。
测试覆盖核心、发布门禁及真实 stdio 二进制的离线拒绝行为；Fake 不证明真实 parser/QFQ/yearly。
归档在线测试不进入 Cargo，真实官方来源验证为 NOT_RUN；全市场覆盖和性能为 NOT_VERIFIED。
预览包本机离线部署检查已 PASS；用户报告 ChatGPT 当前会话能调用 13 工具，普通库没有已发布数据。
这不证明真实采集/发布闭环完成，见[闭环门禁与实施顺序](docs/source-closure.md)。
行情发布已改为[batch与version单点提交](docs/publish-storage.md)；[任务受理](docs/task-storage.md)也以单一快照提交绑定、执行状态和范围占用；当前仍要求单写者运行。

Issue #54 的 [GBBQ 十进制诊断矩阵](evidence/issue-54-gbbq-decimal/README.md)已保存：仅 evidence，官方 normalization 契约仍 NOT_VERIFIED，未解除生产门禁。

## 通用 SQLite 数据入口（#80）

`cargo run --locked --bin mcp_data -- <已初始化 SQLite 路径>` 提供独立数据入口；
普通调用不建库、不升级、不导入 fixture。schema v7 升级仍由 `task_sqlite upgrade` 显式执行。
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
segment 完整不能代替合并全范围覆盖证据，旧 head/revision/固定版本变更均拒绝，不自动 rebase。当前仅 fixture 验证过发布链。MCP 已接入显式 daily 的 update_data/backfill_data 受理（共 15 工具），readiness=blocked；默认选择、自动缺口、catalog 更新及真实文件执行仍未完成；yearly 显式分支见下文，不能通过提供证据字符串解除生产门禁。见 [#92 实施记录](docs/tasks/issue-92.md)。

更新/补采调用的 v1 输入为 request_id、dataset=market.daily、contract_version=v1、items；每项显式提供 entity、base_version_id、start_date、end_date。能力发现按 operation 返回准确 schema。受理响应的 contract_version 标识持久执行契约 explicit-range-task-v1；当前 worker 只记录 SOURCE_NOT_VERIFIED，不发布行情。已部署 preview.2 的 13 工具与本开发分支不同，本段不是重新部署完成声明。

显式 daily 更新的 RangeRunner 已接入应用 worker：先查收据，running 无可靠结果不重采，已知来源拒绝记录失败。fixture 测试可显式注入可信 provider 完成原子发布及查询；生产 MCP 不能选择它，仍只使用 OfficialFilePipeline。get_task_progress 同时返回 frozen_daily_ranges 与核验后的 range_results，包含原/结果版本、无变化标记和实际采集时间。

固定 daily 到 yearly 的应用候选构造在 [#94 切片](docs/tasks/issue-94.md)中实现：核验 base/证据后本地聚合，保留逐年 lineage，并复用现有双 head 条件发布。已接入显式 yearly 更新/补采工具和受控 worker，成功执行只用 fixture verifier 验证；生产 verifier 仍拒绝，不代表来源通过。

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

开发中的 #104 / PR #105 增加发布核实证据读取、独立受理与条件审计（显式 schema v8）。
核实任务记录检查结论，不修改原任务或行情/head；缺收据不等于未发布。
公开 runner/MCP 尚未完成，线上 preview.4/schema v7 不会自动升级；完整能力与部署以 Issue #104 的实际验证为准。
