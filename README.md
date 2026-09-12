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
.day/gbbq 独立快照的一致性仍未通过。parser、QFQ 和 yearly 算法尚未实现。
默认装配的 `OfficialFilePipeline` 始终返回 `SOURCE_NOT_VERIFIED`，没有开关可绕过门禁。

## 本地入口

`cargo run --locked --bin mcp_server` 启动 stdio 原型。`X_DATA_MCP_STORE` 设置本地状态目录，
默认 `.x-data-mcp`。旧 `X_DATA_MCP_TDX_PYTHON` 不再读取，不需要 Python/eltdx/Key。

工具为 `start_collection`、`get_task`、`run_worker_once`、`cancel_task`、`retry_task`。
受理会写入任务并返回 pending；必须显式运行 worker，当前采集会失败并报告未验证，不能发布新行情。
年线没有已发布 daily 时报告 `PUBLISHED_QFQ_DAILY_REQUIRED`。
此入口不是部署完成或自动后台执行的证明。

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
归档在线测试不进入 Cargo，真实官方来源验证为 NOT_RUN；部署、全市场覆盖和性能为 NOT_VERIFIED。
既有发布/任务非原子缺陷不在本阶段修复。
