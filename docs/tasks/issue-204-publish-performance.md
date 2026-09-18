# Issue #204 发布路径性能优化方案

## 状态

- 本文件是优化方案与实测证据的冻结记录；不是性能承诺。
- 所有性能数字均注明：数据规模、构建模式、环境（Box,8 CPU,overlayfs `/home`）、测量方式（`[PERF]` 插桩于 scratch worktree `/home/box/x-data-mcp-perf`，基线代码 `2b7a0c4`）。
- 本方案不改变任何 fail-closed 语义、证据语义、provenance 绑定或生产门禁状态。

## 1. 背景

#4 全目录 `market.daily` 首采验收（5230 计划，#203 freeze plan）运行中实测：

- 单 item ~17-18s（debug build）；5224 items 预估总时长 ~24-26h。
- 进行中观测（progress.txt）：15.2h 完成 3053 items（58%），pace 稳定。
- 在跑进程 `mcp_data`：CPU 99.7% 单核打满，RSS ~90MB —— 纯 CPU-bound，无 IO 等待，无内存压力。

## 2. 定位方法与结论

### 2.1 测量方法

scratch worktree 中对发布路径逐段插桩（`eprintln!("[PERF]")`)，以 retained gen-2 快照实跑 `run_worker_once` 完整链（受理→worker→物化→发布），插桩代码仅用于测量，不进入主线。

### 2.2 单 item 分解（debug,sz.000002 = 8479 bars，总 ~60s)

| 段 | 耗时 | 占比 | 性质 |
|---|---|---|---|
| materialize（含一次性 gbbq 解码 ~2s) | ~2.15s | 3% | 必需 |
| task 簿记（read/StartItem) | ~7ms | ~0% | 必需 |
| **validate_and_deduplicate ×6 遍** | **~55-59s** | **~93%** | 见 2.3 |
| to_value×2 + legacy_digest | ~0.25s | ~0% | 序列化 |
| INSERT ~8500 行 | ~0.24s | ~0% | IO |
| 其余投影/读取 | ~0.3s | ~0% | IO |

### 2.3 根因：O(N²) × 6 遍

`src/publish.rs` `validate_and_deduplicate`：对每条 bar 用 `records.iter().find(...)` 线性扫描全部已收记录，且 `record_key` 每次调用分配一个新 String。8479 bars → ~3600 万次 String 分配+比较。

**O(N²) 实测验证**（三 symbol 比率检验，debug):

| symbol | bars | 单遍耗时 | 比率检验 |
|---|---|---|---|
| sh.600004 | 5630 | 4.26s | — |
| sh.600006 | 6467 | 5.67s | (6467/5630)²=1.32 vs 实测 1.33 |
| sz.000002 | 8479 | 9.72s | (8479/5630)²=2.27 vs 实测 2.28 |

**放大器**：该校验每 item 被调用 6 遍——

| 遍 | 调用点 | 所在事务 | 防的窗口 |
|---|---|---|---|
| 1 | runner `validate_and_deduplicate` | 内存 | 构造性（产出 records+summary)，必需 |
| 2 | `stage` → `compat::check` | 写 tx A | 待写对象自洽 |
| 3 | `prepare` → `candidate` | 读 tx B | stage→prepare 窗口 |
| 4 | `prepare` → `mark_validated` | 写 tx C | B→C 窗口；判据本身已是 raw 字节比对 |
| 5 | `commit` → 候选重读 | 写 tx D | C→D 窗口 |
| 6 | `commit` → 提交后回读 | 同 tx D | 本事务写入正确性自验 |

注：另有第 7 处调用不在正常路径——`commit_publication` 幂等重放分支（publication 已存在时对已发布记录做一遍 `read_projection` 全量校验再比对 raw)。不影响首采计数，但 update/retry 路径每次重放都付一遍全量校验成本。

### 2.4 被实测排除的假设

| 假设 | 实测 | 结论 |
|---|---|---|
| overlayfs fsync/WAL 慢 | 6.5ms/commit | 排除 |
| 大表（11.7M 行）INSERT/索引退化 | 随机位 4000 行插入 20ms | 排除 |
| 版本投影 SELECT 慢 | 4000 行 10ms | 排除 |
| 任务簿记开销 | ~7ms/item | 排除 |
| 内存压力 | RSS ~90MB | 排除 |
| 物化成本 | ~0.12s/item（共享 session 后） | 排除 |

## 3. 已验证的修复效果（scratch 实测）

HashMap dedup(O(N)，语义逐字节等价）,sh.600601 = 8591 bars:

| 段 | 修复前 | 修复后 | 倍率 |
|---|---|---|---|
| validate_dedup 单遍 | 11.1s | 17.7ms | 630× |
| **item 总计（debug)** | **74.7s** | **4.76s** | **15.7×** |
| **item 总计（release，实测）** | — | **~0.66s**(sz.000002 8479 bars) | — |

release 残量分解（sz.000002)：物化 78ms + 校验×6 遍 ~350ms + 序列化/digest ~55ms + 写入 ~45ms + 簿记 ~5ms。

### 3.1 全目录预估（如实标注外推）

| 方案 | 5230 预估 | 依据 |
|---|---|---|
| 当前 main + debug | ~24-26h | 在跑观测 ~18s/item，与 Σb² 模型吻合 |
| L1 + debug | ~4-4.5h | 实测 2.8s/item 稳态 |
| **L1 + release** | **~0.5h** | 实测 0.66s/8.5k-bar item,item 成本≈线性于 bars（均值 3186) |

## 4. 优化切片

### Slice A（本 issue，纯等价）

`validate_and_deduplicate`:HashMap 去重，key 用整数（Day→`num_days_from_ce`,Year→`year`)，零分配；冲突报错时构造 `record_key` 字符串。**排序键保持 `record_key`(String) 不动**——ISO 字符串序与整数序仅在 year≥1000 时一致（"999" 字典序后于 "1000")，保留 String 排序使输出顺序在任何合法输入下逐字节等价；排序只分配 N 个 String，非热点。

等价性：`records` 内 key 唯一（仅未命中时 push),`find` 命中 ≡ map 命中（整数 key 与 String key 一一对应）；冲突错误携带相同 key；重复计数口径不变；排序输出逐字节一致。

测试：现有验收断言天然覆盖（发布内容逐字节一致）；补直接单测：同值重复计数 / 异值冲突及 key / 排序输出 / day+year 两种 period。

### Slice B（可并入 A 的独立 commit，纯等价）

- `insert_record`:`c.execute` 每行重 prepare → `prepare` 外提（write_tx ~43→~25ms release);
- `legacy_digest`/`text(&r.raw)` 复用：同一 payload 序列化 ~4 次→1 次、digest ~5 次→1 次（~100ms/item)。

### Slice C（弱治理门槛，可独立）

遍 4 `mark_validated` 全量重验降级为 `SELECT raw` 字节比对——判据本来就是字节相等，投影篡改由遍 5 在发布前兜底。~80ms/item(release)。

### 明确不在本方案（各自独立裁定）

| 项 | 收益 | 为何独立 |
|---|---|---|
| L2：遍 2-6 改 digest 级校验 | ~250ms/item | 治理裁定：digest 比对 ≡ 全量重读 |
| O3:payload 去重（records+batch 双份） | ~40% 残量 + DB 9GB→4.5GB | 版本格式契约变更 |
| 并行 worker | ~4-6× | 认领模型（`accepted_run_id`)/取消语义变更；`SqliteMarket` 每 op 独立连接、WAL 读并发已具备前提 |
| task 级事务批量 | ~30ms/item | 原子性粒度变更（item 级→task 级） |
| 持久缓存/SQLite staging | 跨进程复用解码 | 已暂缓的架构决策，需绑 manifest_sha+generation+换代失效 |
| synchronous=NORMAL | ~10-30ms/item | 耐久性权衡，治理裁定 |

### 读路径同构受益

`query_data` 每页/`versions` 每条目都走 `read_record` 全版本校验（`sqlite_supply.rs:44,67,90`)。L1 后每页 ~10s→~0.4s(debug)/~90ms(release);L2 若裁定通过则到 ms 级。

## 5. 执行顺序与停止线

```
1. Slice A(+B 同 PR 分 commit)→ review → merge
2. #203 PLAN 加修订注记:release build(动机=运行时长,非性能声明)+ 首跑中止记录
3. release 重跑 53 片 → 预期 ~0.5h,产出"最终代码"的验收证据
4. 停止线:首采时长在验收窗口内可行 + 消费侧翻页延迟可接受 + update/backfill/yearly 链可行
   → 满足则停止;L2/O3/并行 作为已知余量归档,有真实痛点再激活
```

## 6. 对当前运行中的 5230 验收

两种处置（决策点）:

- A. 跑完（剩 ~8h)：产出未优化基线证据；
- B. 中止→合 Slice A→release 重跑：~1.5-2h 到最终代码证据，scratch DB 保留作中止记录。
