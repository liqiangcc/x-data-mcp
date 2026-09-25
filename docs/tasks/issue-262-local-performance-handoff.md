# #262 本机性能优化交接（2026-09-23）

## 范围与定位

用户要求在本机优化软件性能，本次按后续指令先提交当前修改并推送。分支 `issue-262-phase-d`，提交前 HEAD `7a8c4d69be77fdf07682959643b939c10384c85d`。本文件与性能代码同提交；通过 `git log -1 -- docs/tasks/issue-262-local-performance-handoff.md` 获取交接提交 SHA。

生产部署历史仍参考 [原交接](issue-262-codex-handoff.md)。本轮没有部署 ECS、改 schema、改 MCP 契约或事务边界，也没有发布 tag。GitHub #262 在本轮只读查询中为 OPEN。目标“本机尽可能优化”仍在进行，不能宣称理论最优或生产通过。

## 已保留的修改

- `src/yearly.rs`：日期有序输入按连续年份区间分组，替代 BTreeMap/逐 bar 引用数组；选定年份使用二分存在性检查；输出预分配。
- `sum_known`：先用 slice iterator 的克隆检查缺失，再原顺序精确求和，避免 Decimal 临时数组。未知单位、None 优先于部分溢出、checked_add 精度损失检测保持。
- `src/yearly_publication.rs`：从元数据直接构造 yearly batch，不再克隆 daily bars 后丢弃；私有 YearlyCandidate 缓存首次 compat::check 结果。
- `src/yearly_publication/results.rs`：准备计划时复用该不可变候选的校验结果；持久存储、head、base digest 核验仍保留。
- `tests/yearly.rs` 与 `tests/support/yearly_reference.rs`：冻结旧算法做差分和同进程 A/B 测量。
- `src/sqlite_market/compact_repo/tests.rs`：ignored `yearly_pipeline_timing_probe`，对十个新建 tmpfs SQLite 库各发布 10,000 daily bars 后派生 2025/2026 年数据，测 acceptance 和 runner 执行；并检查发布、lineage、head、receipt replay 和 audit。

原先尝试将 no-change JSON 比较改为 typed equality 的修改已撤回：Decimal 表示及 missing/null 语义不能未经证明就改变。当前 equivalent 保持原实现。

## 测量结果及边界

均为 Box、Rust 1.98.1、release，单线程顺序驱动。

| 测量 | 基线 | 当前 | 边界 |
|---|---:|---:|---|
| 纯聚合 p50，10,000 合成 bars | 1003.8µs | 793.3µs | 21 轮交替，每轮 20 次；约降低 21%，无 I/O 和 memo |
| compact runner 执行 p50 | 15.314ms | 14.469ms（提交前复测；首次 14.046ms） | 各十个新库，stub verifier，非 MCP，非全市场 |
| compact acceptance p50 | 2.936ms | 2.644ms（提交前复测；首次 2.658ms） | 包含 resolve、accept 及少量 runner 装配 |

发布链结果仅初步对照，顺序运行且样本少，不能精确归因于某一修改，也不能替代完整真实数据路径。纯聚合测试输入每日都有一行、非交易日历，意在测 CPU 算法。性能不含初始化与 daily 入库准备；发布链运行于 `/dev/shm`，不能说明磁盘持久化耗时。

基线发布链通过 `git archive HEAD` 在 `/tmp/x-data-yearly-baseline-ipt86df1` 导出提交前源码，复制相同 probe 测量；需要同时导出 `src`、`Cargo.toml`、`Cargo.lock`、`build.rs`、`migrations`、`tests/fixtures`。曾因遗漏 migrations/fixtures 编译失败，补齐后成功。基线与工作区复用 target 目录；再次运行必须用 cargo 在正确源码目录构建，不要直接执行 target 中未知版本二进制。临时目录不是交付依赖。

## 验证与复现

```sh
cargo test --release --locked --test yearly --test yearly_publication
cargo test --release --locked --test yearly aggregation_ab_timing -- --ignored --nocapture
cargo test --release --locked --lib yearly_pipeline_timing_probe -- --ignored --nocapture
cargo clippy --all-targets --all-features --locked -- -D warnings
git diff --check
```

已验证算法 6 项、发布集成 25 项，A/B 和 pipeline probes 通过。之前另有 yearly 筛选的 5 项库测试通过，但不是最新所有改动的全库回归。全量 cargo test 尚未重跑。

全仓 cargo fmt --check 有原有排版差异（market_storage/mod.rs、tests/yearly_publication.rs 等），本轮避免混入无关格式化。仅格式化修改代码和新增 probe。Box overlayfs 运行守卫见 AGENTS.md；本 probe 在 tmpfs 创建新库，不改已有容量库。需要全套测试时按仓库要求使用 TMPDIR=/dev/shm，注意其 64MB 容量。

## 下一步

1. 在同机重复交错运行发布链基线与优化版，报告分布和原始样本，排除噪声；增加真实数据窗口、稀疏年份、no-change、不同历史长度的场景。
2. 建立 runner 阶段计时：window read、aggregate、verifier、derive、prepare、commit、receipt；当前 probe 只有 acceptance 与 runner 总时间，尚不能认定 SQLite 或 digest 是主瓶颈。
3. 按热点优化，而非继续堆叠微优化。候选中 raw 与 CheckedRecord 各保留 raw 副本，是缓存的内存代价，可进一步消除双份持有；任何改动须保持不可变约束。
4. 再评估 stage/validate/commit 融合。成功发布已经原子收尾任务，不存在当然需要合并的独立 FinishItem 成功事务。保留 replay、head CAS、来源和版本绑定、未知提交保护。
5. 必要时扩展差分测试至多年份、缺失值、Decimal scale/overflow、未选年份非法记录；不要跳过未选记录的输入合法性校验。

详细背景和历史测量见 [性能分析](issue-262-performance-analysis.md)。其中前半部分为早期假设，后续实测与修正段优先；O(1) 元数据/O(D+k) 窗口等属于简化模型，不能忽略 SQLite 索引及完整性扫描成本。
