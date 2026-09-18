# Issue #4 update 分支——5219 证券同代 `update_data` 无变更验收计划（freeze v1）

状态：**已冻结**（冻结即机械期望值起算；一次中断 → 回 PLAN 记录 → 重跑）

## 0. 范围（bounded）

| 项 | 值 |
|---|---|
| 数据集 | `market.daily` |
| 工作集 | **gen-2 首采验收已发布 head 的全部证券**（预期 5219；以 `/home/box/acceptance-run-issue4/db.sqlite` 机械查询为准） |
| 操作 | `update_data`（`operation=update`，显式 items 分片 1..100 预算内） |
| 数据源 | `X_DATA_MCP_OFFICIAL_MANIFEST` + `X_DATA_MCP_OFFICIAL_DIR=/home/box/acceptance-run-issue3/snapshot`（**同一 inspected gen-2 快照**，与首采验收同一 env 机制） |
| 基座 | `db.sqlite` 拷贝（`acceptance-run-issue4/db.sqlite` → 新 scratch），**不改已封存验收工件** |
| 二进制 | `target/release/mcp_data`（release build——见 §3.1，动机=验收窗口，不构成性能声明） |
| 输入完整性 | gen-2 快照 hash 经 `official_source inspect` 实测重确认 |
| 模式 | MCP 常驻进程 + `run_worker_once` 排空循环 |
| 切片 | ~53 个 `update_data` 请求（53×100 覆盖 5219；末片 19） |
| 裁决类 | **验收裁决：代码 + 数据 + 证据固化为一个 PR** |

## 1. 基线事实（run 前机械重算，与冻结值核对）

- 机械查询输入 DB：`COUNT(data_versions WHERE dataset='market.daily')`（预期 5219）、`COUNT(DISTINCT symbol)`、`daily_bars` 行数（预期 16629206）、全部 `(symbol → head version_id)` 映射。
- gen-2 快照 hash 机械重算：`{data_root}/snapshots/20260513-095450/sha256` 逐文件 + `manifest_sha256`。
- **首个事实**：`{"selection":{"kind":"default"}` 的 `update_data` 在本基线必然 verbatim 拒收（实测：`default security budget exceeded: 5224 > 100`）——default 工作集按**全部 5224 个已发布目录证券**计数，预算检查在逐 item head 绑定之前触发；这与"显式 items 工作集 = 5219 个有 head 证券"是两层不同口径，非缺陷。这是预设预算的正确拒绝，记入 `blocked_paths`。
- 工作集精确性：update 显式 items 的 `base_version_id` 由各证券 head `version_id` 提供；`start_date = base.first_date`，`end_date = 运行日`（上海历，`shanghai_today` 同服务侧）；该 range 覆盖该证券全部已发布历史 → 同代快照下物化结果与 base 完全一致。
- 5 只 `.day` 缺席成员（sh.601091、sh.688837、sz.001246、sz.301686、sz.301716）无已发布 head，**不属于 update 工作集**——这是 contract 性排除（无 base_version_id 可绑），不作为失败。

## 2. 断言表（每条独立可复核）

| # | 断言 | 判定方式 |
|---|---|---|
| U1 | 输入完整性 | 快照逐文件 sha256 + manifest_sha256 == gen-2 冻结值 |
| U2 | default 预算拒收 | `update_data {"selection":{"kind":"default"}}` → verbatim `default security budget exceeded: 5224 > 100`；not_committed |
| U3 | 分片受理 | 53 片全部 accepted；frozen items 合计 == 5219；`base_version_id`/`range` 冻结进 task evidence |
| U4 | 逐 item 无变更完成 | 5219 items 全部 item_state=success；产出 `market_update_results` 行 == 5219 |
| U5 | no_change 语义 | 全部 `market_update_results.no_change=1` 且 `result_version_id == base_version_id`（机械查询，不允许 truthy 伪造） |
| U6 | 零新增版本 | `data_versions` 行数不变（5219 → 5219）；`effective_heads` 映射逐 symbol 不变 |
| U7 | 数据不动 | `daily_bars` 行数不变（16629206）；抽样 3 证券（sh.600004/sh.600006/sz.000002）逐 bar 内容不变 |
| U8 | provenance | 全部 update results `audit_json` 绑 base version_id + 快照 manifest + 成员 digest（抽查 verbatim） |
| U9 | task 簿记 | update tasks 全部 succeeded；item evidence 可追溯（spot-check verbatim） |
| U10 | env-absent | 同一拷贝库 + 无 official env：`update_data` 显式 items 受理后 worker 拒执 → verbatim `SOURCE_NOT_VERIFIED: GBBQ_QFQ_NOT_VERIFIED (...); FILE_ACCESS_USE_NOT_VERIFIED (...); FILE_SET_CONSISTENCY_NOT_VERIFIED (...)`（与首采 D10 同一 verbatim 形态；env 缺席先撞源门禁） |
| U11 | 治理状态 | `GET /gates` blocked_paths verbatim：`GBBQ_QFQ_NOT_VERIFIED` + `FILE_ACCESS_USE_NOT_VERIFIED` + `FILE_SET_CONSISTENCY_NOT_VERIFIED`；`status=NOT_PRODUCTION_READY` |
| U12 | 断点续跑 | （若发生中断）重跑同请求 → 幂等续跑无重复写入（`data_versions`/`market_update_results` 计数不变） |

**机械核对**：所有计数/verbatim 断言由脚本直接 SQL/API 实测，不接受采样/推断。

## 3. 执行序列

| 步 | 动作 | 判定 |
|---|---|---|
| S0 | 环境指纹：HEAD sha + `cargo --version` + `sha256sum target/release/mcp_data` + 快照 manifest_sha256 → `evidence.json` | 记录 |
| S1 | 拷贝 `acceptance-run-issue4/db.sqlite` → `acceptance-run-issue4-update/db.sqlite`；机械基线查询（版本数/heads 映射/bars 计数） → `baseline.json` | 记录 |
| S2 | `official_source inspect`（env on）→ 基线 manifest/逐文件 hash | U1 |
| S3 | `update_data {"selection":{"kind":"default"}}` → verbatim 拒绝 → `blocked_paths` | U2 |
| S4 | 基线查询生成 53 片 items（`{symbol,base_version_id,start_date,end_date}`，按 symbol 排序稳定切分）→ `shards.json` | U3 输入固化 |
| S5 | 逐片 `update_data` → 受理证据收集；`run_worker_once` 排空循环至 `report[0]=="IDLE"` | U3 |
| S6 | 断言 U4–U9 全查 | U4–U9 |
| S7 | 同一拷贝库上重开无 official env 的服务、显式 items `update_data` → 受理后 worker verbatim 拒执 | U10 |
| S8 | `GET /gates` | U11 |
| S9 | `results.json` 落盘；中断情形走 PLAN v1 中断注记路径 | U12 |

## 4. 风险与边界

- **预期单变量**：同代快照下唯一变量是"重复采集同一数据"→ 合法结果只有 NoChange；任何 `Candidate`/`NEEDS_VERIFICATION` 均为异常，脚本记 fail 不豁免。
- **同代 ≠ 跨代证明**：本次验收证明的是"相同快照重采 → 无变更语义"，**不证明**"新一代快照 → 正确检测变更"——后者依赖二代授权快照（既定 blocker）。
- **duration 声明**：release 构建为验收窗口内可执行选择；本计划不构成性能目标声明（#204 既有注记口径不变）。
- **预算语义**：53 片显式 items 是预算内的合法分片；`{"kind":"default"}` 的正确拒收（U2）是本验收的一部分，不是绕过手段。
- **DB 拷贝**：在 v2b 已封存工件副本上运行；原 `acceptance-run-issue4/` 工件不触碰。
- **非目标**：并行 worker、task 级事务、跨代变更检测、yearly/backfill 路径——各自独立切片。
