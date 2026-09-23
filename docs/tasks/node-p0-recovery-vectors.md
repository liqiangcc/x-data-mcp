# Node 迁移 P0 — 故障/恢复与 fail-closed 向量

任务：NM-004（agent-coordination/node-migration）。日期：2026-09-23。
基线：`9fc6a11036fbe8806b4213f98506e33ff82c7703`（`migration/node-devin`）。
关联：Epic #271、#262（OPEN，不授权迁移范围）。上游：`node-p0-inventory.md`、`node-p0-mcp-contract.md`、`node-p0-numeric-digest-vectors.md`。
证据：`evidence/node-migration/p0/recovery/`（matrix.json 11 行、live-runs.jsonl 13 条实测探针、SHA256SUMS、README）。

**状态语义**：`observed_test` = 本任务执行既有测试且全过；`observed_test+live_probe` = 另有本任务合成实机探针；`source_mapped` = 仅源码/机制映射，**不等于运行时 PASS**；`not_run` = 未执行并写明原因。

## 1. 执行摘要

- 本任务执行 10 个既有测试文件：**118 passed / 0 failed / 0 ignored**，exit 0（`TMPDIR=/dev/shm`，rustup 1.98.1，matrix.json `test_run` 记录完整命令与逐文件计数）。
- 13 条合成 live 探针在 `/dev/shm/nm004`（tmpfs）执行，全部 fail-closed 或按设计恢复；无 oracle 失配。
- 无二进制/源码失配：用于 live 探针的 target/debug 二进制构建 SHA `ea14ede`，而 `src/`+`tests/` 在 `ea14ede..9fc6a11` 间逐字节相同（`git diff --stat` 为空），即探针与基线同源。

## 2. 覆盖矩阵（详见 matrix.json）

| 行 | 类别 | 状态 | 关键证据 |
|---|---|---|---|
| task-idempotency-replay | 请求幂等/冲突 | observed_test | `request_id` PK + `command_digest` + `task_id UNIQUE` + `outcome_kind` XOR CHECK（001.sql:9-11）；6 测试过（两连接同 id 同语义、冲突整批拒绝、复合请求单任务化、动态选择不重解析、acquire 侧 replay+crash 调和） |
| revision-cas | 修订/头 CAS | observed_test | `state_revision` 列 + head `expected_previous` 绑定 + generation activate CAS；6 测试过（两连接头竞争无 last-writer-wins、取消/占用变化阻塞发布、年度头冲突、acquire 条件激活冲突+回放） |
| occupancy-claim | 占用/认领 | observed_test | `scope_occupancy` PK（001.sql:19）；7 测试过（全部 5 个 occupancy + 日期不交不缩占 + retry 占用冲突不建任务） |
| cancel-retry | 取消/重试 | observed_test | 13 测试过（cancel.rs 全部 6：pending 不调源、running 完成当前 scope 后停、needs_verification 不动；retry.rs 全部 4：幂等、整批拒绝、占用冲突、仅未完成子集） |
| exit-response-loss-needs-verification | 进程退出/响应丢失/待验证 | observed_test+live_probe | 42 测试过（stopped_verification 36 全过 + sqlite_tasks 5 + publication_verification 2）；live SIGKILL-restart 探针：stale socket+owner.json 保留、重启校验 marker 与 DB inode 后成功重绑 |
| atomic-publish-head-receipt | 原子发布/头/回执 | observed_test | 13 测试过（整批回滚、回执不别名、projection 篡改拒、composite 保留保护到最后提交、未验证输入不晋级、历史固定基发布不晋级、prepare 后基变拒、谱系篡改+伪造 published 拒、损坏态不谎报 no_data、冻结摘要独立字面量复算、compat 元数据/证据篡改拒、版本列表冻结、固定分页忽略新头） |
| generation-pin-activate-retire | 代钉住+激活/退役 TOCTOU | observed_test | 13 测试过（全生命周期、条件激活冲突+回放、pin 阻 retire、验证决策写一次+坏字节 fail-closed、畸形压缩包无证据、热容量拒、绑定会话经激活存活+阻退役、无绑定拒绝派生） |
| fs-uid-lease-socket-guards | 文件系统/UID/租约/socket | observed_test+live_probe | local_runtime 13 测试全过（symlink/未注册文件/DB 替换拒、panic 停服、shutdown 预算退出保留 running+占用、断连存活、帧限只断会话）；13 条 live 探针全 fail-closed 或按设计恢复 |
| peer-uid-authorize-negative | SO_PEERCRED 跨 UID | not_run | Box 无第二 UID 可起外来客户端；机制 source_mapped 于 `Endpoint::authorize` |
| mid-write-crash-with-wal | 写中崩溃 WAL 恢复 | source_mapped | 提交边界语义已被 needs_verification 测试覆盖；字节级撕裂写演示未执行 |
| non-linux-surface | 非 Linux | not_run | Box 仅 Linux；cfg 差异未执行 |

## 3. Live 探针观察值（live-runs.jsonl）

全部 exit 1（fail-closed），除 SIGKILL-restart 为按设计恢复：

| 探针 | 观察输出 |
|---|---|
| socket 父目录 0755 | `runtime path must be owned by this UID, private, and not a symlink` |
| socket 父目录 symlink | 同上 |
| DB 路径 symlink | 同上 |
| DB 在 overlayfs | `service requires a supported local filesystem` |
| 同 DB 第二实例 | `database service already active or lock unavailable: lock acquisition failed because the operation would block`（非阻塞 try_lock） |
| owner.json 伪造字段 | `unknown field 'database_identity', expected 'device' or 'inode'`（标记反序列化 fail-closed） |
| 垃圾字节 DB（mcp_data/task_sqlite upgrade） | `STORAGE_UNAVAILABLE: file is not a database` |
| 空文件 DB | `STORAGE_UNAVAILABLE: connection pragmas differ from task contract` |
| 600 普通文件作 endpoint（connect） | `endpoint is not a socket` |
| bind 路径存在普通文件 | `unregistered existing endpoint; no clobber` |
| 裸相对 socket 路径 | `socket requires an explicit private parent directory` |
| SIGKILL 后同 DB+socket 重启 | stale socket+owner.json 保留；owner marker 匹配 DB inode → 重绑成功（exit 0） |

## 4. P3/P4 阻塞缺口（Node 等价物清单，详 matrix.json `gaps_blocking_p3_p4`）

请求幂等+冲突整批原子性；`state_revision`/`expected_previous` 两级 CAS；`scope_occupancy` 持久化与全量释放；needs_verification+stopped-runner 回执绑定（36 测试面）；发布原子性+回执不可别名+未验证不晋级；代 pin/activate/retire CAS；Linux 守卫（private_metadata、fstatfs 白名单、flock 租约、owner-marker no-clobber、SO_PEERCRED）的 Node-API 等价物。

## 5. 验证记录

| 命令 | 退出码 | 结果 |
|---|---|---|
| `cargo test --locked --test <10 files>`（TMPDIR=/dev/shm） | 0 | 118/0/0 |
| 13 条 live 探针（data_service/mcp_connect/mcp_data/task_sqlite） | 各见 live-runs.jsonl | 全部 fail-closed 或按设计恢复 |
| `git diff --stat ea14ede..9fc6a11 -- src/ tests/` | 0 | 空 — 探针二进制与基线同源 |
| `json.tool matrix.json`、`json.loads` live-runs、`sha256sum -c` | 0 | 全合法 |
| `git diff --check` | 0 | 干净 |

环境事件：执行开始时 overlay 0 可用（旧 target 被外部清理后回升至 63G），全部测试经 cargo 正常编译运行；oracle/探针临时文件均在 /dev/shm，未留盘。

## 6. 范围声明

仅新增 `docs/tasks/node-p0-recovery-vectors.md` 与 `evidence/node-migration/p0/recovery/`；未改运行时源码、既有测试、迁移、AGENTS/README、会话文件；无 push/merge/deploy/ECS/官方输入/真实 DB。性能预算（50%/1.2x）仍未冻结。
