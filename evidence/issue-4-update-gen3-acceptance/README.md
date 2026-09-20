# Issue #4 update 分支——gen-2→gen-3 跨代 `update_data` 变更验收证据

状态：**12/12 PASS**（#228 freeze PLAN，debug build）

## 结果摘要

| 项 | 值 |
|---|---|
| 断言 | **12/12 PASS** |
| update tasks | 53 片全部终态 `failed`（预期：跨代输入下 item 级 fail-closed） |
| items | 5219 全部 `NEEDS_VERIFICATION`，0 成功、0 其他错误形态 |
| 原因分布 | `combined coverage absent` = 5082；`RevisionUnverified` = 137（全部 ∈ S_evt） |
| `NoChange` | **0**——跨代结构不可达（coverage reason 内嵌快照 manifest sha，新代必变） |
| `data_versions` | 5219 → 5219（**零新增发布版本**） |
| `effective_heads` / `daily_bars` | 5219 / 16,629,206 → 逐位不动 |
| `market_update_results` | 5219 行，无本批 update 行写入 |
| 定点 pin | `sz.000001` = `nv:RevisionUnverified`（+1 cat1 事件 @2026-09-24 被检出） |
| worker | 53 次非空闲运行，一次排空 |
| 运行时长 | 03:18:26→03:34:41 UTC ≈ 16min（不构成性能声明） |
| blocked_paths | `S4_default_selection`（5224>100 verbatim 拒收）、`R1_env_absent`（`SOURCE_NOT_VERIFIED` verbatim 拒执） |
| `/gates` | 3 blocker verbatim + `production_ready=false` 一致 |

## 期望集与实测对照

| 集合 | 大小 | 说明 |
|---|---|---|
| `S_evt` | 242 | 逐证券 gbbq 事件集 gen-2↔gen-3 diff（同源 pin 解码器，市场字节 `1→sh / 0→sz`） |
| `S_day` | 9710 | hsjday `.day` 成员级字节 diff（Windows 路径分隔符已规范化） |
| `S_chg` | 9723 | `S_evt ∪ S_day` 与已发布工作集 5219 的差集口径 |

实测：`RevisionUnverified` 137 项全部 ⊆ `S_evt`（`rev_not_in_S_evt=[]`）；`S_evt` 中 70 个成员落入 `combined coverage absent` 分支（覆盖评估先于修订比对失败，属合法 fail-closed 路径）；`illegal_reasons=[]`。

## 关键实测事实

- **跨代变更全部 fail-closed**：gen-3 inspected 快照对每个已发布证券检出输入差异，但 `TdxRangeProvider` 不提供跨代 combined 覆盖证据 → 5082 项 verbatim `NEEDS_VERIFICATION: combined coverage absent`；事件集可判异的 137 项 verbatim `NEEDS_VERIFICATION: RevisionUnverified([dates...])`。**任何 Published 新版本均为非法，实测为零。**
- **`NoChange` 不可达是代码事实而非假设**：coverage evidence reason 绑定快照 manifest sha256，代际切换必然改变元数据，`Decision::Candidate` 缺 combined → `CoverageUnknown`/`combined` 分支。PLAN 已就此作冻结后修正注记。
- **env-absent 拒执**：无 official env 的拷贝库上 `update_data`（request_id `u-r1-gen3`）受理后 worker verbatim `SOURCE_NOT_VERIFIED: NOT_PRODUCTION_READY; publish blocked; GBBQ_QFQ_NOT_VERIFIED (...); FILE_ACCESS_USE_NOT_VERIFIED (...); FILE_SET_CONSISTENCY_NOT_VERIFIED (...)`，零写入。
- **default 预算拒收**：`5224 > 100` verbatim，`not_committed`。

## 边界声明

- 本验收证明"新一代官方字节 → 变更被检出且全部 fail-closed"；**不证明**跨代正向发布——该路径需要独立的 combined 覆盖证据载体（未提供）。
- 三个 blocker（`GBBQ_QFQ_NOT_VERIFIED`/`FILE_ACCESS_USE_NOT_VERIFIED`/`FILE_SET_CONSISTENCY_NOT_VERIFIED`）原样保留，本次验收不削弱任何治理门禁。
- debug build + overlayfs 上 `X_DATA_MCP_UNSAFE_NONLOCAL_FS=1` opt-out 仅用于本地验收，生产禁用（AGENTS.md 口径）。

## 工件

| 文件 | 内容 |
|---|---|
| `PLAN.md` | freeze PLAN（PR #228，`d76bbe8`） |
| `build_gen3_manifest.py` | gen-3 快照 manifest 生成器（retained-inputs pin） |
| `derive_expected_sets.py` | `S_evt`/`S_day`/`S_chg` 推导器 |
| `expected_sets.json` | 期望集全量数据（296KB） |
| `snapshot-manifest-gen3.json` | 安装用快照 manifest |
| `run_updategen3_acceptance.py` | 验收 runner（Box 执行） |
| `run-log.jsonl` | 完整请求/响应/断言留痕（1.9MB） |
| `results.json` | 12 条断言全量结果 + run_identity |
| 运行目录 | `/home/box/acceptance-run-issue4-update-gen3/`（`db.sqlite` 拷贝，源基线 `acceptance-run-issue4-update/` 未触碰） |

## 运行指纹

| 项 | 值 |
|---|---|
| HEAD | `6daa99bd4945a3d6b0568f3d908265555316108f` |
| toolchain | cargo 1.98.1 (rustup stable) |
| 基线 | `/home/box/acceptance-run-issue4-update/db.sqlite`（gen-2 已发布 5219 版本） |
| gen-3 输入 | `retained-inputs/` gen-3 段：`gbbq 502236d9…` / `base 56c35745…` / `hsjday 563797f0…` |
| manifest sha256 | `1e8d1ca77e99efb218213795a1d518f5ce6e4057fa93cf99520429ebb0cd5311` |

## 复现

```bash
# Box 侧，retained-inputs gen-3 已登记
cargo build   # rustup stable 1.98.1
python3 evidence/issue-4-update-gen3-acceptance/derive_expected_sets.py
python3 evidence/issue-4-update-gen3-acceptance/build_gen3_manifest.py
python3 evidence/issue-4-update-gen3-acceptance/run_updategen3_acceptance.py
```
