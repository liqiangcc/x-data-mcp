# #78 阶段 A 实施记录

派发：`x-data-mcp-78-d2-20260912T085420Z`，执行者既有 d2。
代码 base：`1e270c648e7884fffd65ef13cfe9f47e0dab22ad`。
工作树：`/root/x-data-mcp-issue78`；分支 `issue-78-sqlite-tasks`。
整体设计：`2002518cad08493722473e2132d3e7f4a86518da`；SQLite：`6f174e5caa4d5db110b6d101f7ed20ee8b004d12`。
准入与AC以#78评论5644867377及现行正文为准。已核对父#3/#5、#69/#75和开放#72/#76；没有发现阻塞A的新业务冲突。旧Goal不恢复，两个设计PR不合并。

## 编码前最小方案

仅新增通用任务领域/应用契约、SQLite Adapter、legacy任务协议Adapter及显式开发二进制。默认mcp_server仍用现有JSON装配；不迁移、不双写、不做行情事务。本阶段不注册新的dataset或管理工具。

最小DDL（最终约束及迁移脚本随实现提交）：

```sql
schema_migrations(version INTEGER PRIMARY KEY, checksum TEXT NOT NULL, applied_at TEXT NOT NULL);
requests(request_id TEXT PRIMARY KEY, command_json TEXT NOT NULL, command_digest TEXT NOT NULL,
         outcome_kind TEXT NOT NULL, task_id TEXT UNIQUE REFERENCES tasks(task_id), terminal_json TEXT);
tasks(task_id TEXT PRIMARY KEY, command_key_json TEXT NOT NULL, status TEXT NOT NULL,
      state_revision INTEGER NOT NULL, accepted_run_id TEXT NOT NULL, owner_run_id TEXT,
      created_at TEXT NOT NULL, last_progress_at TEXT NOT NULL, frozen_work_json TEXT NOT NULL,
      result_json TEXT, retry_of TEXT REFERENCES tasks(task_id));
task_items(task_id TEXT REFERENCES tasks(task_id), item_id TEXT, target_json TEXT NOT NULL,
           progress_unit TEXT NOT NULL, frozen_inputs_json TEXT NOT NULL, scope_keys_json TEXT NOT NULL, stage TEXT NOT NULL,
           outcome_json TEXT, PRIMARY KEY(task_id,item_id));
task_scopes(task_id TEXT REFERENCES tasks(task_id), scope_key TEXT, PRIMARY KEY(task_id,scope_key));
scope_occupancy(scope_key TEXT PRIMARY KEY, task_id TEXT REFERENCES tasks(task_id), state_revision INTEGER NOT NULL);
```

TaskRecord本身即执行记录；不重复建与其同义的execution表。缺执行证据测试包括request存在而task缺失、items缺失或非法状态。frozen_work保留首次解析结果，读时对照不可变items/scopes，不能从残缺行猜补范围。

连接仅在SQLite Adapter持有。普通读取用READ_ONLY且单个读事务，不建库/升级/回退；写入口只打开既有库。独立init/upgrade命令管理schema_migrations和PRAGMA user_version，未知版本/校验和拒绝。WAL、foreign_keys=ON、synchronous=FULL，busy预算默认250ms，明确不是性能SLA。

内部CommandKey(operation,dataset,contract_version)、Command(完整规范化意图)、FrozenWork、TaskRecord/TaskItem/ScopeClaim、RequestRecord、TaskSnapshot。契约处理器负责默认值和集合语义；SQLite不猜测数组含义。完整命令比较是依据，hash仅辅助。动态解析在新请求确认未绑定后进行，事务再次检查绑定；竞争时已有绑定优先，不能覆盖其冻结范围。未知历史契约拒绝使用最新处理器。legacy day/year保持一个复合任务。

原子accept使用BEGIN IMMEDIATE：检查原绑定/完整性、重试资格与修订、全部范围冲突，再一次提交request/task/items/scopes/occupancy。不消费普通拒绝的request_id。无任务RequestTerminal只保留内部表达，不新增核实业务入口。

状态决策与SQL分离。claim、进度、取消、结束都带expected_revision，事务更新task与全部claim修订。accepted_run_id在受理时固定，owner_run_id只在领取时设定。正常当前running保留running；旧run任一非终态只读呈现needs_verification并禁止领取；显式生命周期reconcile可持久标记，保留原进度与占用。普通读不调用reconcile。

取消仅申请cancel_requested（包括pending），保留保护；同步执行器确认未启动/所有已开始动作返回后才收尾释放，不能由wire传入“已停止”布尔值。已终态/重复取消返回实际状态不修改修订；过期条件拒绝。重试先重放其全局绑定，未绑定才事务检查原任务/选择项资格和占用，新任务关联原任务，原任务不回写；重试复用返回真实终态。

显式开发stdio复用旧五个任务工具参数/返回形状，并提供受控run_worker_once（仅同步拒绝来源的执行器）；默认八工具入口保持。测试专用非证券契约仅在测试注册。未知字段拒绝。task query不读JSON行情；本阶段无跨存储发布承诺。

## 错误与wire映射

| 内部分类 | legacy wire |
|---|---|
| InvalidCommand | invalid_arguments；既有empty_request_id/empty_symbols/empty_periods/invalid_period在legacy解码保持 |
| RequestConflict / RangeConflict | request_conflict / range_conflict，后者返回legacy scopes |
| TaskNotFound / RetrySelectionInvalid | task_not_found / invalid_retry_selection |
| AcceptanceNeedsVerification | get_request为request_needs_verification，其余storage_error并保留原因 |
| StorageUnavailable（未初始化、schema、坏库、busy） | storage_error |
| OutcomeUnknown（提交不能确认） | storage_error，message明确OUTCOME_UNKNOWN并要求原request_id核对 |
| RevisionConflict / UnsupportedContract | storage_error，message保留STATE_REVISION_CONFLICT / UNSUPPORTED_CONTRACT |

## AC → 测试矩阵（编码前计划，尚未PASS）

- AC-01：真实临时库两个连接同标识同/异参、范围竞争、批量部分冲突、不同日期同证券周期冲突；提交前子进程退出无半绑定，提交后退出再查复用。
- AC-02：集合调序/重复、固定默认、动态冻结且重放不调用resolver、操作/dataset/参数变化、hash相同但完整语义不同、旧复合请求、未知历史契约。
- AC-03：单事务读取与并发提交、合法running、终态无占用、缺task/items/occupancy、未初始化/坏库/未知schema/busy、读前后业务表一致。
- AC-04：test-only非证券契约沿用受理/进度/取消/重试；公共模型不访问symbol/period；不在生产tools/list注册。
- AC-05：取消/领取/自然结束竞争、过期revision、重复取消、重试重放真实状态、旧pending/running/cancel_requested重启拒绝执行和保留保护。
- AC-06：迁移原子性与checksum、实际开发stdio及原八工具离线兼容；fmt/clippy/test/diff和CI。真实来源/迁移/部署NOT_RUN；进程退出不是断电证据。

## 本轮收敛交付与依赖

按用户最新指令，先让 Draft #82 CI 全绿，然后停止扩展实现，交 Coordinator 审查；此记录不宣称全部 AC 已审结。
代码提交 `ecf717fd565b032c5fbb4f12409c3e15aecc6417` 的
[Actions run 34685892720 / rust job 103532701652](https://github.com/liqiangcc/x-data-mcp/actions/runs/34685892720/job/103532701652)
已 PASS：fmt、clippy all-targets/all-features/locked/-D warnings、test locked、diff-check、现有 build locked。
后续文档提交的准确 HEAD/CI 在 PR #82 与 #78 交付评论固定，不用本段旧 SHA 冒充新 HEAD 验证。

新增 `rusqlite 0.40.2`，关闭默认特性，仅启用 bundled；锁定 `libsqlite3-sys 0.38.2`。
依据是需要真实 SQLite 的事务、只读 open flags、busy timeout 和显式 PRAGMA；bundled 避免依赖宿主机 SQLite 版本。
参考 [rusqlite 官方库文档](https://docs.rs/rusqlite/0.40.2/rusqlite/)、[SQLite 事务](https://www.sqlite.org/lang_transaction.html)、[PRAGMA](https://www.sqlite.org/pragma.html)。
Cargo.lock 只新增此依赖闭包，未更新已有包。bundled 的本机编译成本由 Actions 承担，未清理共享 target/cache。

### AC → 实际自动测试 → 结果

以下具体测试均在上述同一 Actions run/job 的 `cargo test --locked` 日志中为 `ok`。
`tests/sqlite_tasks.rs` 共 12 个测试 PASS；库内 2 个实质故障/快照测试 PASS。
`crash_child` 是故障子进程入口，普通测试运行直接返回，不单独算一次故障验证。

| AC | 具体测试名 | 本轮已验证行为 |
|---|---|---|
| AC-01 | `two_connections_serialize_same_id_same_semantics` | 两线程各自打开真实连接，同 ID 等价请求只提交一次 |
| AC-01 | `two_connections_conflicting_id_or_scope_never_partially_accept` | 同 ID 异参、不同 ID 同范围竞争，一成功一拒绝 |
| AC-01 | `partial_batch_conflict_rolls_back_all_and_unrelated_scope_is_available` | 部分范围冲突整批不绑定，无关范围可受理 |
| AC-01/06 | `sqlite_tasks::tests::process_exit_before_commit_rolls_back_and_after_commit_replays_same_task` | 实际 accept 提交前子进程 exit(78) 后五表无半绑定；提交后 exit 丢失响应，原 ID 重放同任务 |
| AC-02 | `normalized_compound_request_is_one_task_and_complete_intent_controls_replay` | 集合调序/去重、固定 qfq 默认、复合 day/year 一个任务；操作/dataset/版本/参数变化冲突 |
| AC-02/04 | `non_security_contract_shares_control_and_replay_never_resolves_dynamic_selection_again` | 测试专用 documents 契约共用受理、进度、取消和重试；动态 all 重放不重解析，显式选择不同于 all；未知旧契约拒绝 |
| AC-03 | `sqlite_tasks::tests::read_uses_one_snapshot_when_another_connection_commits_between_queries` | 读到 task 行后另一真实连接提交新修订，当前读取的 task/items/claims 仍同一旧快照 |
| AC-03/05 | `current_running_terminal_and_read_only_state_are_distinct_from_missing_evidence` | 当前 running、正常终态无占用、重复读取不改状态、过期修订拒绝、执行尚未结束不得释放 |
| AC-03 | `residual_records_fail_closed_without_read_repair` | 缺 task/items/scopes/占用/绑定、摘要损坏、伪终态拒绝；不通过读取补写 |
| AC-03/06 | `absent_bad_unknown_schema_and_busy_do_not_initialize_or_fall_back` | 不存在/坏库/未知 schema 明确失败；真实写锁 busy 预算耗尽无新绑定，不回退 JSON |
| AC-05 | `every_old_nonterminal_run_is_visible_but_not_executable_or_released` | 旧 pending/running/cancel_requested 不执行，普通读取不修复；显式 reconcile 保留进度与保护 |
| AC-05 | `cancel_is_request_until_controlled_stop_and_retry_preserves_original` | 取消请求保留占用、确认未启动后收尾、重复取消无改写、重试复用返回实际 failed、原任务不回写 |
| AC-05 | `completion_during_cancellation_uses_actual_outcomes_and_keeps_unfinished_items` | 同步执行器内另一连接取消，返回后按实际参与结果收尾；已成功项不得重试 |
| AC-06 | `explicit_schema_upgrade_is_transactional_and_checks_migration_identity` | v1→v2 显式升级、失败回滚、不覆盖已有文件、checksum 拒绝 |
| AC-06 | `stdio_initialize_then_tools_list_exposes_exactly_eight_tools`、`production_stdio_fails_closed_without_python_and_does_not_publish`、`actual_stdio_queries_are_readonly_and_unknown_arguments_reject` | 原 mcp_server 实际离线 stdio 回归 PASS，默认八工具和来源门禁保持 |

### 剩余未验证边界（第一轮返修后）

- 新二进制与 retry wire 的首轮缺口已增加真实 CLI/stdio 测试，日期冲突已增加测试契约；具体入口见下文返修矩阵。此前 NOT_RUN 结论属于 `8d9b3b0`，不代表返修后的测试范围。
- 辅助摘要碰撞而完整命令不同、重复项带冲突参数等更细场景尚未单独测试：NOT_RUN；没有注册新 dataset/date wire。
- 已测试取消发生在同步执行器内部；所有 CAS 窗口、多个业务 worker 的并行执行和长时间压力未穷举：NOT_VERIFIED。继续保持单写 worker 运行边界。
- 进程 exit(78) 不是断电/硬盘故障耐久性证明。真实断电、提交 I/O 错误导致 OUTCOME_UNKNOWN 的注入、性能阈值：NOT_RUN/NOT_VERIFIED。
- 本机曾有一次 cargo check 因修订号 u64/SQLite INTEGER 类型转换失败；已显式转换，后续 Actions clippy/test PASS。本机完整验收未执行；没有以本机磁盘限制降低 Actions 命令要求。
- 来源采集、真实行情迁移、部署、SQLite 同库行情发布：NOT_RUN。#46/#54、授权与跨文件一致性门禁未解除；#79/#80/#81 没有开始。

PR #82 保持 Draft；CI PASS 只证明上述测试场景，不是 production approval、全部 AC 验收或设计 PR #72/#76 整体审结。

## Coordinator 第一轮返修（仅 #78）

依据 [review 5645059361](https://github.com/liqiangcc/x-data-mcp/pull/82#issuecomment-5645059361)。
首组返修 `e9f42109ce5bc945927de80bff59137123a5c55d` 已在
[run 34686454218 / job 103534158322](https://github.com/liqiangcc/x-data-mcp/actions/runs/34686454218/job/103534158322) 全绿。
补充代码 `9c3412021f25ad047dfdc961fe000cee096d99f8` 增加已成功项和空新请求 ID 的对照。
最终文档 HEAD、run/job 及逐项实际结果固定在 PR #82 / #78 返修交付评论，不能把首组 run 当成补充代码的验证。

| 审查项 / AC | 自动测试入口 | 验证内容 |
|---|---|---|
| retry wire / AC-05/06 | `real_legacy_and_sqlite_stdio_retry_errors_have_identical_code_scopes_and_shape` | 两个真实 stdio 子进程比较完整结构化错误，并各自核对 MCP text/structuredContent/isError 外壳；原 ID（含空选择）、空选择、空新 ID、范围外/重复/混合选择 |
| retry wire / AC-05/06 | `completed_scope_retry_errors_match_real_stdio_with_persisted_test_outcomes` | 持久化有界成功/失败 fixture，再由旧/新真实 stdio 拒绝已成功项；code/scopes/shape 相等。fixture 不证明真实行情或发布 |
| CLI / AC-03/06 | `sqlite_cli_init_upgrade_no_clobber_and_stdio_read_never_initializes_or_upgrades` | 实际 init/no-clobber、v1→v2 upgrade、未知 schema/非法操作拒绝；真实 stdio 普通读取不建库、不升级 |
| stdio / AC-05/06 | `sqlite_stdio_six_tools_and_complete_task_control_flow` | initialize、六工具集合、start/get/cancel/retry/run/get_request、真实终态重放、关键错误及六工具 extra fields 拒绝 |
| 日期范围 / AC-01 | `disjoint_dates_do_not_narrow_security_period_occupancy` | test-only contract 的 2024/2025 两命令持有相同证券×周期 scope_key，第二次 RangeConflict，无新绑定/占用 |
| 关注点分离 | 既有 `tests/day_file.rs` 及上述全部 SQLite 测试 | SHA-256 实现移到 `src/hash.rs`；`day_file::sha256` 保留 re-export 兼容，SQLite 使用 `hash::sha256`，不依赖 day-file 领域模块 |

retry 的普通验证沿用旧 Adapter 的 period→原任务→原 ID→空选择→非法选择→受理顺序；
公共事务仍再次检查重试资格与占用。没有修改旧协议，也没有改变默认生产装配。
全部新测试由现有 `cargo test --locked` 发现，无需新 workflow 或本地 bundled 构建。
PR 继续 Draft，等待 Coordinator 二次审查；断电、全部 CAS 窗口、多 worker、性能和真实来源边界保持未验证。

## Coordinator 第二轮：retry 决策归属修正

依据 [review 5645168509](https://github.com/liqiangcc/x-data-mcp/pull/82#issuecomment-5645168509)，
将原任务/绑定读取、原请求 ID 冲突、空选择、越界/已成功项判断和空新 ID 校验统一移到 `TaskControl::retry`，保留原可观察顺序。
应用层返回 `RetryRequestIdConflict`、`EmptyRetrySelection`、`EmptyRequestId`、
`RetrySelectionInvalid { items }`；MCP 只解码 period/参数形状，将 item 身份和 typed 错误映射为旧 code/scopes，不读取 outcomes 或重算资格。
SQLite 原子受理中的修订/资格复核、事务与状态模型不变。

新增 `application_retry_owns_legacy_precedence_and_typed_ineligible_items_without_mcp`：
直接调用应用层，在真实临时 SQLite 上验证校验优先级、越界/已成功项的类型化结果及拒绝不新增任务。
既有两项真实 old/new stdio parity 测试保持，验证应用层返回经协议映射后仍兼容。
准确 HEAD/run/job 与实际结果在本轮 PR/#78 交付评论固定；此前绿灯不替代本轮验证。
