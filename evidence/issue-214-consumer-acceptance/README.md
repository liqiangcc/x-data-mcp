# Issue #214 消费侧读路径验收——执行证据

## 结论

**13/13 断言 PASS**(results.json `all_pass=true`)。消费面在已发布真实数据集上按契约正确：分页完整、版本绑定严格、cursor 纪律、可用性诚实、计算固定绑定拒绝、任务内省一致、全程只读无副作用、边界显式拒绝。

## 验收对象

| 项 | 值 |
|---|---|
| 服务 | `target/release/data_service` @ `f77d7d5`(main,含 #213 开发豁免） |
| 数据 | `/home/box/acceptance-run-issue4-update/db.sqlite`(update5219 验收库，~9.0G，只读不拷贝） |
| 传输 | unix socket `/home/box/preview-deploy/run-realdata/mcp.sock` + `mcp_connect`，双方 `X_DATA_MCP_UNSAFE_NONLOCAL_FS=1` |
| 形态 | 本机开发形态（overlayfs + opt-out)——**非生产部署形态** |

## 断言结果

| # | 断言 | 结果 |
|---|---|---|
| C1 | 服务绑定（0.2.0-preview.12 / 19 工具 / production_ready=false) | PASS |
| C2 | 能力发现（generic-data-v1，写操作 blocked + 三 blocker verbatim) | PASS |
| C3 | 三证券全量分页（8479/6467/5630 条，升序无重复，首末+锚点逐值命中 D8 冻结值，version_ref 逐条绑定） | PASS |
| C3b | 目录全量分页 5224 行，entity 唯一 | PASS |
| C4 | 版本绑定正负路径 + 空实体空 records | PASS |
| C5 | 伪造/跨选择 cursor 显式拒绝 | PASS |
| C6 | 版本列表恰 1 项 + canonical sha256 + published_at | PASS |
| C7 | availability=unknown + coverage_evidence 含 manifest sha 与 NOT_PRODUCTION_READY + confirmed_gaps 空 | PASS |
| C8 | compute_metrics 固定绑定拒绝 + 重复一致 + 未知 metric 拒绝 | PASS |
| C9 | list_tasks 109 / {103,4,2} + get_task/get_task_progress/get_request 一致 | PASS |
| C10 | 只读不变量：全部计数前后不变 | PASS |
| C11 | 边界拒绝：未知字段/page_size 越界/错误 contract_version/未知 dataset/写工具 schema | PASS |
| C12 | 诚实字段：snapshot_id/query_time/limitations/coverage 元数据 | PASS |

## 运行史

| 工件 | 说明 |
|---|---|
| `results.json` + `run-log.jsonl` | 修正后干净跑，13/13 |
| `results-v1a.json` + `run-log-v1a.jsonl` | 首跑 12/13:C8 `repeat_identical` 失败，定性为断言比较面含 JSON-RPC envelope `id`（响应内容逐字节一致）——断言方法修正，非服务行为异常；冻结值未动 |

## 如实边界

- 不构成生产就绪声明：`GBBQ_QFQ_NOT_VERIFIED` / `FILE_ACCESS_USE_NOT_VERIFIED` / `FILE_SET_CONSISTENCY_NOT_VERIFIED` 三 blocker 与 `NOT_PRODUCTION_READY` 原样。
- 服务运行于 overlayfs 依赖 #213 豁免（flock/身份/执行器守卫被弱化）——本证据证明**本机开发形态**下消费面正确，不证明生产部署形态。
- `market.yearly` 无已发布版本，compute_metrics 只验收了固定绑定拒绝路径；正向计算验收待 yearly 发布后独立切片。
- 耗时为事实记录，非性能断言。
