# AGENTS.md

## 修改前阅读

1. 阅读 [README.md](README.md)，确认项目边界与当前状态。
2. 阅读 [docs/conventions.md](docs/conventions.md)，按其中固定版本读取规范与任务相关专题。
3. 使用 `gh` 读取本仓库目标 Issue、讨论及关联证据，核对实际分支、提交和工作区。

规范采用版本与链接只在 `docs/conventions.md` 维护。本文件仅提供执行入口，不复制整套上游规则。

## 项目约束

- 当前实现为 Rust；用户已指定全量 Node.js 迁移方向，迁移设计见 `docs/tasks/nodejs-full-migration-plan.md`。迁移按契约与阶段验收推进，不能提前宣称已切换。跨语言工程规范适用，Java 专项不直接套用。
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

ECS 部署目标 `ecs-node`（OpenSSH 别名，key auth，`ssh ecs-node` 直接可用）：x86_64，2 vCPU，约 2GiB RAM，40GB 根盘。它用于最终部署、真实 MCP 路径与最终 disk/WAL
验收；250 日 fixture micro-soak 可在 Box `/dev/shm` 执行，但不替代全市场 AC。
截至 2026-09-21，100-item ECS 功能 smoke 已完成而 #259 容量 AC 未通过；当前 schema
禁止在 ECS 上启动 5,221-symbol collection，须先修复 audit history payload 与
canonical `bar_json` 布局。

## Codex / Devin 协作（2026-09-23）

用户指定 Codex 为 Lead，通过 tmux 指挥全新 Devin 会话，Devin 固定使用 `swe-2-high`。正式迁移采用全新 Lead + 全新 Worker；当前 `codex` 会话只负责建立规则和交接。这是本项目的分工协议；不假定两个会话共享上下文，也不把 tmux 存活视为自动持续调度。

### 角色与隔离

- Lead 模型由用户指定为 `gpt-6-sol`，推理强度 `medium`；Worker 仍为 `swe-2-high`。未经用户指定不自动升级；启动时核验实际模型与强度，配置意图不等于生效证明。

- 新 Lead 会话命名 `codex-node-lead`：负责需求、任务拆分、架构裁决、审查、独立验证和阶段验收。Devin Worker 会话命名 `devin-node`：完成有界任务、自测、提交证据、报告阻塞。只有完成接管握手的新 Lead 可以派发正式任务；旧 `codex` 会话接管后停止派单，用户明确重新指定除外。
- Lead 独立 worktree 为 `/home/box/x-data-mcp-node-lead`，初始分支 `migration/node-lead`；Worker 独立 worktree 为 `/home/box/x-data-mcp-node`，初始分支 `migration/node-devin`。两者从同一冻结提交创建，必须包含本协作规则。主工作区仍为 `/home/box/x-data-mcp`。运行前核对实际路径、分支、SHA、工作区状态与模型，不能凭此表假定现场不变。
- 新建会话不等于已经交接。初始化者留下 BOOTSTRAP.md：用户目标与已授权范围、规则提交 SHA、迁移方案路径、已确认事实/待决问题、实际会话与 worktree 映射、首项任务、接管状态。新 Lead 读取后写 TAKEOVER.md，核对两端身份与基线并完成 Worker ACK，才将 session.json 的 active_lead 指向自己；初始化者看到 TAKEOVER 后不再派单。未握手时状态为 BOOTSTRAPPING，不启动迁移实现。
- 不复用 `d` 或 `devin-epic` 的上下文，不向它们发键、不停止它们、不改变其工作区。历史事实通过已提交文档和证据引入。Lead 审查执行 worktree 时以只读为默认；需要修改先交接所有权，禁止双方同时修改同一文件。
- 新会话显式阅读本文件、README、docs/conventions.md、迁移方案和本轮任务文件。全量迁移实施需建立独立 Issue/Epic，关联 #262；#262 不自动代表迁移范围已验收。启动握手和只读盘点可先执行。

### tmux 通信与持久交接

- tmux 用于投递短指令、查看输出和通知；长任务与结果使用文件。共享邮箱位于 `$(git rev-parse --path-format=absolute --git-common-dir)/agent-coordination/node-migration/`，不提交会话日志、终端转储或秘密。Lead 维护 `session.json`，记录两端 session/pane ID、worktree、分支、基线 SHA、模型和当前 task ID。
- 使用唯一 task ID（如 `NM-000`）；Lead 写 `TASK-<id>.md`，包括目标、关联 Issue/来源、基线、允许修改范围、非目标、AC、验证命令和停止条件。Devin 写 `ACK-<id>.md` 与 `RESULT-<id>.md`，Lead 写 `REVIEW-<id>.md`。每个文件只有一个写入者，临时文件加 rename 发布完整结果。
- 同时最多一个未验收实现任务。状态为 DISPATCHED → ACK → RUNNING → READY_FOR_REVIEW → ACCEPTED / CHANGES_REQUESTED；阻塞用 BLOCKED，并写明证据和所需决策。ACK 必须复述范围、实际 cwd/branch/SHA/model。重复投递相同 task ID 只确认状态，不重复执行副作用。
- 投递前用 `tmux list-panes` 核对目标 pane ID、cwd 和前台程序，用 `capture-pane` 确认输入状态。仅在 agent 输入框可接收消息时，以 `tmux send-keys -t <pane-id> -l -- '<短指令>'` 发送文字，再单独发送 Enter；多行正文通过任务文件读取。不得向 shell、权限弹窗、未知进程或用户正在编辑的输入框盲发文字/Enter；不得自动发送 Ctrl-C、Escape 或选择批准。
- Devin 回报先落盘，再用 `tmux display-message -t <lead-pane-id> 'NM-xxx READY_FOR_REVIEW: <结果路径>'` 通知。Lead 活跃时轮询结果文件并读取执行 pane。display-message 是提示，不保证唤醒或投递给模型；Lead 忙时禁止向其输入框盲注入。Lead 空闲且输入状态已确认时才可 send-keys 投递短回报。Lead 不在线时保留结果并等待，不擅自推进下一阶段。
- **投递必须验证闭环**：先用 `send-keys -l` 输入文字，等终端处理完成并确认完整文字仍在目标 agent 的输入框，再独立执行 `tmux send-keys -t <pane-id> Enter`（Enter 不使用 `-l`，否则可能输入字面文本）。不要把粘贴与 Enter 紧邻发送后就假定成功；终端的粘贴/输入处理可能尚未完成。
- 发送 Enter 后再次读取目标可见 pane，确认该消息成为已提交对话、输入框清空或 agent 出现处理/工具执行状态；再等待对应 task ID 的 ACK。`send-keys` 退出码为 0 只说明按键已投递，不证明消息已提交或任务已受理。状态分为 TEXT_ENTERED、SUBMITTED、ACKED；只有确认提交才能报告“已发送”，只有 ACK 才能报告“已接收”。
- 若文字仍留在输入框且目标 agent 明确空闲、没有权限弹窗，也没有用户介入，补发一次 Enter 并重新检查；不要重复粘贴全文，不要连续盲按 Enter。若状态不明确，先检查或报告阻塞。该规则同时适用于 Lead→Worker 与 Worker→Lead。
- 未见 ACK 时先检查 pane 和文件；不能因超时重复提交任务。会话重启/压缩后从 session.json、当前 TASK/RESULT/REVIEW、git 状态恢复，不以滚屏或历史总结替代事实。

### 验收与推进

- RESULT 包含实际修改文件与提交 SHA（未提交则明确说明）、命令与退出码、证据位置、PASS/FAIL/NOT_RUN、风险及下一步建议。提交完成或 Devin 自报 PASS 均不代表 Lead 验收。
- Lead 检查 diff、契约/业务规则及证据，独立运行相关验证；写 REVIEW 后才派下一任务。性能报告固定输入、环境、代码 SHA、并发、计时边界和原始样本，分别说明开发反馈速度、运行耗时、内存和 DB/WAL；不因改成 Node 就断言更快。
- 默认按迁移方案 P0–P6 推进。先冻结契约与基线，再验证精确数值/摘要、SQLite 原子发布与恢复、Linux 租约/UID 能力；保持 fail-closed、数据治理和历史版本语义。
- 任务授权范围内的本地修改、测试与修复自主完成；不得因协作协议擅自部署、切换真实数据库、删除数据、push/merge 或发布。需要外部动作时由 Lead 核对用户已有授权，不能反复索要已给出的授权。
- 不自动切换模型。`swe-2-high` 不可用、上下文不足或任务边界不清时先报告；更换会话要写交接并重新握手。执行者完成当前任务后等待 Lead，不能仅凭宏观目标自派未审查的新阶段。

## Lead/Worker completion delivery hard gate (2026-09-23)

- A tmux notification is not delivered until the text and the Enter key are sent as separate operations.
- After `send-keys -l`, capture the target pane and confirm the complete message remains in the agent input box.
- Send a separate `Enter` (without `-l`), then capture the pane again and confirm the message was submitted, the input cleared, or the agent entered processing.
- If text is present but not submitted and the target is visibly idle with no user interaction or permission prompt, send one Enter only; never paste the message again or press Enter blindly.
- A successful `send-keys` exit code alone is TEXT_ENTERED, not SUBMITTED or ACKED. Record the state honestly.

