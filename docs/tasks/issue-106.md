# 异常核实任务有界处置（#106）

权威：#3 异常核实任务独立处置/重复与状态保护/有界执行与持久化/检查结果绑定规则；#75 固定设计 2c706325aa2020382631355f802403c05b9662d8，SQLite c2f89085850423734193f3090dd435c34f4688ec。实现 base a2bdb5acf70c03437ff0928aad833558b9cc6671。整体目标继续包括其他开发、部署及最终验收。

## 运行期证明协议

采用 `verification-file-guard-v1`，面向同一 Linux 内核、可信本地 SQLite 及同目录 guard 文件，不泛化到远程文件系统/分布式执行。使用 [Rust File::try_lock](https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock) 的非阻塞独占锁；该 API 从 Rust 1.89 提供，当前项目使用 Actions stable，不因此宣称整仓 MSRV。无新增依赖。

1. 执行核实前取得每任务独立 guard；Descriptor 绑定 task/run、规范化数据库路径及 device/inode、guard 路径及 device/inode、Linux boot_id、协议版本。guard 不克隆、不显式提前解锁，RAII 持有期覆盖核实的读证据、提交/取消/异常处理全部路径。
2. guard 描述先写入并同步独立新文件，再在主 SQLite 中把 Descriptor/digest 与 Claim 状态转换原子登记；领取失败不留下半登记。协议只证明真正通过此入口执行的任务，不能给既有未登记历史执行补写证明。
3. 检查端只打开登记的既有 guard，一次 try_lock；不创建缺失证明文件、不等待进程。匹配文件句柄与路径身份、数据库身份、内核启动身份及有界 descriptor 字节。Busy 不是已停止；路径替换、缺文件、未知协议/内核或坏描述均不能作为停止证据。
4. 取得 guard 只建立协议内执行已离开受保护区的事实。还须在 SQLite 一致快照核对原核实实际收尾结果；证据不足不释放。检查持有 guard 直到条件收尾提交完成；状态/审计变化拒绝旧 Plan。纯 CAS、PID 缺失、超时或旧 run_id 均不单独证明停止。
5. 从未领取且完整 revision0/no-owner/no-effects 的核实任务可使用已有同类的“从未开始 + 条件排除未来领取”证明；正常当前运行不被强制中断。已有合法完成结果只重读，不改为中断。核实的原业务目标始终不写入/释放。
6. 不自动删除 guard/审计历史；清理策略不在本任务实施。普通查询不初始化运行文件或迁移库。内核重启、文件复制/替换及未登记协议的旧执行保持 NOT_VERIFIED，不能倒填停止事实。

## 最小 DDL 与内部类型（实施前约定）

计划显式 schema v9（只增加本任务所需表，实施时固定 migration 字节）：

- `verification_runtime_guards(task_id TEXT PRIMARY KEY REFERENCES tasks, claim_revision INTEGER NOT NULL, descriptor_json TEXT NOT NULL, descriptor_sha256 TEXT NOT NULL)` STRICT；只有持有真实 guard 的 Adapter 能随 Claim 插入，不改普通历史 Claim。
- `verification_inspections(request_id TEXT PRIMARY KEY REFERENCES requests, target_task_id TEXT NOT NULL REFERENCES tasks, audit_json TEXT NOT NULL, audit_sha256 TEXT NOT NULL)` STRICT；请求级 terminal outcome、审计与必要核实收尾在同一短事务提交，无递归 task。

`GuardDescriptor` 是持久事实；`ExecutionGuard` 是不可克隆的实际锁句柄；仅序列化 descriptor 不构成停止许可。应用 `InspectionPlan` 封装已验证快照、证据及计划结果；Adapter 执行条件提交，不决定是否应中断。

结果区分 `already_finished`、`interrupted`、`insufficient_evidence`、`not_applicable`，包含本次 request/target/revision、actor/time、原因与依据。每次可靠检查（包括不足）绑定并消费 request_id；同 ID 重放历史结果，不重新检查；新 ID 才基于新证据检查。前置输入/权限/不存在拒绝不绑定。错误分别映射 invalid_arguments/forbidden/task_not_found/request_conflict/precondition_conflict/needs_verification/storage_error/outcome_unknown；不以请求 terminal 冒充目标已终态。

## AC→测试矩阵

- AC1：真实 stdio tools/registry/schema、无递归 task、额外 stopped/force/provider/clock 拒绝、owner 信息保护、正常任务不适用。
- AC2：真实 SQLite 两连接同/异 request 竞争；不足结果消耗 ID 且固定重放，新 ID 可再次检查；已完成结果不重写。
- AC3：真实 File 锁与两个进程；活跃执行持锁拒绝、正常退出/实际进程退出后取得锁、锁文件替换/删除/其他 DB/boot 身份拒绝；SQLite 原子 Claim 登记关联。底层锁测试不能冒充整个安全收尾已完成。
- AC4：只收尾核实自身，原业务任务/占用/market/head 字节保持；旧执行器过期提交失败；合法完成不回滚。未登记历史执行保持保护。
- AC5：真实提交前退出回滚/提交后响应丢失重查；目标/证据变化、坏关联/审计、未知 schema/busy；检查历史结果与目标当前状态分开。
- AC6：Actions fmt/clippy --all-targets --all-features --locked -- -D warnings/test --locked/build --locked/diff-check；实际 stdio/隔离 synthetic package smoke。服务器本机仅静态编辑和轻量检查，稀疏 checkout 不冒充完整验证。

## 当前切片和未验证项

先交付底层 guard/跨进程证明试验，再接入原子领取登记、有界检查/条件收尾、MCP 及审查。本文记录设计，不宣称任一未执行测试 PASS 或功能已完整交付。线上仍 preview.4/schema7；不因本任务自动升级。

真实来源/#53 四股/新版 ChatGPT 消费侧 NOT_RUN；#54/#6/#46/#71/文件一致性仍 NOT_VERIFIED，NOT_PRODUCTION_READY。没有新的数据源、真实采集或迁移，不更改历史 evidence。
