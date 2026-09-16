# Issue #3 真实快照端到端验收运行记录（main `6c395d6`）

2026-09-16，对 retained-inputs gen-2 官方快照通过真实 `mcp_data` stdio 服务入口
执行 `start_collection`/`market.daily`/`v1` 采集并复核发布结果。**这是本机
验收证据运行，不是生产部署**：不接 ecs、无调度器、不改任何 gate。

## 运行身份

| 项 | 值 |
|---|---|
| 代码 | `6c395d68012628fcc0a6ecee8a91588048dc001d`（`mcp_data --version` 报告 `x-data-mcp 0.2.0-preview.11 6c395d6… NOT_PRODUCTION_READY`） |
| 构建 | `cargo build --locked --bins`（rustup stable 1.98.1，debug profile） |
| 时间 | 2026-09-16T06:20:40Z – 06:25:22Z（UTC，见 `run-log.jsonl` 首末行） |
| 服务入口 | `mcp_data <initialized-database-path>`（JSON-RPC stdio，`DataApi::new` env 安装钩子） |
| 环境变量 | `X_DATA_MCP_OFFICIAL_MANIFEST=evidence/issue-3-acceptance-run/snapshot-manifest.json`、`X_DATA_MCP_OFFICIAL_DIR=/home/box/acceptance-run-issue3/snapshot` |
| 库 | `task_sqlite init /home/box/acceptance-run-issue3/acceptance.sqlite` 新建（schema 12） |
| 快照目录 | `/home/box/acceptance-run-issue3/snapshot/`：retained-inputs 三 zip 的硬链接（同 overlayfs），原始字节不入库 |
| 调度 | 一次性手工 `run_worker_once` 驱动，无守护/无定时器 |

## 输入绑定

`snapshot-manifest.json`（本目录，**非** retained-inputs）声明 gen-2 三角色的
sha256/字节数/官方 URL/获取时间（2026-09-15 下载，时间取 MANIFEST.md 记录的
文件 mtime；单次下载未逐文件记录完成时刻，如实标注）。`inspect_snapshot`
运行结果：`byte_integrity=FILE_BYTES_VERIFIED`，`manifest_sha256=
b8a0d7b9aec1a270549d1e53ba26c0ab04842a23c05d1d1f7e75dd44a9f444d3`；
`declared_origin/archive_member_binding/file_set_consistency/
business_semantics/access_use` 全部 `NOT_VERIFIED`，`production_ready=false`。

## 执行序列（`run-log.jsonl` 逐条请求/响应留痕）

1. `initialize` → 服务就绪（协议 `2025-06-18`）
2. `start_collection` market.daily v1，explicit selection =
   sz.000002 + sh.600006 + sh.600004 → `task-5784bbb178631af9084eb6cb1d8aab08`，
   三 item 受理（`readiness=blocked` 如实伴随受理）
3. `run_worker_once` → `status=succeeded`，三 item 全部 `success:true`
4. `query_data` 六条冻结 oracle bar 服务侧读回
5. `update_data` market.yearly（base=000002 已发布 head）→ 受理后 worker
   执行 → item `success:false`，门禁拒绝原文留痕
6. 服务退出后直接读 SQLite 做机械复核（不经过服务层）

## 发布结果

| 证券 | version_id | bars | 首日 | 末日 |
|---|---|---|---|---|
| sz.000002 | `collection-version-2990ce6a…be6` | 8479 | 1991-01-29 | 2026-09-14 |
| sh.600006 | `collection-version-eb79c6b9…317b` | 6467 | 1999-07-27 | 2026-09-14 |
| sh.600004 | `collection-version-c0a0a182…f2c9` | 5630 | 2003-04-28 | 2026-09-14 |

publication receipt（`publication_batches`）：`collection-result-2990ce…` /
`collection-result-eb79c6…` / `collection-result-c0a0a1…` 各绑定 item
`["market","<sym>","day"]`。`effective_heads` 三行指向上述版本。

## 验收断言（`results.json`：23/23 PASS）

- 发布 bar 数 = `data_versions.record_count` = `daily_bars` 行数；首末日期一致。
- Oracle bar（`#54` 契约回归 `client_oracle_bars` 冻结值，服务读回+库内复核双路）：
  - sz.000002 2000-01-10：`O=-8.14 H=-8.06 L=-8.15 C=-8.06`（24 事件仿射链点）
  - sz.000002 1999-12-29：`O=-8.28 H=-8.27 L=-8.29 C=-8.27`（25 事件）
  - sh.600006 2025-07-16：`C=7.25`（半分边界 7.245→ROUND_HALF_UP→7.25）
  - sh.600006 2025-07-17 事件日：`O=7.24 C=7.32`
  - sh.600004 2026-07-23：`C=7.68`；2026-07-24 事件日：`O=7.67 C=7.67`
- `provenance.snapshot_inputs`：每版本 2 条（day member + gbbq member），
  `archive_sha256` 与 retained 文件实测 sha256 逐条一致；`member_sha256`
  64-hex 在位（gbbq member `4a3e9b86…9d77` 与 MANIFEST.md 登记的
  `official-gbbq-member` pin 一致）。
- `daily_bars.volume`/`amount` 全 NULL —— `.day` 量额字段未提升（单位语义未锚定）。
- yearly 派生可计算性：从已发布 daily 机械聚合自然年 OHLC 可导出（000002
  1991–2026、600006 1999–2026、600004 2003–2026），**该聚合只是复核计算，
  未产生 yearly 发布**。

## 门禁与未验证项（如实记录）

- 服务级 `update_data`/`market.yearly` item 拒绝原文：
  `BASE_UNAVAILABLE: SOURCE_NOT_VERIFIED: NOT_PRODUCTION_READY; publish
  blocked; CATALOG_PUBLISH_NOT_VERIFIED …; GBBQ_QFQ_NOT_VERIFIED …;
  FILE_ACCESS_USE_NOT_VERIFIED …; FILE_SET_CONSISTENCY_NOT_VERIFIED …;
  LOCAL_PIPELINE_NOT_IMPLEMENTED (validated parsers, QFQ and
  fixed-published-daily yearly)`。yearly 服务发布仍被 `OfficialYearlyVerifier`
  未实现 + 全部门禁拒绝，无 derived yearly 版本入库。
- `PRODUCTION_BLOCKERS` 五条原样未解除；受理与版本 provenance note 均携带
  `NOT_PRODUCTION_READY`。
- **C3 caveat**：C3=每10股送转数为链约束间接角色（#169 冻结契约标注
  indirect），C3≠0 事件 24/94；本次发布沿用该角色不另作主张。
- 非 cat-1 GBBQ 排除计数（provenance note 逐版本记录）：sz.000002=76、
  sh.600006=5、sh.600004=18，全部"excluded uninterpreted"。
- 快照 inspect 的 origin 认证、成员绑定、文件集一致性、业务语义、访问使用
  边界全部 `NOT_VERIFIED`；coverage=Complete 仅表示该快照内 .day 成员自身
  连续区间，不声称上市完整历史。
- 调度为一次性手工 `run_worker_once`；`mcp_data` 无长驻、无自动重试。

## 复现

```bash
cargo build --locked --bins
mkdir -p /home/box/acceptance-run-issue3/snapshot
for f in base gbbq hsjday; do
  ln -f /home/box/retained-inputs/official-$f.zip \
        /home/box/acceptance-run-issue3/snapshot/$f.zip
done
python3 evidence/issue-3-acceptance-run/run_acceptance.py
```

脚本自行 `task_sqlite init`、起 `mcp_data`（注入两 env）、跑完整序列、机械
复核并写 `run-log.jsonl` + `results.json`。retained-inputs 缺席时前置断言
失败退出，不降级。
