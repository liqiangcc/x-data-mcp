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

Box（Tailscale 节点 `grok-bot-vm-411252337`，SSH `box@grok-bot-vm-411252337`；旧节点 `cursor` 已被回收下线，磁盘数据无损）环境事实：`/home` 与 `/tmp` 为 overlayfs，runtime-guard 契约对其 fail-closed，全量测试须以 `TMPDIR=/dev/shm`（tmpfs）运行；Box 自带 cargo 1.85.1 无法编译本仓库，使用 rustup stable 1.98.1（`~/.cargo/bin`）。

固定证据输入目录 `/home/box/retained-inputs/`（见其中 MANIFEST.md），官方文件只下载一次入库复用，重复下载需说明理由；gen-1 retained 字节永久不可得，跨代一致性走已提交派生证据。

Box 补充事实：本机无白名单内本地盘（唯一块设备 vda 即 overlay 后端，`/dev/shm` 仅 64MB，`CapEff=0` 无法自行挂载数据盘）。`data_service` 与验证执行器共用 `local_filesystem::supported_file` 守卫；开发/preview 场景可用显式 opt-out `X_DATA_MCP_UNSAFE_NONLOCAL_FS=1`（#213）放行 overlayfs——单例租约与身份绑定语义随之失效，仅限本地验收，生产禁用；客户端 `mcp_connect` 连接时同样需要该变量。
