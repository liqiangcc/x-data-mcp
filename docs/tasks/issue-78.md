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
           progress_unit TEXT NOT NULL, frozen_inputs_json TEXT NOT NULL, stage TEXT NOT NULL,
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
