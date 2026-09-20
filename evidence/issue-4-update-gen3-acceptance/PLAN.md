# Issue #4 update 分支——changed-generation `update_data` 验收计划（freeze v1）

状态：**已冻结**（冻结即机械期望集起算方法固定；一次中断 → 回 PLAN 记录 → 重跑）

父 Issue：#4；输入：#217 gen-3 采集（PR #227，`12c8aea`）；前序：#207/#208/#209 同代无变更验收（12/12 PASS，`63490c0`）。

## 0. 范围（bounded）

| 项 | 值 |
|---|---|
| 数据集 | `market.daily` |
| 工作集 | gen-2 首采验收已发布 head 的全部证券（预期 5219；以封存 `acceptance-run-issue4/db.sqlite` 机械查询为准；与 update5219 同一基线库） |
| 操作 | `update_data`（`operation=update`，显式 items 分片 ≤100 预算内） |
| 数据源 | gen-3 inspected 快照：`X_DATA_MCP_OFFICIAL_MANIFEST` + `X_DATA_MCP_OFFICIAL_DIR` 指向 gen-3 快照根（§S2 构建） |
| 基座 | `acceptance-run-issue4/db.sqlite` 拷贝 → 新 scratch，不触碰已封存工件 |
| 二进制 | `target/release/mcp_data`（release build，动机=验收窗口，不构成性能声明） |
| 执行环境 | 需同时具备：gen-2 已发布基线库、gen-2 retained 字节（期望集推导）、gen-3 三文件字节。本 WSL2 机缺 gen-2 retained——实际执行在具备上述工件的环境进行（gen-3 字节 sha256 已冻结于 `acquisition.json`，跨机搬运后按 pin 复验） |
| 裁决类 | 验收裁决：代码 + 数据 + 证据固化为一个 PR |

## 1. gen-3 冻结输入事实（验收前机械复核）

| role | gen-3 sha256 | bytes | Last-Modified |
|---|---|---|---|
| gbbq.zip | `502236d9d8394e89b1a4091f5d0d2b86e419c0c4da2c788ddcea16ef7d3763e3` | 6,337,960 | Fri, 18 Sep 2026 10:14:43 GMT |
| hsjday.zip | `563797f0d1b852e91b85ee0aafe396c0df1bb12e55b569e8526db76a4e181c64` | 550,106,783 | Fri, 18 Sep 2026 07:58:50 GMT |
| base.zip | `56c3574505bbd3fa3a1899b46e09112070dea17cbea2f713b7ccff2628379bec` | 15,963,181 | Fri, 18 Sep 2026 12:59:10 GMT |

- gen-3 gbbq member 206,034 条 vs gen-2 205,776（**+258**）；冻结集唯一新增事件 `sz.000001 @2026-09-24 cat1`（TQ oracle `bonus=2.49` 交叉验证一致）；无删除、无冻结集字段变更（`gen2-vs-gen3-frozendiff.json`）。
- 采集事实（URL/UTC 区间/HTTP 头/逐尝试记录）verbatim 于 `evidence/issue-217-gen3-acquisition/acquisition.json`。
- `FILE_SET_CONSISTENCY`：无集合身份/修订可取得 → **NOT_VERIFIED**（#217 S3，诚实记录，不推断）。`inspect()` 只验证字节完整性，此状态不阻塞安装；它在 `/gates` 继续呈现为 blocked_path。
- #71 边界：**NOT_OBTAINED**，本验收不构成任何使用授权声明。

## 2. 决策语义冻结（代码事实，先于运行）

`update_data` 走 `TdxRangeProvider`（`src/official_market.rs`）：

- `combined=None` 且 `revision_ref=None`——单代快照无独立全区间覆盖证据；
- `prepare()`（`src/market_update.rs`）：重叠日期值修订且无 revision_ref → `NeedsVerification(RevisionUnverified)`；纯新增日期 → `Candidate` → 发布侧见 `combined=None` → `NeedsVerification(CoverageUnknown)`；物化内容逐 bar 相同 → `NoChange`；
- 推论（冻结断言方向）：**gen-3 跨代 update 下没有任何 item 能 publish 新版本**——出现任何 `Published`（新 version_id）即 FAIL；合法结局空间 = `{NoChange, NeedsVerification}`，且必须与实测物化差异精确相关。

## 3. 期望集机械推导（S3 产物，`expected_sets.json`）

对 5219 工作集逐证券机械计算，不接受推断：

- `S_evt`：gen-2/gen-3 两个 gbbq member 经同一固定解码器（community decoder，source sha256 `e28fae76…`，与 frozendiff 同源同 pin）解码 → 逐证券事件集 diff；事件集有差异的证券入 `S_evt`；
- `S_day`：gen-2/gen-3 两个 hsjday.zip 成员级对比（成员名集合 + 每成员字节 sha256）；`.day` 成员字节不同的证券入 `S_day`；
- `S_chg = S_evt ∪ S_day`；两个输入文件 sha256 一并记入产物。

判定不变式（逐 item，机械核对）：

- item ∉ `S_chg` → **必须 `NoChange`**（`result_version_id == base_version_id`）；
- item ∈ `S_chg` → **禁止 `Published`**；结局为 `needs_verification` 或 `no_change`（后者仅在 bar 级物化差异实际为零时合法，member 字节差异本身不构成内容差异证据）；
- bar 级细分（以该 item 实际物化为准）：重叠日期存在值修订 → reason `RevisionUnverified`；仅新增日期 → reason `CoverageUnknown`；实测差异为零 → `NoChange`；
- 定点断言：`sz.000001` 必须 `needs_verification` 且 reason 族为 `RevisionUnverified`（已知 +1 cat1 事件 → QFQ 基准位移 → 历史 bar 大面积 revised）。

## 4. 断言表（每条独立可复核）

| # | 断言 | 判定方式 |
|---|---|---|
| C1 | gen-3 输入完整性 | 快照根三文件 sha256 + bytes == 上表冻结值；`inspect()` 通过，`byte_integrity=FILE_BYTES_VERIFIED`，`file_set_consistency=NOT_VERIFIED` |
| C2 | default 预算拒收 | `update_data {"selection":{"kind":"default"}}` → verbatim `default security budget exceeded: 5224 > 100`；not_committed |
| C3 | 分片受理 | 53 片全部 accepted；items 合计 == 5219；`base_version_id`/`range` 冻结进 task evidence |
| C4 | 逐 item 结局合法 | 5219 items 结局 ∈ `{success(NoChange), needs_verification}`；**零 Published 新版本**：`data_versions` 计数 5219 → 5219，`effective_heads` 逐 symbol 不变 |
| C5 | 结局-期望集一致 | §3 三条不变式逐 item 机械核对；不一致逐项列出 verbatim |
| C6 | sz.000001 定点 | `needs_verification` + `RevisionUnverified`，revised 日期列表非空且 verbatim 留存 |
| C7 | NoChange 内容不动 | `daily_bars` 行数不变（16,629,206）；NoChange items 抽样逐 bar 内容不变 |
| C8 | provenance | needs_verification/result 行 `audit_json` 绑 base version_id + gen-3 manifest sha256 + gbbq member digest（抽查 verbatim） |
| C9 | task 簿记 | update tasks 终态合法（succeeded 仅 NoChange；其余 needs_verification）；item evidence 可追溯 |
| C10 | env-absent | 同一拷贝库 + 无 official env：显式 items 受理后 worker verbatim 拒执 `SOURCE_NOT_VERIFIED: GBBQ_QFQ_NOT_VERIFIED (...); FILE_ACCESS_USE_NOT_VERIFIED (...); FILE_SET_CONSISTENCY_NOT_VERIFIED (...)` |
| C11 | 治理状态 | `GET /gates` blocked_paths verbatim：`GBBQ_QFQ_NOT_VERIFIED` + `FILE_ACCESS_USE_NOT_VERIFIED` + `FILE_SET_CONSISTENCY_NOT_VERIFIED`；`status=NOT_PRODUCTION_READY` |
| C12 | 断点续跑 | （若发生中断）重跑同请求 → 幂等续跑，无重复写入 |

**机械核对**：所有计数/verbatim 断言由脚本直接 SQL/API 实测，不接受采样/推断。

## 5. 执行序列

| 步 | 动作 | 判定 |
|---|---|---|
| S0 | 环境指纹：HEAD sha + `cargo --version` + `sha256sum target/release/mcp_data` + gen-3 manifest_sha256 → `evidence.json` | 记录 |
| S1 | 拷贝基线库 → scratch；机械基线查询（版本数/heads 映射/bars 计数）→ `baseline.json` | 记录 |
| S2 | 构建 gen-3 快照根：三 zip 就位（跨机搬运后按冻结 sha256 复验）+ 由 `acquisition.json` 事实生成 `x-data-mcp.official-file-input.v1` manifest → `official_source inspect` | C1 |
| S3 | 期望集推导（§3）→ `expected_sets.json` | C5 输入固化 |
| S4 | `update_data {"selection":{"kind":"default"}}` → verbatim 拒绝 | C2 |
| S5 | 基线查询生成 53 片 items（与 update5219 同一切分规则）→ `shards.json` | C3 输入固化 |
| S6 | 逐片 `update_data` → 受理证据收集；`run_worker_once` 排空至 `IDLE` | C3 |
| S7 | 断言 C4–C9 全查 | C4–C9 |
| S8 | 无 official env 重开服务，显式 items 受理后 worker verbatim 拒执 | C10 |
| S9 | `GET /gates`；`results.json` 落盘；中断走中断注记路径 | C11/C12 |

## 6. 风险与边界

- **本验收证明"变更被诚实检测且 fail-closed"**：变更项 → NEEDS_VERIFICATION（理由分类正确），未变项 → NoChange，零静默发布。**不证明**跨代变更数据可发布——`combined=None` 下发布被显式拒绝是设计事实；变更世代的可发布能力（combined 覆盖证据/修订授权路径）是独立决策项，不在本验收范围。
- **同代 ≠ 跨代**：#209 证明同代重采 → NoChange；本验收证明跨代 → 变更检测 + fail-closed。两者互补，不互相替代。
- **理由分类不外推**：`RevisionUnverified`/`CoverageUnknown` 的分布是实测产物；计划只冻结判定规则与定点断言，不预设计数。
- **duration**：5219 items 全历史物化，工作量大于同代验收；release 构建为窗口内可执行选择，不构成性能目标声明。
- **非目标**：backfill/yearly/catalog 路径、变更数据发布能力、FILE_SET_CONSISTENCY 证据构造（#217 S3 已 NOT_VERIFIED 终态）、#54 候选判别（独立链）。
