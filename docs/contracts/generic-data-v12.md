# generic-data-v12 契约（ETF 前复权读面，D-89 / D-92 / D-94）

**状态**：代码面契约，**未部署**（[#607](https://github.com/liqiangcc/x-data-mcp/issues/607) PR-D）。
本文件记录 `market.daily` + ETF + `adjustment=qfq` 从「必然 `adjustment_unavailable`」变成
「按实体核对门槛放行或带原因码阻断」这一 wire 可见的变化（D-70 Q10 判例）。
compact schema 不变（27），工具数与工具签名不变，只改读面的结果与错误信封的 `details`。
设计与裁决见 [issue-607-etf-qfq](../tasks/issue-607-etf-qfq.md) §3、§6，
[decisions](../decisions.md) D-89、D-92、D-93、D-94。

本文件是相对 [generic-data-v11](generic-data-v11.md) 的增量；v11 的读取面、任务面、
错误码与部署约束继续有效。**当前部署仍是 `generic-data-v11`**（preview.46，vm2-126），
部署顺序与部署前的行为见 §7。

## 1. 相对 v11 的增量

| 项 | v11 | v12 |
|---|---|---|
| `capability_revision` | `generic-data-v11` | `generic-data-v12` |
| data 工具数 | 37（ops 16 / research 17 / maint 9） | **不变** |
| compact schema | 27 | **不变**（没有迁移） |
| `market.daily` + ETF + `adjustment=qfq` | 永远 `adjustment_unavailable`（「not delivered」） | 按实体放行或阻断，见 §2/§3 |
| `get_data_capabilities` 的 `series` | 无 ETF `qfq` 行 | **新增一行**：`(market.daily, etf, qfq, 6, yuan, share, volume_quantum=100, default=false)` |
| `get_data_capabilities` 的 `algorithm_versions` | 无此字段 | **新增**（仅 detailed 视图）：`qfq` = `qfq-exact-v1`、`etf_factor_checks` = `etf-factor-checks-v1` |
| 读面 `metadata` | 无 `etf_checks_digest` | 放行的 ETF `qfq` 响应带 `etf_checks_digest`，其余口径不带（字节不变） |
| `adjustment_unavailable` 的 `details` | 只有 `Error::Task` 分支填 `details` | ETF `qfq` 阻断填 `details.reason` / `blocking_events` / `categories` |
| A 股 `qfq`、ETF `raw`、`market.index` | | **逐字节不变**（§6） |

## 2. ETF 前复权读面

```text
market.daily + ETF + adjustment=qfq + 该实体通过核对
  → rows：price_scale=6（1e-6 元，尾随零不补，如 4.0449），
    volume/amount 与不复权逐字节相同（量额不参与复权，单位仍是 份 / 元），
    metadata.etf_checks_digest = 判定所用核对记录集合的 sha256
market.daily + ETF + adjustment=qfq + 存在阻断事件
  → error { code: "adjustment_unavailable",
            message: "adjustment_unavailable: ETF sh.510050 has no verified 前复权 series",
            details: { reason: "etf_factor_unverified", blocking_events: N,
                       categories: [1, 11] } }
market.daily + ETF + 无 adjustment
  → 仍是 error { code: "adjustment_required_for_etf" }（D-70 Q4 不变）
market.daily + ETF + adjustment=raw
  → 不变（price_scale=3）
```

- **阻断是实体级的**（D-89 第 6 条、设计 §3.3 (a)）：一个实体的任何一个参与序列的事件未通过核对，
  整个实体的 `qfq` 都阻断，不返回截断或部分序列。阻断时不返回空页——空页会被回测读成
  「这只 ETF 那天没有数据」。
- **`versions` / `availability` 面同样报同一个错**：三条路径共用 `resolve_read_shape`
  （`src/sqlite_market/series.rs`），阻断在形状解析处生效，不只在读行时生效。
- **`get_data_capabilities` 照实列出 ETF `qfq` 能力**：阻断是实体级事实，不是数据集级能力缺失
  （设计 §6.3）。`default=false`：ETF 仍要求显式口径，缺省不取 `qfq`。
- **`details` 只给条数，不给事件明细**：明细走研究导出或运维查询，可变长数组不进错误信封。
- **导出面（`--adjustment qfq`）不在本修订内**：`research` 导出对 ETF 仍不交付前复权，
  点名 ETF 的 `qfq`-only 导出仍报 `adjustment_unavailable`。

## 3. 阻断原因码（`details.reason`）

| 原因码 | 含义 | 设计条目 |
|---|---|---|
| `etf_factor_unverified` | 有事件未通过核对（既核对不了官方 `qss`，也核对不了公告）；也用于「来源库未配置/打不开」的 fail-closed | B1/B2/B6，§3.2.2 |
| `etf_event_order_unverified` | 同一除权除息日上 cat1 + cat11 的合并次序未锚定 | B4 |
| `etf_event_category_unexpected` | 出现 1/11 之外的类别（DDL 已挡，防御性检查） | B3 |
| `etf_factor_unparsable` | 因子不可解析成精确分数（非有限、位数超预算、cat11 的 `c3=0`） | B5 |
| `etf_qfq_not_built` | 该 ETF 还没有构建过前复权序列（新上市/尚未派生） | §3.2 |

同一实体同时满足多个条件时按上表的固定优先级取一个原因码（`etf_event_category_unexpected`
> `etf_factor_unparsable` > `etf_event_order_unverified` > `etf_factor_unverified` > `etf_qfq_not_built`），
同一输入两次运行的结论一致。

## 4. 减法前复权的 ≤0 限制（D-94，**A 股与 ETF 都适用**）

**减法前复权保留的是价差，不是比例。** 早期价格减去累计现金分红后**可能 ≤0**：

- ETF：`sh.510050`、`sh.510880` 在生产数据里有这种 bar（最低约 −0.286 元）。
- A 股：同一算法一直有这个问题，`#621` 记录 **747 只 / 777,958 根 bar** 落在 ≤0。

**这类价格照原值返回**：不阻断、不截断、不改成 0、不换算法、不新增原因码。它是**说明性限制，
不是交付门槛**。因此：

- **不能直接用前复权序列算收益率/对数收益**：比例关系在 ≤0 的 bar 上不成立（负价、跨 0 的比值无意义）。
- 需要收益率时用**不复权价 + 因子**自行还原，或把窗口限制在第一个 ≤0 的 bar 之后。
- 这一点对 A 股 `qfq`（`qfq-exact-v1`）同样成立，v11 及更早的契约没有写清楚，v12 起写明。

## 5. 读时判定与复现（设计 §3.2.2）

- **判定在每次读取时做**：读面把该实体当前存储的因子事实（`adjustment_events` 的
  `evidence_digest`）与不复权首根 bar 交给门槛，不与任何启动快照比对。日更写进来的新事件
  因此**在下一次读取就阻断**，不需要额外的同步步骤。
- **缓存按 `(entity, 因子摘要集合)`**：因子变化自然换 key，不存在需要主动失效的缓存。
- **新核对记录不需要重启**：每次读取做一次廉价探针（来源库 `manifest.jsonl` 的 `(长度, mtime)`），
  只有它变了才扫描清单算出核对集合摘要（不读 blob）；摘要与在内存里的不同时才做一次完整校验读。
  代价：每个 ETF `qfq` 请求一次 `stat`，来源库变动时一次清单扫描，核对集合变动时一次完整读。
- **来源库未配置/打不开 → 全部 ETF `qfq` fail-closed**：原因写进 message，`details.reason` 为
  `etf_factor_unverified`。没有「没有来源库就当作已核对」的路径。
- **复现**：放行的响应带 `etf_checks_digest`（核对记录集合的 sha256，与 `etf-factor-checks-v1`
  解析器版本一起使用）。相同输入 + 相同核对集合 = 相同结论。

## 6. 不变的（逐字节）

| 面 | 证据 |
|---|---|
| A 股 `qfq`（`qfq-exact-v1`，6dp） | 既有 golden 快照 |
| ETF `raw`（3dp，`volume_quantum=100`） | `#607` PR-C 的等价性测试（`a_stored_etf_forward_series_changes_no_read_or_maintenance_surface`）扩展为 gate-closed 的 `qfq` 之后，除 `qfq` 之外的每个面逐字节相同 |
| `market.index`（2dp，point / 手） | 同上 |
| 工具签名、`tools/list`、工具数 | 本修订只改读面结果与 `details` |

## 7. 部署顺序与部署前的影响

1. **先部署二进制**（不导入核对记录）。部署后所有 ETF `qfq` 一律
   `adjustment_unavailable` + `details.reason = etf_factor_unverified`
   （来源库未配置时 message 里写明 `X_DATA_MCP_SOURCE_STORE`），**这是可接受的状态**：
   与 v11 的「必然 `adjustment_unavailable`」对客户端是同一个错误码，只是多了 `details`。
2. **owner 批准并导入核对记录**（`source_store import --format etf-factor-checks-jsonl|csv`）。
   导入后放行**不需要重启**（§5）。
3. 部署动作本身不改变 `schema`（仍是 27），只换二进制。

## 8. 未验证

- 未在 vm2-126 上部署，也没有在真实生产库上跑过放行/阻断（本地夹具与单元测试见 PR 说明）。
- 核对记录的导入不在本修订内：首次导入是 `#607` convert v2 产出的 **1,233 条、覆盖 337 只**
  （D-92 之后阻断 0，设计 §9.3、§9.5），需要 owner 批准。导入多少就放行多少，
  未导入的实体保持 `etf_factor_unverified`。
- ETF 前复权的体积按 PR-C 写库基准实测 **41.8 B/bar**，按生产 1,645,415 根 bar 估算库增长约
  **69–96 MB**（设计 §9.5）；**部署后**的实际体积与 write-loop 增量仍待采集。
