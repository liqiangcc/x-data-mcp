# Issue #262 交接文档（→ Codex）

> 目标：AI 仅通过两个 MCP server + 仓库 Skill 完成每日市场数据更新，无 shell/SSH/SQLite 旁路。
> 本文件交接当前状态、已验证事实、遗留工作与操作要点。所有性能数字均注明规模/环境/版本。

## 1. 当前状态（截至 2026-09-23）

- 分支：`issue-262-phase-d`，HEAD = `434aa00`（已推送 origin）
- 最新部署：**preview.24**（tag `v0.2.0-preview.24`，commit `9bf103d`）已部署到 ECS，fresh schema-19
- 全市场已在 ECS 上通过纯 MCP 跑通：catalog → daily → yearly
- dist 传输：`refs/dist/v0.2.0-preview.24`（Git transport，非 GitHub Release）

### preview.24 实测（ECS：2 vCPU / 2GiB / 40GB；5,221 实体 / 16.65M daily bars）

| 指标 | 实测 | 目标 | 状态 |
|---|---|---|---|
| daily 发布 | 5,221/5,221（5,226 catalog − 5 只官方缺口） | 全市场 | ✅ |
| yearly 发布 | 5,221/5,221，0 失败，0 残留 NEEDS_VERIFICATION | 全市场 | ✅ |
| yearly accept p50 | 1.01s/百条（p90=1.77，max=5.17） | <5s | ✅（max 边缘） |
| yearly per-item p50 | 158ms | <150ms | ⚠️ 差 ~8ms |
| yearly task 合计 | 867.9s | <15min | ✅ |
| yearly 端到端 | ~17min（含 2.5min availability 预检 + 串行轮询） | — | — |
| 存储（DB+WAL） | 847MB | ≤2.5GiB | ✅ |
| 本地=ECS 二进制 SHA | data `1b87eb58…` / acquire `812223da…` | 一致 | ✅ |
| MCP-only 数据路径 | 零 shell/SQLite 旁路 | 必须 | ✅ |

完整证据：`evidence/issue-262-mcp-daily-sync/phase-g-preview24-fullmarket.json`；
基线对比：`phase-g-preview23-fullmarket.json`（accept 3.42s、per-item 198ms、task 1036.5s）。

## 2. 已完成的关键优化（勿回退）

按提交顺序（均在 `issue-262-phase-d`）：

1. `7d00c18` — 读路径：`MarketRepository::fixed_facts`（O(1) 行查）+ `fixed_window`（窗口物化）；
   published 版本 `raw()["integrity_sha256"]` 报告存储 typed `integrity_digest`（**Design A**，
   staged/validated 仍用 legacy envelope digest，`compat::check` 自洽需要——不要混淆两种 digest flavor）
2. 第二轮 — `synthesize` 对 published 版本不再序列化 bar 数组进 `raw["batch"]`；
   runner 单次 aggregate，`AnnualSeries` 同时喂 `YearlyVerifier::verify` 和 `derive_yearly`
3. 第三轮 — `verify_lineage` 窗口化（`leaf_bars_in_range`）；`commit` 的 base digest
   复核从全量物化改为行读
4. `5843adc` — `CompactBar::check` 词法 canonicality 检查（替代 normalize+to_string）；
   `fixed_window_years`（只判年份存在，不物化 Bar）；聚合级内容寻址 memo
   （key = base integrity_digest + years + as_of）；`Decimal::new(cents,2)` 替代除法

容量库基准（11,783 符号 / 29.2M bars / Box release）：`fixed()` p50=28.4ms → `fixed_window()` p50=2.9ms。

## 3. 遗留工作（优先级排序）

### 3.1 per-item 158ms → <150ms（唯一未达标 stretch）

剩余开销已分散，候选方向：

- **合并 per-item 写事务**：当前每 item 约 4 个写事务（StartItem、stage、commit_yearly、
  FinishItem）。可把 stage+mark_validated+commit 融合进单个事务（`commit_yearly` 已支持
  plan-only 模式的雏形，见 `yearly_publication.rs`）。注意 legacy 后端需要 candidate 已
  stage，融合时要保持两个后端语义一致。
- **digest 复用**：聚合结果与版本内容的 sha256 有重复计算空间。
- **accept max=5.17s** 与 p90=1.77s 差距说明尾部有慢 chunk（深历史 sh.60xxxx 段），
  若继续优化优先看 `fixed_window_years` 在深历史上的 segment 扫描。

**建议先做 perf 验证再改**：ECS 上用 `perf record -g -p <data_service pid>` 跑一个 yearly
chunk，确认热点再动手。之前帧指针缺失时用 dwarf call-graph（`--call-graph dwarf`）。

### 3.2 部署脚本硬化（`scripts/deploy_phase_g.sh`）

- `REPO` 默认 `/root/x-data-mcp`，实际仓库在 `/home/box/x-data-mcp`——部署时必须显式
  传 `REPO=/home/box/x-data-mcp` 或在正确 cwd 下运行；建议修默认值或自动探测。
- 无 GitHub Release 时应自动回落 `--force-git-transport`（当前需手动加 flag）。
- payload 缺文件时报错不够具体（曾遇到缺 `SHA256SUMS` 报 "No such file"）。

### 3.3 已知数据缺口（非系统缺陷，如实记录即可）

- `sz.001246 / 301569 / 301660 / 301686 / 301716`：官方 hsjday 包无 `.day` member。
- 已有 daily head 的实体不能用 `start_collection` 重采，须走 `update_data` 增量路径。

### 3.4 Issue #262 收尾

- DoD 核心项已全过；不要宣称 issue/PR 完成/合并，除非 `gh` 查到权威状态。
- `production_ready=false` 仍为真（生产认证在 #225 范围）。
- 可选项：把 5 只缺口股、Chrome 竞争事件、磁盘写满崩溃恢复事件整理进 issue 评论。

## 4. 环境事实（重要，踩过坑）

### Box（本机，`/home/box/x-data-mcp`）

- Rust 用 rustup stable **1.98.1**（`~/.cargo/bin`）；自带 cargo 1.85.1 编译不过。
- `/home`、`/tmp` 是 overlayfs，runtime guard fail-closed：
  - 全量测试必须 `TMPDIR=/dev/shm`（仅 64MB）
  - `mcp_connect` / smoke 需要 `X_DATA_MCP_UNSAFE_NONLOCAL_FS=1`（仅限本地验收，生产禁用）
- 固定证据输入：`/home/box/retained-inputs/`（官方文件只下载一次）

### ECS（`ssh ecs-node`）

- 预览根：`/opt/x-data-mcp-preview`；current 软链 → 当前 release
- DB：`state/preview.sqlite`；socket：`run/mcp.sock`（数据）+ `run/acquire.sock`（采集）
- 部署脚本在 ECS `/opt/x-data-mcp-preview/deploy_phase_g.sh`（从仓库同步更新）
- 磁盘 40GB 曾写满导致 service 崩溃——部署/跑批前 `df -h /` 检查；归档 DB 在
  `state/preview.sqlite.v*-archived-*`，空间紧张时可清理
- **曾发生**：无关 headless Chrome（`/tmp/chrome-proto` profile）把 load 打到 20+，
  chunk 耗时膨胀 ~10×。发现耗时异常先 `ps aux --sort=-%cpu` 看竞争。
  `pkill -f` 模式串若出现在自身命令行会自杀——用 `pkill -f "chrome[-]proto"` 写法。

### 部署命令（已验证）

```bash
# Box 上
bash scripts/build_release.sh                      # 产出 dist/<name>.tar.gz + stage 目录
X_DATA_MCP_UNSAFE_NONLOCAL_FS=1 python3 scripts/smoke_release.py \
    --bin-dir dist/<name>/bin --report dist/<name>/smoke-report.json
# payload：bin/{data_service,acquire_service,mcp_connect,mcp_data} + tarball + .sha256 + smoke-report.json
bash scripts/publish_dist_ref.sh <tag> <tag_sha> <payload_dir>   # fail-closed，binding 不符会拒

# ECS 上
cd /opt/x-data-mcp-preview && bash deploy_phase_g.sh <tag> --force-git-transport
```

## 5. 驱动脚本（ECS `/tmp/`，重启即丢，必要时从本记录重建）

- `/tmp/fullmarket.py` — daily 采集驱动（53×100 串行，`/tmp/fm_done.json` 断点续跑）
- `/tmp/fullmarket_yearly.py` — yearly 驱动（availability 预检 → 年交集 → update_data，
  `request_id` 前缀 `yearly-p23-`；重跑时换新前缀避免 REQUEST_CONFLICT）
- `/tmp/fetch_bindings.py` — `list_data_versions` 按 100/批拉 daily head 绑定
- 状态文件：`/tmp/fm_{catalog,securities,bindings,avail,done,daily_results,yearly_results}.json`

### MCP 调用要点

- `contract_version` 必填：数据侧 `"v1"`，acquire 侧 `sync_status` 也用 `"v1"`
- `selection` 批次上限 100 实体（`INVALID_MARKET_COMMAND: selection must contain 1..100`）
- `list_data_versions` 不支持 `version` 字段；`retry_task` 需 `item_ids`
- request_id 幂等：重发同 ID 返回历史结果；新尝试必须用新 ID

## 6. 流程与纪律（沿用）

- 改代码前读 `README.md`、`docs/conventions.md`，用 `gh` 核对 issue/分支/提交实际状态
- 提交格式：`<type>(262): <desc>` + Devin trailer（见 git log）
- 证据归档：`evidence/issue-262-mcp-daily-sync/`，JSON 含规模/环境/并发/测量方式/代码版本
- 禁止：shell/SSH/SQLite 旁路（运维操作除外，但不算数据路径）、迁移旧数据、
  凭 fixture smoke 宣称 production ready
- `start_collection` 不接受 `{"kind":"default"}`；default selection 只属于 `update_data`/`backfill_data`
- catalog `start_collection` 参数：`{"dataset":"securities.catalog","selection":{"scope":"current_sh_sz_a"}}`

## 7. 快速恢复上下文

```bash
cd /home/box/x-data-mcp && git log --oneline -10
gh issue view 262
cat evidence/issue-262-mcp-daily-sync/phase-g-preview24-fullmarket.json | python3 -m json.tool
ssh ecs-node 'tail -20 /tmp/fm_yearly.log; df -h /; uptime'
```
