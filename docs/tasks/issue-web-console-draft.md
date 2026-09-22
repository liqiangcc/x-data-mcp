# 只读运维观测 Console（Web 适配器）

> Issue: [#268](https://github.com/liqiangcc/x-data-mcp/issues/268)
> 状态：草稿，待冻结。依赖：#259 S5G/S6 全部硬门通过后启动；`production_ready` 仍为 false。

## 定位

Web = 只读的"治理仪表盘"；MCP = 唯一操作通道；SQLite = 唯一事实源。
人的巡检与审计复查需要一个页面形态——AI 操作者经 MCP 执行 workflow，
人在这里看结果、查异常、对账证据。**全部能力都是查询**。

## 架构契约（冻结边界）

1. **Web 是 adapter，与 MCP 同层**。只允许调用既有读口
   （`SqliteSupply` 的 query/versions/task_facts/verify 投影、
   `SqliteMetrics`）；不得引入裸 SQL、自由查询界面或独立读路径。
2. **零写能力**。无发起/取消/重试/发布入口；可提供"复制为 MCP 调用
   参数"的便利输出，副作用只能经 MCP 契约发生。
3. **零额外运行时**。`data_service` 内嵌只读 HTTP 端点 + 静态资源
   （vanilla JS/htmx 级别）；读连接复用 WriteCoordinator 有界 reader
   池；ECS 2 vCPU / ~2GiB 上不引入独立前端服务。
4. **单操作员、最简认证**。内网绑定 + 静态 token（冻结时定稿细节）。
5. **无实时性承诺**。轮询刷新；日更粒度为天。
6. **读路径语义不变**。发现读口缺失先补 core 读口再接线；
   认证未配置或 token 错误时 fail-closed。
6a. **UI 形态参考 `liqiangcc/acp-web-console`**（同使用场景：
    iPhone Safari 上的单人巡检）。继承其已验证的移动端决策：
    布局走 native scroll + `scroll-snap` 抽屉（手写跟手不可行，
    合成器线程原生处理才不卡）；主题用语义化 CSS 变量 +
    `{codex,chatgpt,light}` 结构；大列表必须窗口化渲染
    （回放风暴教训：先收集再渲一次，不要逐行触发重排）；
    大负载走 gzip HTTP；传输用 HTTP 轮询/长轮询而非 WS
    （其 de48112 已收敛到该形态）。iOS 已知坑直接继承：
    http 非 localhost 无 SW 缓存（network-first 策略）、
    拖动期间关 backdrop-filter、tap/haptic 在 gesture context
    内触发。**仅借用 UI/传输形态**——Rust 内嵌服务不变，
    不引入 Node 运行时。
7. **统计分析只覆盖治理元数据**与既有 `compute_metrics` 结果展示；
   不提供 ad-hoc 全市场行情扫描（存储为 per-entity scope 设计，
   无全局 trade_day 索引，横切为 O(29M bars) 全扫——非本平台职责，
   由离线导出或下游系统承担）。
8. **观测数据的记录口径**：不新增进程外采样器。耗时类指标经既有
   durable 通道落库——runner 把相位耗时写入 `ItemOutcome.evidence`
   （该字段本就是 executor 产出的无决策权 durable 证据，零 schema
   变更）；空间趋势（DB/WAL、B/item 增量成本）允许一张 append-only
   采样表，随日更任务事务落库——它本身是治理数据，属唯一例外。

## 能力清单

### A. 日更巡检（P0，首发范围）

| 能力 | 回答的问题 | 读口/数据源 |
|---|---|---|
| 任务面板 | 当日任务跑了没/跑完没/卡在哪个 item | `list_tasks`、`get_task_progress`（tasks/task_items） |
| 失败队列 | 失败 item + 原因分类（source violation / integrity / storage / conflict），与 `get_task` 输出逐字一致 | item outcome + results typed 列 |
| coverage/gap 看板 | 实体覆盖区间、缺口列表、head 最新日期 | `get_data_availability`、`data_versions` 元数据、gap 决策表 |
| 存储健康 | DB/WAL 大小与趋势、reader 占用、checkpoint 状态、B/bar | `SqliteMetrics` + dbstat（复用 capacity measure 查询） |

### B. 治理统计（P0/P1——全部元数据级聚合，无 bar 扫描）

只展示系统已产生的测量值。标注口径：**直读** = 现有表/dstat 直接
可查；**evidence 落库** = runner 写入 `ItemOutcome.evidence`（既有
字段，零 schema 变更）；**采样表** = 契约第 8 条的 append-only 例外。

| 能力 | 回答的问题 | 数据源/成本 |
|---|---|---|
| 日更成功率与失败原因分布 | 按日/按周成功率趋势、失败分类占比（source violation 率漂移 = 上游格式变化的先行指标） | `task_items.outcome` 聚合，O(items/天) |
| 覆盖统计 | 全市场实体按 last_day 分布、缺口总数、覆盖率 | `data_versions` 元数据，O(N) 行级 |
| 版本链健康 | delta 深度分布直方图、rollover 计数、逼近 256 上限的实体预警 | `data_versions.delta_depth` 聚合 |
| 连续性审计统计 | unchanged/append/full 比例、异常 full-rewrite 频率 | `market_update_results.continuity_mode` 聚合 |
| 来源一致性 | generation 绑定的实体数、审计 outcome 分布 | results/source_bindings 聚合 |

#### B.1 空间占用（#259 容量门的日常监控版）

| 指标 | 读取方式 | 价值 |
|---|---|---|
| DB / WAL 当前大小 | **直读**：文件 stat / CheckpointMetrics | ≤2.5GiB 容量预算的实时对照 |
| B/bar 实测 | **直读**：dbstat（capacity measure 同款） | 压缩率漂移预警（S5B 基线 48.2B/bar） |
| segment 利用率 | **直读**：`segment_bars` pgsize/unused | 碎片检测 |
| DB/WAL 趋势 | **采样表**：每日任务事务内落一行 | 增长曲线 |
| 每类写入增量成本 | **采样表**：日更后 DB 增量 ÷ item 数 | #259 验收指标（1,721/2,426 B/item）每日复核 |

#### B.2 耗时（日更 SLA 仪表）

| 指标 | 读取方式 | 价值 |
|---|---|---|
| 任务 wall time | **直读**：`tasks.created_at`/`last_progress_at` | 日更 ≤5min 门的日常监控 |
| per-item 耗时 | **直读**：`task_item_execution_starts.started_at` 相邻差分（item finish 时刻经 evidence 补强） | 慢 item 定位 |
| 相位耗时分布 | **evidence 落库**：runner 写 `ItemOutcome.evidence`（parse/stage/commit/fsync 分项） | 性能回归定位：变慢直接看哪段退化 |
| 失败/重试率趋势 | **直读**：`task_items.outcome_json` 聚合 | 数据质量漂移 |
| reader/writer 等待时间 | WriteCoordinator 内部计数（需补暴露口，低优先） | 读写竞争监控 |

### C. 溯源与审计（P1——本产品差异化面）

| 能力 | 回答的问题 | 读口 |
|---|---|---|
| 版本浏览器 | 版本链拓扑（checkpoint/delta）、深度、状态、digests | `list_data_versions` + parent 链 |
| lineage 视图 | 版本 DAG；年线基座绑定（base/algorithm/as_of_day） | `yearly_lineage` + `data_versions` |
| 来源对账 | 版本 ↔ generation ↔ manifest/member sha256 ↔ 证据 canonical JSON | `source_bindings` + `evidence_bundles` |
| 更新审计时间线 | 逐日 continuity_mode、prefix_bytes、delta_digest | `market_update_results` |
| 验证结果 | verify 结论、被检 digest、fail-closed 原因 | `verify_versions`/audit 投影 |

### UI 规范（跨层，贯穿 A–D）

- 布局：iPhone Safari 竖屏优先；抽屉式导航（左列表右详情或
  snap 三栏），`overflow-x:auto` + `scroll-snap-type:x mandatory`，
  JS 只驱动遮罩/class，不做跟手位移
- 渲染：行列表窗口化（版本链/审计时间线可上千行）；批量数据
  先收集再单次渲染；逐行 DOM 追加禁止出现在热路径
- 传输：HTTP 轮询（默认 15s，页面可见时才轮询）；大负载 gzip
- 主题：语义化 CSS 变量，默认深色（codex 风格起步）
- PWA：图标/manifest 可留；http 场景 SW 不注册是预期行为，
  不为 PWA 缓存引入复杂度

### D. 数据浏览（P2——最后做，先验证请求模式）

| 能力 | 约束 |
|---|---|
| K 线浏览（单实体区间） | `query_data` 窗口读；强制日期范围+分页；无导出 |
| 日线↔年线对照 | 同实体双 period 并排 |
| 指标展示 | 仅展示 `compute_metrics` 已算结果，不发起新计算 |

## 明确非目标

- 写操作入口（发起/取消/重试/发布）
- 裸 SQL / 自由查询界面 / 全量导出
- ad-hoc 全市场行情横切统计（行情级分析由下游承担）
- 实时推送、多租户、账号体系、外网访问、移动端

## 验收（AC）

1. A+B 全部上线，全部由既有读口/元数据聚合驱动；review 确认
   无 adapter 层 SQL 逃逸与写路径。
2. 页面响应 ECS 上 <1s（单实体类）；读并发不挤占 writer。
3. 日更执行期间 web 读取不造成 WAL 增长失控或写延迟回归
   （对照 #259 基线）。
4. 失败队列分类与 MCP `get_task` 输出逐字一致（adapter 不加工语义）。
5. 认证 fail-closed：未配置/错误 token → 拒绝，不降级开放。
6. `docs/` 新增运维手册：页面清单、读口映射、认证姿态。

## 排期

- 阻塞于：#259 全部硬门通过 + ECS 终验完成（S6）
- 建议拆分：A+B（巡检+治理统计）为一个 Issue；C（溯源审计）、D（数据
  浏览）后续 Issue。本稿冻结 A+B 范围与全部架构契约，C/D 仅记录意向。
