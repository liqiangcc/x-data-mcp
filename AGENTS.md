# AGENTS.md

## 修改前阅读

1. 阅读 [README.md](README.md)，确认项目边界与当前状态。
2. 阅读 [docs/conventions.md](docs/conventions.md)，按其中固定版本读取规范与任务相关专题。
3. 使用 `gh` 读取本仓库目标 Issue、讨论及关联证据，核对实际分支、提交和工作区。

规范采用版本与链接只在 `docs/conventions.md` 维护。本文件仅提供执行入口，不复制整套上游规则。

## 项目约束

- 实现语言为 Rust。跨语言工程规范适用，Java 专项不直接套用；需要的 Rust 专项约定由真实用例推动形成。
- 数据治理是核心职责，MCP 是入口 Adapter；业务决策、流程编排、能力契约、技术实现与装配分别维护。
- 研发任务以本仓库 Issue 为入口。AC 来自需求共识、业务规则或公开 Contract，不能仅依据旧项目或当前实现反推。
- 未确认的数据范围、存储策略、历史时点语义和性能阈值不能写成既定要求。只阻塞依赖该决定的工作。
- 不因采用 Rust 就宣称高性能；性能证据须注明数据规模、环境、并发、测量方式及代码版本。
- 不为本次初始化迁移旧行情、密钥、数据库或运行产物。后续数据迁移按具体 Issue 的范围执行。

## 验证与反馈

使用项目已有稳定验证入口；当前可执行能力见 README，不运行不存在的命令，不伪造 PASS。建立验证入口时，按上游语义明确 target 范围、结果、退出码和未验证项。

实际开发暴露的规范歧义、冲突或不必要成本，按 `docs/conventions.md` 的反馈流程记录；不静默绕过，也不为了满足形式增加无价值抽象。

开发与验收环境为本机 WSL2（`/root/x-data-mcp`，8 核 / 15GiB，cargo 1.96.0 rustup stable）：根文件系统非 overlayfs，runtime-guard 本地通过；`/dev/shm` 为 tmpfs。官方输入位于本机 `/opt/x-data-mcp-preview/official-inputs/`（见其中 snapshot-manifest.json），官方文件只下载一次入库复用，重复下载需说明理由；gen-1 retained 字节永久不可得，跨代一致性走已提交派生证据。

Box（旧 Tailscale 节点 `grok-bot-vm-411252337`）已于 2026-09-25 回收下线：其上 `/home/box/retained-inputs/`、迁移协作 mailbox 与 agent 会话随之失效；Node.js 迁移（#271）已冻结，实现与证据保留在 `migration/*` 分支。dev/preview 场景的 `X_DATA_MCP_UNSAFE_NONLOCAL_FS=1`（#213）仅限本地验收，生产禁用；客户端 `mcp_connect` 连接时同样需要该变量。

ECS 部署目标 `ecs-node`（OpenSSH 别名，key auth，`ssh ecs-node` 本机直接可用）：x86_64，2 vCPU，约 2GiB RAM，40GB 根盘。它用于最终部署、真实 MCP 路径与最终 disk/WAL
验收。注意：生产部署实际运行在本机 `/opt/x-data-mcp-preview/`（`x-data-mcp-data`/`x-data-mcp-acquire` systemd 单元 + `run/mcp.sock`/`run/acquire.sock` unix socket）；本机即承载生产实例，`ssh ecs-node` 别名当前未配置。

生产运行事实（preview.29 @ 4493d45，2026-09-29 验证）：

- 部署管线：版本 bump + tag + `scripts/build_release.sh` + `scripts/smoke_release.py --bin-dir <解包目录>/bin --report <path>` + `gh release create --target <sha>`（tag 已存在时也必须传 `--target`，否则 `targetCommitish` 落成默认分支名，smoke 绑定校验失败）+ `deploy_phase_g.sh <tag>`（在 git 仓库目录内运行，`GH_REPO` 需就绪；release 资产名必须含 `<tarball>`、`<tarball>.sha256`、定名 `smoke-report.json`；每次部署归档旧库并全新 init 最新 compact schema（现 v20），非原地升级）。
- 契约要点：`securities.catalog` 的 selection 为 `{"scope":"current_sh_sz_a"}`；已有 daily head 的实体禁止 `start_collection`（`market.daily` 返回 `existing daily head requires the update_data range path`），必须走 `update_data` 的 `explicit-range-task-v1` item（entity/base_version_id/start_date/end_date）；`market.yearly` 的 collection 被 worker 设计性拒绝，年线一律走 `update_data`（item 含 `base_daily_version_id`、`years`、`publication.expected_yearly_head`）。
- 跨代日更：coverage reason 绑定新 manifest sha → typed ops 统一回退 envelope（#289）；除权股 `.day` member 非前缀追加 → 全量重写为根 checkpoint 版本（`continuity_mode=3`/`Full`，parent_version_key=NULL，亲缘链在 `market_update_results`），仅前向窗口可走；窗口覆盖被改写历史日仍 `RevisionUnverified` fail-closed（#290/#293）。
- preview.29 runtime：capability revision `generic-data-v3`；Phase-G fresh init 新库为 compact schema 20；旧 801MB DB 归档于 `/opt/x-data-mcp-preview/state/preview.sqlite.v20-archived-20260928T053759Z`。2026-09-29 explicit fresh rebuild 后，2026-09-30 operator-path 已激活 generation `2e9183306d4292c96602185152770a5ef54fbc28d1c9c909d3297c9d9bfe70a8`，并发布 catalog `catalog-1790660536962300201`（5,227 records）、5,223 个 `market.daily` published versions 和 5,223 个 `market.yearly` derived versions；最新集中行情日为 2026-09-29（5,211 entities）；4 个官方 `hsjday` 成员缺失保持 absent（`sz.001246`、`sz.301569`、`sz.301660`、`sz.301718`）；当前 DB 约 881MB；`x-data-mcp-data`/`x-data-mcp-acquire` 双 systemd 服务 active；x 消费侧 bounded `KlineReader` live readback 已通过（`liqiangcc/x` PR #150），完整 simulator/Security Master 仍非该 slice；production_ready 仍为 false。
- preview.26–28 历史验收实测下限：同代无变化 `update_data` 全量 5,222 项 ≈ 116ms/item 平均（冷启 ~226ms → 稳态 50-70ms，generation 缓存）；跨代 envelope ≈ 430ms/item；首采 5,227 entity ≈ 340s；年线全量 ≈ 900s。
