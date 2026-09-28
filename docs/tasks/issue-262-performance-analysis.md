# Issue #262 性能分析

## 当前裁定（2026-09-28）

本文 preview.23/24 的统一门禁分析为历史记录，下文实测数字保持原样。owner 已用
class-specific 合同取代旧统一门禁：同代 no-change full-market `update_data`
hard ≤15min、checkpoint 后 net growth hard ≤4KiB/item（baseline ≈3.6KiB/item，
5,222 items 对应 hard ≈20.4MiB）、必须 `data_versions +0`；
preview.27/schema20 实测 5,222 items / 616s / +18.5MB / 0 new versions，PASS。
cross-generation envelope 约 37min 仅为 measured baseline，尚无批准 hard SLO；
真实 append/rewrite 与其余场景各自独立计量。

> 最新提交交接与验证边界见 [本机优化交接](issue-262-local-performance-handoff.md)。前半部分保留分析过程，后续实测与修正优先。

## 范围与结论

本文分析 `issue-262-phase-d` 上 preview.23/preview.24 的全市场 MCP-only 实测，覆盖 daily 初采、yearly 派生、SQLite 发布、查询路径和存储增长。性能数字必须与规模、硬件、构建版本和运行环境一起解释；fixture、100-item smoke 和未经复测的估算不作为全市场性能通过证据。

当前结论：主要数据路径已经达到线性或接近线性复杂度，preview.24 已通过全市场 yearly 任务总耗时和存储容量目标；yearly 单 item p50 仍为 158ms，高于 `<150ms` 目标。daily preview.24 的完整时间窗口被外部 Chrome 争用污染，不能作为纯代码基线。整体生产认证仍为 `production_ready=false`。

## 实测基线

preview.24 的证据文件为 `evidence/issue-262-mcp-daily-sync/phase-g-preview24-fullmarket.json`：

| 项目 | 实测 | 目标/判定 |
|---|---:|---|
| 环境 | ECS 2 vCPU / 2GiB / 40GB，release，schema-19 | 固定验收环境 |
| 规模 | 5,221 entities，16.65M daily bars | 全市场 |
| daily 发布 | 5,221/5,221 | 通过 |
| yearly 发布 | 5,221/5,221，0 失败 | 通过 |
| yearly accept | p50 1.01s，p90 1.77s，max 5.17s / 100 items | `<5s`；max 边缘 |
| yearly 执行 | p50 15.79s，p90 22.04s，max 24.85s / 100 items | 任务总和 867.9s，`<15min` 通过 |
| yearly 单 item | p50 158ms | `<150ms`，未通过 |
| yearly 端到端 | 1,020s（约 17min） | 含 2.5min availability 预检和串行驱动开销 |
| 数据库 + WAL | 847MB | `<=2.5GiB`，通过 |

preview.23 到 preview.24 的 yearly 改善为：accept p50 `3.42s -> 1.01s`，单 item `198ms -> 158ms`，任务总和 `1036.5s -> 867.9s`，端到端 `22.6min -> 17.0min`。

daily 窗口为 4,550.1s，但证据记录有约 45 分钟的无关 headless Chrome CPU 争用；停止 Chrome 后 chunk 从约 200s 降至约 5.5s。因此该窗口证明了部署环境的资源敏感性，不能直接证明代码需要 75 分钟。

## 复杂度对比

令 `N` 为实体数，`B` 为所有 bars 数，`H` 为单实体参与计算的历史 bars，`C` 为本次变更 bars，`Y` 为年份数，`D` 为 parent-delta 链深度，`k` 为查询返回 bars 数。

| 操作 | 理论下界 | 当前主要路径 | 评价 |
|---|---:|---:|---|
| 单版本元数据读取 | `Omega(1)` | `O(1)`，`fixed_facts` | 基本达到下界 |
| 日期窗口读取 | `Omega(k)` | `O(D+k)` | 额外成本为 delta 链 |
| 完整历史读取 | `Omega(H)` | `O(D+H)` | 接近下界 |
| 初次 daily 导入 | `Omega(B)` | `O(B+metadata)` | 渐近最优，仍可降常数 |
| yearly 聚合 | `Omega(H)` | `O(H+Y+D)` | 未保存年度摘要时合理 |
| 增量 daily 更新 | `Omega(C)` | 目标 `O(C+D)`，部分 runner 校验可能接近 `O(H)` | 仍是主要算法风险 |
| 增量版本存储 | `Omega(C)` | `O(C+metadata+refs)` | 接近下界 |
| no-change 更新 | `Omega(N)` | `O(N+metadata)` | 接近下界 |
| 版本链查询 | `Omega(k)` | `O(D+k)` | 由 checkpoint 限制深度 |

初次导入不能低于 `Omega(B)`，因为每条输入必须被解析、验证和保存。历史数据持久化不能低于 `Omega(B)` 空间下界。schema-19 的 immutable segments、parent-delta 和 no-change 复用避免了每次更新复制完整历史，空间复杂度已接近合理下界。

## 为什么过去聚合很慢

旧路径将完整 daily 历史反复物化和序列化：读取 legacy envelope、构造 bars 数组、聚合、再次验证 lineage、重新构造 yearly envelope、计算 digest，并为一个 item 执行多次 SQLite 写事务。该流程的主要问题是重复工作，而不是 yearly OHLC 聚合公式本身。

已完成的优化包括：

- `fixed_facts` 和 `fixed_window` 将已发布读取改为事实/窗口读取；
- published envelope 不再携带完整 bars 数组；
- yearly verifier 和 `derive_yearly` 复用一次 `AnnualSeries`；
- lineage 和 commit 使用窗口读取；
- `fixed_window_years` 只判断年份存在，不物化 bars；
- 对相同 base digest、年份和 `as_of` 复用聚合结果；
- compact schema-19 使用 typed segments 和 parent-delta。

这些改动将主要成本从重复的全历史物化，降为一次窗口读取、一次聚合和 typed publication。

## 当前瓶颈

剩余的 158ms/item 主要属于固定成本，而非大 O 退化：

1. 每个 item 仍涉及 StartItem、stage、commit、FinishItem 等多个写事务；
2. stage、validated、publish 之间存在重复状态和完整性处理；
3. 聚合结果、版本内容和兼容摘要可能重复计算 digest；
4. 深历史实体的 accept max=5.17s 表明存在慢 chunk 尾部；
5. SQLite 单写者和 2 vCPU 环境放大事务与 fsync 固定成本。

## 下一步优化与验收

第一步应先在 ECS 对一个 yearly chunk 使用 `perf record -g --call-graph dwarf`，确认 CPU 时间在 SQLite 提交、digest、segment 扫描和历史物化之间的分配。没有 profiling 证据前不应直接改数据布局。

优先实施 yearly 单 item 写事务合并：在保持 legacy 和 compact backend 语义一致的前提下，将 stage、mark-valid、publication/head CAS、task result 和 FinishItem 纳入同一个事务。随后验证 request replay、head 冲突、事务失败和崩溃恢复。验收应在相同 ECS 环境报告 p50、p90、max、任务总和、WAL 峰值和失败数，并以 `<150ms/item` 为目标。

若事务合并后仍未达标，再按以下顺序考虑：

1. 复用 yearly digest 和已经验证的聚合结果；
2. 为 daily version 保存按年摘要，使 yearly 从 `O(H)` 降至 `O(Y)`；
3. 对 append 使用增量年度摘要，对 backfill/revision 只重算受影响年份；
4. 使用按 segment/年份绑定的 prefix 或 Merkle digest，避免为完整性证明重新物化历史。

按年摘要和增量 digest 必须绑定 version、generation、coverage 和 lineage；不能以缓存命中替代来源或完整性验证。并行写入不是当前优先方向，SQLite 单写者下可能增加锁竞争、WAL 和尾部延迟。

## 限制

- preview.24 daily 完整窗口受外部 Chrome 争用影响，需要无竞争复测；
- yearly 端到端时间包含 availability 预检和串行 MCP 驱动开销，不能与 task-internal 时间混为一谈；
- 5 个证券缺少官方 `.day` member，是来源数据缺口，不是性能失败；
- 性能通过不等于 `production_ready=true`，生产认证仍属于 #225 范围。

## 本机优化实测（2026-09-23，工作区基于 434aa00）

本轮只在 Box 上优化软件路径，没有改 ECS 或发布新版本。新增可重复命令：

```sh
cargo test --release --locked --test yearly aggregation_ab_timing -- --ignored --nocapture
```

基准将 `434aa00` 的纯聚合实现冻结在 `tests/support/yearly_reference.rs`，与当前实现同进程、单线程交替执行。输入是 10,000 条合成 daily bars；21 轮，每轮每实现 20 次；构造输入在计时外，使用 black_box，禁用 memo 路径。一次实测旧版 p50=1042.8µs，新版 p50=862.1µs，耗时下降约 17.3%，速度比 1.210。此结果只覆盖 CPU 聚合，不含 SQLite、MCP、来源读取或发布；不是完整任务的加速比或理论最优证明。

实际保留的修改：

- 年度分组利用日期有序性维护连续区间，替代 BTreeMap 和逐 bar 引用 Vec；分组更新 O(H)，额外区间空间 O(G)，G 为输入包含的年份数。
- 显式年份存在性使用二分搜索，O(Y log G)，不会因逐年线性查找退化为 O(YG)。
- 派生 yearly batch 直接复制元数据，不克隆后又丢弃 daily bars。
- 不可变 YearlyCandidate 保存一次 compat::check 结果，应用层后续通过引用复用；存储读取和提交时的独立核验仍保留。

范围修正：sum_known 仍为每年收集 Decimal Vec，所以整体聚合临时空间不是纯 O(G)，还包含 O(M)，M 为最大年度 bar 数。此前对 no-change 字段直接比较的改动已撤回，以保留 JSON 中 Decimal 表示和 missing/null 的原始比较语义。成功发布的任务收尾已包含在发布事务中，不能未经跟踪就假定另有独立 FinishItem 事务。也不能凭单独窗口读取 p50=1.5ms 就断言它在完整本机调用链中不是瓶颈。

验证：yearly 算法测试 6 项通过（另 1 项手动 benchmark），yearly_publication 集成测试 25 项通过，cargo clippy --all-targets --all-features --locked -- -D warnings 通过，git diff --check 通过。现有全仓 fmt 检查在未改动的 market_storage/mod.rs 和 tests/yearly_publication.rs 有历史排版差异，本轮仅格式化修改文件。尚未完成本机完整 yearly 发布链的阶段计时和改前/改后对照，因此不宣称 158ms/item 已下降或整体已达最优。

后续同机迭代：sum_known 用可克隆的 slice iterator 先检查 None，再执行原顺序的 checked_add/checked_sub，不再收集 Decimal Vec。缺失值仍优先于部分求和溢出；未知单位仍直接返回 None。由此纯 aggregate 的额外空间降为 O(G+Y)，不包含已存在的输入、memo 或发布候选。再次使用相同 A/B 命令，旧版 p50=1003.8µs，新版 p50=793.3µs，速度比 1.265、耗时下降约 21.0%；这次仍与冻结旧版比较，不能将不同运行的数字当成单独这一步的净收益。31 项算法/发布测试及全目标 Clippy 再次通过。
