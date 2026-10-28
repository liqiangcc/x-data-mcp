# Issue #383 / XDM-81 跨代日更写路径耗时构成与 acquire 切代内存

## 状态

- 本文件是**只读分析 + 实测证据**的冻结记录；不是性能承诺，不修改 [SLO](../SLO.md)、[decisions](../decisions.md)
  或生产门禁。
- 所有数字注明口径：`实测` = 本次在本机测得；`推断` = 由代码与既有测量推导、待插桩确认。未标注者为实测。
- 本机（1.9 GiB RAM、2 vCPU、共享负载）**不允许编译 Rust**，故本轮不产出 runtime 内测；全部证据来自
  ①生产库的只读副本（`/root/xdm81-bench/prod-copy.sqlite`）②按仓库真实 DDL 建表的本机 SQLite 微基准
  ③既有 XDM-64 验收报告。runtime 级复核入口见 §6（`X_DATA_MCP_STAGE_TRACE=1`）。

## 1. 证据锚点

| 锚点 | 数值 | 来源 |
|---|---|---|
| 跨代日更（w=1）总时长 | 822.41 s / 5,199 items | XDM-64 measure-report |
| 跨代日更（w=2 / w=4） | 660.97 s / 618.6 s | 同上（#429） |
| SLO 行 2 | 600 s（D-52 豁免） | [SLO](../SLO.md) |
| `start_collection` item 均值 | 626.3 s / 5,226 items = **119.8 ms/item** | 生产库 `task_items`/`market_events` 时间戳 |
| 其中事务内（首个事件前） | p50 **25.00 ms**、p90 49.86 ms、max 319.87 ms | 同上（5,226 项） |
| `no_change` item | 6.4–17.9 ms（随 range 天数增长） | 同上 |
| 稀疏发布 item（派生） | calendar 33.7 s、suspensions 35.7 s、security_master 2.5 s | 生产库同窗口 |
| 行数（每 entity 每代） | adjustment 11.0 行、capital 11.7 行、evidence ≈584 B | 生产库聚合 |
| 平均序列长度 | 2,142 bars/series（33.6 M 行） | 生产库聚合 |

写路径单行常数（`synchronous=FULL`、WAL、2,000 行/事务、中位数 9 次）：

| 写 | µs/row |
|---|---|
| `adjustment_events` INSERT | 16.5 |
| `capital_events` INSERT | 11.8 |
| `market_events` INSERT | 7.5 |
| `series_bars` INSERT | 2.6 |
| adjustment 整集（11 行）替换 | 86.3 |
| capital 整集（12 行）替换 | 100.4 |

事务常数：100 items 一个事务 **0.38–0.55 ms/item**；改为每 item 一个事务 **3.28 ms/item**，即
commit/fsync ≈ **2.75 ms/次**（两次独立测量，中位数 7 次）。

## 2. 跨代 item 的时间构成

### 2.1 item 全预算：写路径只占 0.3–0.4%

| 段 | 每 item | 占 158 ms/item | 性质 |
|---|---|---|---|
| prepare/acquire（typed 前缀证明、全历史读取、qfq 映射） | ≈157 ms | ≈99% | 推断：全预算减去已实测的写者与簿记 |
| 事务提交与写路径（本文 §2.2） | 0.38–0.55 ms | 0.3% | 实测 |
| 任务簿记（读 task item、状态更新） | <1 ms | <1% | 实测（`no_change` 6.4–17.9 ms 含此段） |

`822.41 s / 5,199 items = 158.2 ms/item`。事务内基线可由 `start_collection` 的 25 ms（p50，含 2,142 bars
全量重写）交叉校验：该口径下写者占 38%，与「增量 append 只需 0.4 ms」并不矛盾——差别在重写量。

**结论：跨代 822 s 的差距不可能由写路径解释。** 要进 600 s 必须动 prepare 侧（§3 O1、O3），写路径
优化上限约 5 s。

### 2.2 写路径事务内前 5（一个 cross-gen append item，实测 0.38–0.55 ms）

| # | 项 | µs/item | 占 0.38–0.55 ms |
|---|---|---|---|
| 1 | 派生整集重写（adjustment 11 + capital 12 行，DELETE+INSERT+版本 upsert 一半） | 187 | 34–49% |
| 2 | 行情写入组（events 2 + head UPDATE 2 + tail bars 8 = 12 行） | 57 | 11–15% |
| 3 | task item 三写（running / execution_start / finished，finish 重写约 340 B JSON） | ~30 | 6–8% |
| 4 | `derived_dataset_versions` 2 次 upsert（adjustment_events + capital_events 各一行） | ~30 | 6–8% |
| 5 | 证据（continuity/version 2 写 + 1 次 evidence 读） | ~20 | 4–5% |
| — | 余下：保存点与语句开销、head/evidence 读取、digest 比较 | 55–225 | 15–40% |

口径说明：第 1、2 名由「行数 × 单行常数」合成（行数来自生产库，常数来自 §1 微基准），第 3–5 名同为
模型值；合计与实测总和的差额计入「余下」。本机负载抖动使逐项消融（ablation）不可用（跳过任一组件的
耗时反而更高），因此**逐项占比只作排序依据，不作精度承诺**。

已排除的两个候选：

- `next_event_seq` 的 `SELECT MAX(seq)`：`market_events.seq` 是 `INTEGER PRIMARY KEY`（rowid 别名），
  取最大值是 b-tree 末端 O(log n)，不是扫描；15,691 事件的表上无成本。
- WAL checkpoint：`wal_autocheckpoint=1000` 页，跨代窗口内未触发；延迟 checkpoint 无收益。

### 2.3 与 #429 并行度结论的关系

w=2 只把 822 s 降到 661 s（−19.6%），w=4 到 619 s。本机是 1 物理核 2 超线程（[STATUS](../STATUS.md)），
并行 prepare 的吞吐上界约 1.2×，因此该降幅是**主机拓扑限制**，不能反推「60% 时间在单写者上」。写者实测
只占 0.3%，与之一致：boost 的收益来自 prepare 并行，不是事务合并。

## 3. 优化候选（保留 fail-closed 与单写者）

| # | 候选 | 估计收益 | 风险 | 崩溃恢复 | replay 一致性 | 结论 |
|---|---|---|---|---|---|---|
| O1 | 保持分块事务（100/chunk），不引入 per-item 事务 | 现状已最优：per-item 会多付 2.75 ms/item（5,199 项 ≈ +14 s） | 无 | 每 chunk 一次 `BEGIN IMMEDIATE`，item 级 savepoint 不变 | 不变 | **维持现状**，并把它写成约束 |
| O2 | 派生整集改为「内容未变只改 binding」的 UPDATE | 实测 ≈0（0.60 vs 0.55 ms/item）；ranged-delete 上限 ≈90 µs/item（≈0.5 s/全量） | 与 `derived_dataset_versions` 版本断言、replay 逐行比对耦合 | 不变 | 需证明「内容未变」与版本指纹等价 | **不做**，收益不足 |
| O3 | prepare 侧去重：同一 entity 的 base bars / 全历史 / qfq 映射在 forward+raw 两侧共享，跨 item 缓存 | 推断最大项；须先用 §6 的 span 定位 | 缓存键漏算 generation/coverage 会破坏 fail-closed | 无（纯内存缓存） | 不变 | **推荐下一步**，先测量 |
| O4 | 延迟/放大 checkpoint（`wal_autocheckpoint`、手动 truncate） | 0（窗口内未触发） | 故障后 WAL 重放时间变长 | 变长 | 不变 | **不做** |
| O5 | acquire 切代内存流式化（本文 §4） | 峰值 511 MiB → 64 KiB 缓冲 | 无：digest 与字节校验完全一致 | 不变（staging 由 manifest 字节校验兜底） | 不变 | **本次实现** |

排序依据：收益确定性 × 风险。O3 是唯一可能撬动 SLO 的方向，但它需要 runtime 插桩证据才能定方案，
本轮不猜；O5 是唯一「大收益 + 零语义变化」的项，因此作为本 PR 的实现项。

## 4. acquire 切代内存峰值

### 4.1 峰值来源定位

| 事实 | 数值 |
|---|---|
| XDM-64 实测 VmHWM（无上限） | 523,528 kB = **511 MiB** |
| 480 MiB 上限下峰值 / 可用内存 | 479.8 MiB / MemAvailable 302 MiB（靠 swap 撑住） |
| `hsjday.zip` 规模 | ≈550 MB ≈ 524 MiB |

511 MiB ≈ 单份 day archive 的 97%，指向「一份完整 artifact 常驻」。逐条清点切代路径上的整文件物化点，
改前只有两处，且都在 fetch 阶段：

1. `Source::Mirror` 的 `Mirror::fetch`：`fs::read` 整份文件再写盘（镜像/离线验收路径）。
2. `fetch_role` 的续传校验：`fs::read` 整份文件算 sha256。

其余都是小对象：`snapshot::inspect` 已是 64 KiB 流式；manifest 限 1 MiB；`probe_*` 经 `member_bytes`
逐成员读且单成员上限 64 MiB（采样 64 个成员，同一时刻只有一个在内存）；install 是目录 rename。

**结论：峰值不是解压，也不是 manifest 校验，而是整份 artifact 读入内存。**

### 4.2 方案

- 统一走 `crate::hash` 的流式实现：`digest_reader` / `digest_file` / `copy_digest`，固定 64 KiB 缓冲，
  与内存版 digest 逐字节等价（见 §5 测试）。
- 镜像 fetch 改为「读→写→hash 一趟」，不再先 `fs::read` 再 `fs::write`。
- 续传校验改为 `digest_file`，长度检查在前，仍对不上就重下。
- 备选（本轮未做，也不必要）：把 `member_bytes` 改成按需区间读、manifest 之外引入流式 unzip；
  当前单成员上限 64 MiB 已在预算内。

预期效果：切代峰值从「≈1 份 archive」降到「64 KiB + 既有基线」，480 MiB 上限下不再依赖 swap。
崩溃恢复语义不变：staging 目录仍是临时区，装代由 manifest 逐字节校验兜底；写一半的文件长度/摘要都不匹配，
只会被判为未完成并重下，不会进入 `install_staged`。

## 5. 本次落地

- `src/hash.rs`：新增流式 `digest_reader` / `digest_file` / `copy_digest`（`STREAM_BUFFER = 64 KiB`），
  单元测试覆盖 0 B、缓冲边界 ±1、跨缓冲、文件缺失（fail-closed）、流式拷贝字节与 digest 等价。
- `src/acquire/fetch.rs`：`Mirror::fetch` 改流式拷贝+摘要；测试用 3×缓冲的 artifact 证明 bytes/sha/内容与
  旧行为一致。
- `src/acquire/service.rs`：续传校验改用 `digest_file`；单元测试证明「摘要一致则保留、同长度篡改则重下」。
- `src/snapshot.rs`：删掉自成一套的 64 KiB 摘要循环，改用共享实现（行为不变）。
- `src/sqlite_market/{series_commit,compact_repo}.rs`：补 `daily.commit.*` 的 `stage_trace` span，为 §3 O3
  的 runtime 测量提供入口。默认关闭，不影响行为与耗时。

## 6. 测量入口（后续复核）

```
X_DATA_MCP_STAGE_TRACE=1   # 每个 span 一行 XP025_STAGE {json} 到 stderr
X_DATA_MCP_RUN_PREPARE_WORKERS=1  # 单线程 prepare，串行可读
```

新增 span：`daily.commit_batch.item_payload`、`daily.commit.series.{forward,raw}`、`daily.commit.raw_side`、
`daily.commit.factor_set`、`daily.commit.{adjustment,capital}_events`；与既有 `daily.prepare.*`、
`daily.commit_batch.{item_start,item_finish}` 拼出完整 item 预算。本机微基准脚本不随仓库提交
（`bench_append.py`/`rows.py`，seed 与表结构复制自 `migrations/tasks/*`）。

## 7. 后续建议

1. **O3（推荐）**：用 §6 的 span 在 aws-node 上跑一次 100-item 跨代窗口，量化 prepare 的
   `base_reads`/`acquire_typed`/`states` 三项，再决定是共享 forward+raw 读取还是缓存全历史。
2. **写路径**：把「跨代追加必须留在分块事务内」写成显式约束（注释或 ADR），避免以后因某个 bug
   回退到 per-item 事务（+2.75 ms/item）。
3. **内存**：把 480 MiB 上限留在 SLO 里，但补一条「峰值不得随 artifact 大小线性增长」的断言。
4. **度量**：`daily.commit.series.raw` 与 `.forward` 的差值可直接验证「raw 侧 qfq 依赖」的成本，
   若 raw ≈ forward，说明 raw 侧可复用 forward 的中间量。
