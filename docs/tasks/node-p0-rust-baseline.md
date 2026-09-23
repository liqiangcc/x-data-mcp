# Node 迁移 P0 — Rust 基线测量

任务：NM-005（agent-coordination/node-migration）。日期：2026-09-23。
基线：`5435d93eca28622bb7f546fe0ad6f78b6a42fce5`（`migration/node-devin`，二进制 `0.2.0-preview.24`）。
关联：Epic #271、#262（OPEN，不授权迁移范围）。上游：P0 清单/契约/向量文档。
证据：`evidence/node-migration/p0/baseline/`（results.json 汇总、bench-debug/release.jsonl 原始样本、两份 latency CSV、SHA256SUMS、README）。

**本文件只冻结测量观察值，不批准 50%/1.2x 预算，不作任何 Node 更快断言。**

## 0. 测量边界

- 硬件/环境：Box `grok-bot-vm-411252337`，Intel Xeon VM `nproc=8`，内核 6.12.94+，16GB RAM（捕获时 available 7392MB），loadavg 2.68/2.75/2.01。
- 文件系统：worktree overlayfs；服务运行时目录在 `/dev/shm`（tmpfs，fstatfs 白名单内，无 opt-out）。/dev/shm 上限 64MB —— 大额写入不在此测量。
- 工具链：rustup stable cargo/rustc 1.98.1；并发：全部探针单线程，无注入并发负载。
- 计时器：`date +%s%N`（进程级 wall）与 `Instant::now()`（进程内）；RSS 读 `/proc/<pid>/status VmRSS`。

## 1. 开发回路（dev-loop）

| 测量 | 样本（ms） | exit |
|---|---|---|
| cold debug build（`rm -rf target` 后 `cargo build --locked --bins`） | 33611（单次） | 0 |
| cold release build（`cargo build --locked --release --bins`） | 92494（单次） | 0 |
| warm no-op build | 24141（首次，重建后指纹稳定化）/ 138 / 131 / 109 | 0 |
| incremental `touch src/lib.rs` → build | 5780 / 5891 / 5912 / 5434 | 0 |
| `touch tests/day_file.rs` → `cargo test --test day_file` | 7475 / 6074 / 513 / 504（后两次为已编译复跑） | 0 |

## 2. 运行时工作负载（固定合成输入，20 reps，原始样本见 bench-*.jsonl）

| 探针 | debug p50/p95（ms） | release p50/p95（ms） |
|---|---|---|
| day_file::parse 2500 条（80KB，sha256 记入样本行） | 0.888 / 1.070 | 0.012 / 0.043 |
| qfq::forward_adjust 250 bar × 20 事件 | 1.239 / 1.728 | 0.150 / 0.187 |
| yearly::aggregate 2500 bar 全年 | 1.181 / 1.462 | 0.201 / 0.325 |
| segment_digest 250 bar | 0.659 / 0.742 | 0.029 / 0.040 |
| normalize_factor ×10000 | 14.997 / 21.201 | 1.741 / 4.142 |

输入为确定性合成构造（日期严格递增、OHLC 合法、f32 amount bits），输入 sha256 记入 `bench-*.jsonl` 每行 `input` 字段。

## 3. MCP 延迟与 RSS（release 二进制）

| 测量 | 样本 | 备注 |
|---|---|---|
| `mcp_data` stdio spawn + initialize + tools/list(146KB) + 3 calls | 20 reps：p50 16.5ms / p95 24ms（min 14） | 每次含进程 spawn |
| `mcp_connect` socket 单调用（sync_status） | 20 reps：p50 18.5ms / p95 20ms | spawn+connect+1 call+teardown |
| `data_service` 启动→socket ready | 6 / 6 / 6 ms | 3 reps |
| `data_service` 空闲 VmRSS | 8292 / 8172 / 8504 kB | 3 reps |
| `data_service` 20 调用后 VmRSS | 11804 kB | 单次 |
| bench 进程 RSS | 2264→3012 kB（release 链接）/ 2444→3480 kB（debug） | |

## 4. DB/WAL 足迹（合成库）

| 项 | 值 |
|---|---|
| `task_sqlite init`（schema 18 空库） | 319,488 B |
| `task_sqlite init19`（schema 19 空库） | 397,312 B |
| 服务运行中 | `db19.sqlite-wal` 0 B（只读调用无写）、`db19.sqlite-shm` 32,768 B、`mcp.sock` 0 B、`owner.json` 29 B |
| SIGTERM 后 | socket 删除、owner.json 保留、WAL checkpoint 归并 |

## 5. AC 映射与诚实声明

- AC1：dev-loop（构建/测试）+ runtime workload（bench/MCP）均有原始样本、rep 数、p50/p95 与退出码。✓
- AC2：固定合成 parser/aggregation 工作负载以 bench harness 覆盖（未用被 ignore 的计时探针，因其为单断言非采样器——doc 说明）；非全市场声明。✓
- AC3：资源与 FS 边界已记录；无 Node 更快断言；预算仍为提案。✓
- AC4：机器可读 JSON + SHA256SUMS + README；所有数字可回溯原始样本与 SHA。✓
- AC5：无 correctness/fail-closed 失败（NM-004 的 118 测试与本次探针的 fail-closed 观察一致）；未触发 ENOSPC。✓

## 6. 范围声明

仅新增 `docs/tasks/node-p0-rust-baseline.md`、`evidence/node-migration/p0/baseline/`、`scripts/node_migration_p0/bench.rs`；未改运行时源码/测试/迁移/AGENTS/README/会话文件；无 push/merge/deploy/ECS/官方输入/真实 DB。
