# x-data-mcp 全量 Node.js 改造方案

状态：设计草案，尚未实施。日期：2026-09-23。
基线：`3658ecbc5991a6e22f10c8aa169b9c571d2e3a5f`；方案分支：`docs/nodejs-full-migration-plan`。

## 1. 目标与决策

将本项目全部业务代码、MCP 服务、采集服务、CLI、测试和发布流程迁移到 Node.js，降低日常改动的构建等待时间，同时保留现有数据治理和故障恢复语义。最终运行、构建和 CI 不依赖 Rust、Cargo、Rust 二进制、Rust 原生扩展或 Rust WASM；不采用 Node 外层包装 Rust 核心的混合方案。

使用 JavaScript ES modules，Node.js 24 LTS 作为首个候选运行时；实施时在锁文件、CI 和发布清单中固定经过验证的具体补丁版本。使用 JSDoc 描述内部数据结构，边界使用运行时 schema 校验，不引入 TypeScript 编译环节。采用 npm 和提交的 package-lock.json，开发直接运行源码，生产也不要求打包转译。

迁移不扩大数据范围，不修改业务验收口径，不因更换语言将 production_ready 改为 true。当前 AGENTS.md/README 的 Rust 技术约束须在迁移实施提交中同步更新；用户已明确要求全量 Node.js，编写方案不受原技术选型约束阻塞。

本方案是完整迁移设计，不是实施完成声明。当前 #262 仍 OPEN，不能将本方案视为该 Issue 已批准新增迁移范围；实施应建立独立迁移 Epic 并关联 #262 的契约与证据。

## 2. 已知问题与证据边界

当前是一个 Cargo package，多二进制共用核心库。近期本机修改后的 release 编译约需 30–40 秒，这是会话中的局部观察，不是正式构建基准。改成 Node 可以取消业务源码的 rustc/codegen/link 等待，但不能消除依赖安装、SQLite 原生依赖准备、测试和计算本身的时间。

当前纯聚合优化的 21% 收益，以及 compact runner 的本机测量，见 [本机性能交接](issue-262-local-performance-handoff.md)。这些是 Rust 基线，不证明 Node 更快或更慢。全量迁移需要同时测量开发反馈速度、运行时间和内存，不能只对比程序启动时间。

## 3. 交付形态与模块划分

保持两个独立服务：data_service 管理本地数据和任务，acquire_service 管理官方文件获取及 generation 生命周期。MCP stdio 客户端经 mcp_connect 访问私有 Unix socket；不新增 HTTP 暴露面。

建议先使用单 npm package，按关注点划分目录，不为每个 Rust 文件创建一个 npm package：

```text
src/
  domain/         # 身份、时间、定点数、数据模型、业务错误
  parsers/        # day、DBF、行业、GBBQ、zip member
  algorithms/     # QFQ、yearly、指标计算
  governance/     # provenance、coverage、generation、来源核验
  tasks/          # 请求幂等、状态机、占用、取消、重试、核实
  application/    # collect/update/backfill/republish/query 等用例
  storage/        # SQLite schema、repository、版本、receipt、摘要
  acquire/        # 下载、manifest、verify、activate、留存策略
  mcp/            # registry、schema、协议映射、能力发现
  runtime/        # socket、租约、文件 guard、worker、退出
  cli/            # 所有服务和运维命令的装配入口
  workers/        # CPU 密集计算的 worker_threads 入口
schemas/          # MCP 与持久协议 schema/golden 样例
migrations/       # 保留并验证现有显式 SQLite migrations
scripts/          # Node 构建、检查、部署报告和驱动程序
test/             # 单元、契约、跨进程、差分及性能测试
```

| 现有能力 | Node 目标模块 | 必须保留的关键行为 |
|---|---|---|
| day_file/dbf/catalog_tdx/gbbq | parsers | 字节、编码、单位、未知字段与拒绝边界 |
| qfq/yearly/metrics | algorithms | 精确运算、舍入、缺失值、as_of、lineage |
| snapshot/file_consistency/generation | governance/acquire | 字节绑定、不可变代、核验后激活 |
| task_control/execution/verification | tasks | revision CAS、占用、幂等、未知执行保护 |
| sqlite_market/sqlite_tasks/sqlite_supply | storage | 同事务发布、历史版本、receipt、只读不建库 |
| data_supply/mcp | application/mcp | 工具发现与实际 handler/schema 一致 |
| local_runtime/verification_runtime | runtime | 单写者、同 UID、本地文件及停止证明 |
| src/bin 的所有入口 | cli | 命令、参数、退出码和可追溯构建版本 |

迁移盘点必须覆盖旧 mcp_server、mcp_sqlite_dev、mcp_data，以及 task_sqlite、inspect_day、inspect_snapshot、consistency_check、create_demo、seed_gap_subject 等入口，不能只实现最新 data_service。旧 JSON 和 schema-18 兼容能力也属于现有表面，默认保留；若要退休须另行明确变更，不能静默消失。

## 4. Node 运行与依赖设计

- SQLite 首选候选为 Node 内置 node:sqlite；先核验目标 Node 版本的 API、稳定性标记、SQLite 版本、BigInt 读取、事务、只读打开、busy/checkpoint 和 pragma 行为。若能力不足，切换为经过同样验收的成熟 Node SQLite 驱动（例如 better-sqlite3）。驱动选择在存储实施前冻结，业务 repository 不依赖驱动特有返回类型。
- crypto 负责 SHA-256，Buffer/DataView 负责二进制编解码，net 负责 Unix socket，worker_threads 负责 CPU 作业隔离。
- JSON Schema 由单一工具注册表引用，使用 Ajv 等经过兼容性验证的校验器；不启用隐式类型转换、默认值写入或悄悄删除额外字段。
- ZIP、DBF 编码及下载采用经过锁版本和边界测试的 Node 库或本项目解析器；zip 路径穿越、长度截断、解压预算和成员绑定均须保留。
- Linux flock、SO_PEERCRED、基于 fd 的文件身份及停止核验并非全部可由 Node 标准库表达。必须在 P1 验证维护中的 Node 系统调用绑定；必要时用最小 C/C++ Node-API 绑定，仅暴露内核原语，不包含业务逻辑。交付不得依赖 Rust 或 shell 子进程模拟锁。这与 SQLite 自身的原生运行库同属平台依赖，业务实现仍全部是 JavaScript。
- 原生绑定优先提供目标平台预构建产物，安装与发布清单记录版本和摘要，避免每次业务改动触发本地编译。若内核能力无法等价提供，迁移阶段判为不通过，不能退化成 PID 文件或跳过 UID 校验。

## 5. 数值、字节与身份兼容

### 5.1 精确数值

价格分、交易日和数据库整数使用范围检查；可能超出 2^53-1 的值以 BigInt 保持精确。金额、复权系数和汇总使用明确 scale 的十进制表示或整数有理数，不使用 Number 计算业务金额。

建立独立 arithmetic 模块：checked add/sub、scale 对齐、96 位 Decimal 可表示性、溢出拒绝、HALF_UP 和 HALF_EVEN 分别实现。不能只换用任意精度库就默认兼容：旧实现拒绝的精度损失仍须拒绝；年度某字段有 None 时，即使先前部分和溢出也应返回未知。对 .day 原始 f32 金额的转换必须对齐现有投影规则及测试向量，不能随意调用 toFixed。

### 5.2 序列化与摘要

保持现有 MCP wire 类型，不因为 JavaScript 不支持 JSON BigInt 就统一改成字符串；按每个契约字段采用无损解析/序列化，拒绝静默截断。内部 BigInt 不直接交给 JSON.stringify。

typed framing、字节序、长度前缀、十进制规范字符串、UTF-8、日期及 UUID/版本 ID 规则都要有固定向量。分别覆盖 compact-market-version-v1、legacy envelope digest 和 RFC 8785，不能把 JSON.stringify 当通用 canonicalizer，也不能混淆 staged 与 published 的摘要风格。

时间使用显式 UTC instant 和 Asia/Shanghai 业务日期；不使用宿主机默认时区。year_unfinished、未来日期拒绝、固定 as_of 与重放冻结事实保持不变。

## 6. 存储、任务与并发

保留现有 SQLite 结构和 schema 版本含义：schema-18 legacy 与 schema-19 compact 的后端分派、显式 init/upgrade、非空升级拒绝规则不因换语言改变。不自动迁移旧行情或重新生成历史摘要。

Node data_service 内由专用数据库执行 worker 持有写连接和串行命令队列；事务函数内部不 await 外部 I/O。CPU workers 只接收冻结输入、返回候选，不能直接写数据库。单写者仍跨进程由租约保护，不能只依靠 JS 队列。

发布事务必须同时更新版本状态、head CAS、结果、receipt、task item 和占用释放。排队前校验不能代替事务内对 revision/head/generation 的复核。SIGTERM/异常退出不能把未知提交改写成失败；先查 durable receipt，证据不足保留 needs_verification。取消为状态机动作，不等于直接杀 worker 并释放占用。

队列和 worker 数均设置预算和背压；CPU worker 传输批量紧凑数据，避免逐 bar JSON 往返。先单 worker 建基线，再按本机测试增加并发，记录主线程事件循环延迟和内存，不能仅靠 Promise.all 实现 CPU 并行。

## 7. MCP 与采集兼容

冻结原实例 tools/list、get_data_capabilities 及各 operation 的 schema/示例，覆盖初始化、通知、错误、structuredContent、isError、未知参数拒绝和断连。准确工具集按实例/平台发现，不能照搬 README 某个历史数量。

request_id 的相同请求重放、不同请求冲突，以及 cursor 的作用域、过期和分页预算保持。生成 capability 的单一注册表必须驱动 schema 与分派，禁止广告尚未实现的工具。

acquire_service 完整迁移下载、校验、生成 manifest、generation 状态转换和验证后激活；data_service 不承担网络采集。未知/冲突 generation 仍拒绝执行，不添加未经批准的来源 fallback。脚本中承担生产采集或编排业务的 Python 路径迁移为 Node；历史离线证据原样归档，不要求改写既有证据文件。

## 8. 分阶段实施与退出条件

各阶段独立提交，最终目标仍为全量 Node。Rust 只在过渡期作为隔离测试 oracle；不成为 Node 服务的运行依赖。

| 阶段 | 交付 | 退出条件 |
|---|---|---|
| P0 契约冻结 | 全量工具/CLI/schema/错误/fixture 清单；真实输入摘要；同机开发和运行基准 | 每个入口、存储格式、故障场景有对应测试与责任模块，无遗漏 |
| P1 平台验证 | Node 工程、SQLite 驱动、数值/JSON codec、Linux guard 最小验证 | 大整数/Decimal/摘要固定向量及跨 UID、锁释放、符号链接/替换拒绝通过；驱动与原生绑定选型冻结 |
| P2 算法与来源 | day/DBF/GBBQ/QFQ/catalog/yearly/metrics 全 JS 实现 | 有效和损坏输入与 oracle 等价；真实 retained 输入逐字段及摘要一致 |
| P3 存储与状态机 | legacy/compact repository、任务与核实、发布/重用 | 原子性、重复请求、CAS 冲突、损坏、磁盘错误、进程退出及 receipt 恢复测试通过 |
| P4 服务与运维 | 两个 MCP 服务、所有 CLI、驱动及部署检查 Node 化 | 真实子进程 MCP 契约、隔离 fresh DB 的完整流程通过；无 Rust 子进程调用 |
| P5 本机性能优化 | 冷启动、修改反馈、首采、append、no-change、yearly、查询、增长与内存报告 | 同机可复现对照满足冻结预算；没有为性能删减核验；背压与事件循环响应通过 |
| P6 切换与收尾 | Node 发布包、切换/回滚演练、CI/README/AGENTS 更新 | 全新库验收、故障恢复、两服务版本一致、旧服务停止后才启动新服务；最终包与 CI 无 Cargo/Rust |

## 9. 测试与性能验收

推荐 npm scripts（以下为待实现，不是当前已存在命令）：

```text
npm run check          # lint、JSDoc/静态检查和 schema 自检
npm test               # node:test 单元、集成与独立进程测试
npm run test:contract  # MCP、CLI、wire schema 与兼容固定向量
npm run test:recovery  # 进程退出、response-loss、写失败与保护状态
npm run bench          # 固定本机输入与预热规则的机器可读报告
npm run package        # 锁版本安装、产物 manifest、摘要及 smoke
```

差分覆盖有效输出、拒绝行为、receipt/lineage、固定版本读取、head 变更和重复执行。Rust 与 Node 使用不同临时库，不同时写同一生产库。P6 后通过归档 golden 向量、属性测试和进程故障测试维持验证；常规 CI 不再编译 oracle。

性能按固定本机、输入摘要、版本、并发、缓存状态和计时边界报告 p50/p95/max、wall time、RSS/heap、事件循环延迟、DB/WAL 和日增长。至少包含 100-item 和全目录规模；历史窗口长度、changed/no-change 分开。GC 与 worker 启动成本计入适当边界，不把热缓存算作冷启动。

拟议验收预算：源码修改无需编译/转译，热依赖下单模块测试反馈 p50 至少比 Rust 基线缩短 50%；同范围数据任务时间和峰值 RSS 不高于冻结基线的 1.2 倍，现有已确认业务硬门继续适用。以上是设计建议，P0 中需以测量和产品决策冻结，未冻结前不能宣称通过或以降低正确性换达标。用户优先本机软件优化，不以升级硬件作为解决措施。

## 10. 发布、切换与回滚

发布包包含 JavaScript、锁定依赖/平台绑定、受验证 Node 版本要求、SQL migrations、工具 schema、manifest、SHA256SUMS 和 smoke 报告。移植现有 Git dist-ref、包版本/SHA 绑定和回滚报告能力，避免迁移语言时丢失可追溯发布。部署脚本可保留薄 shell 启动外壳，但业务和验证驱动全部为 Node。

首次验收使用隔离 fresh schema-19 库并从已核验输入重新构建，不直接覆盖或迁移已有行情。停止旧服务、等待写入边界确认、备份数据库及 WAL 一致快照与 generation 绑定、核对包后再切换两服务。用户请求/历史版本如需跨库延续，单独设计导入和幂等边界；不能将请求不存在误解释为可重做。

回滚必须保留旧库和旧包，仅在未知执行先核实后切回。切换后 Node 新写入不能直接丢弃或无条件交给旧服务；应先隔离并按审计确认恢复方案。切换窗口内冻结新写请求，明确回滚边界。回滚备份不是 Node 最终运行依赖，验收后 Rust 工程退出主构建，历史保留在 Git/tag。

## 11. 风险、估算与下一步

| 风险 | 控制与停止条件 |
|---|---|
| 数值或 JSON 表示差异改变摘要 | 固定向量和全字段差分不一致就停止切换 |
| SQLite 驱动行为或原生 API 缺口 | P1 实证验证；不得用弱锁、PID 文件或缺失权限检查代替 |
| CPU 工作阻塞 MCP | worker 隔离、队列预算、事件循环测量；超预算回到优化阶段 |
| 历史工具/后端漏迁移 | P0 清单逐项签收；默认不删除兼容入口 |
| 隐含跨模块依赖扩大重写量 | 先迁移业务边界，再接入口；不逐文件机械翻译 |
| 编译等待消失但总测试更慢 | 同机修改到测试结果的端到端计时，不只比较启动 |
| 原生依赖安装仍慢 | 预构建平台产物与锁版本缓存，单独报告首次安装 |

按核心迁移工作量，P0/P1 小于 P2/P3，P3/P4 是主要正确性风险；P5/P6 依赖前序证据。当前没有足够吞吐数据给出可靠日历工期，不承诺数天完成重写。P1 完成后，用已移植垂直切片的实际工作量估算其余阶段。

下一步：创建迁移 Epic，完成 P0 清单；实施 P1 的三个可审查样板——精确数值与摘要、SQLite 原子发布/退出恢复、Linux 租约与 UID 核验。之后按 P2–P6 推进，不引入 Rust 混合运行路线。本次仅提交方案文档，不执行迁移、部署或数据切换。
