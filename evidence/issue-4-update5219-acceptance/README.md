# Issue #4 update 分支——5219 证券同代 `update_data` 无变更验收证据

状态：**12/12 PASS**（freeze v1 修正版断言，release build）

## 结果摘要

| 项 | 值 |
|---|---|
| 断言 | **12/12 PASS** |
| update tasks | 53 片全部 succeeded |
| items | 5219 全部 success，0 失败 |
| `market_update_results` | 5219 行，全部 `no_change=1`,`result_version_id == base_version_id` |
| `data_versions` | 5219 → 5219（零新增版本） |
| `effective_heads` | 逐 symbol 与封存 v2b 基线完全一致 |
| `daily_bars` | 16,629,206 → 16,629,206（不动） |
| 抽样 3 证券逐 bar | 与基线逐行一致 |
| audit provenance | 5219 行全部绑 `base_version_id` + inspected manifest sha + gbbq member sha;`combined=null`,`revision_ref=null` |
| worker | 53 次非空闲运行，一次排空 |
| 运行时长 | ~30min(release；不构成性能声明） |
| blocked_paths | `S3_default_selection`（预算 verbatim 拒收）、`R1_env_absent`（源门禁 verbatim 拒执） |

## 关键实测事实

- **default 选择正确拒收**:`update_data {"selection":{"kind":"default"}}` → verbatim `default security budget exceeded: 5224 > 100`,`not_committed`。default 工作集按全部 5224 个已发布目录证券计数（预算守卫先于逐 item head 绑定）；显式 items 工作集 = 5219 个有 head 证券。两层口径不同，如实记录。
- **同代无变更语义成立**：同一 inspected gen-2 快照重采全部 5219 证券 → 全部 `Decision::NoChange` → 复用 base version，零新增 `data_versions`,`effective_heads`/`daily_bars` 逐位不动。
- **env-absent 拒执**：同一拷贝库无 official env → update item 受理后 worker verbatim `SOURCE_NOT_VERIFIED: GBBQ_QFQ_NOT_VERIFIED (...); FILE_ACCESS_USE_NOT_VERIFIED (...); FILE_SET_CONSISTENCY_NOT_VERIFIED (...)` 拒执，无任何写入。
- **治理门禁不变**:3 个 blocker verbatim + `production_ready=false` 在所有受理响应中一致出现。

## 边界声明

- 本验收证明"相同快照重采 → 无变更/复用语义";**不证明**"新一代快照 → 正确检测变更"——后者依赖二代授权快照（既定 blocker 路径）。
- release build 为验收窗口内执行选择，非性能目标声明（#204 口径不变）。
- `NOT_PRODUCTION_READY` 与 3 个 blocker 原样保留。

## 工件

| 文件 | 内容 |
|---|---|
| `PLAN.md` | freeze v1（含执行中修正注记） |
| `results.json` | 12 条断言全量结果 |
| `run-log.jsonl` | 完整请求/响应/断言留痕 |
| `run-log-aborted-v1.jsonl` | 首跑中止留痕（U2 冻结 verbatim 与实测 5224>100 偏差，修正后重跑） |
| 运行目录 | `/home/box/acceptance-run-issue4-update/`(`db.sqlite` 拷贝 + `baseline.json` + `shards.json`)；封存源 `acceptance-run-issue4/` 未触碰 |

## 复现

```bash
cargo build --release   # rustup stable 1.98.1
python3 evidence/issue-4-update5219-acceptance/run_update5219_acceptance.py
```
