# preview.7 部署结果（主线 2b92fa7）

2026-09-13，preview.7 完成发布、切换和本机运行时验收。该记录不是生产来源批准，也不是完整目标完成声明。

## 代码、包与 smoke

- 主线发布代码为 `2b92fa71884283bc8176511b7b8b7d886b7fc2a2`，GitHub Release 为 [v0.2.0-preview.7](https://github.com/liqiangcc/x-data-mcp/releases/tag/v0.2.0-preview.7)。
- 主线 package run `34757887683` / job `103725335260` 成功；artifact `10317539641` 下载后 ZIP 大小 `13124052` 字节，包归档 SHA-256 `c2006954a300a2bbe1e338b1e2c642266b955aa2704b2f63edd307384922c50e`，与 Release asset digest 一致。
- 包内 manifest、版本和代码 SHA 一致；隔离 synthetic fixture smoke 11 项及 peer check PASS。该检查不运行真实来源，报告中的 `live_source=NOT_RUN`、`production=NOT_PRODUCTION_READY` 保持有效。

## 实际切换

`/opt/x-data-mcp-preview/current` 已切换到 `2b92fa71884283bc8176511b7b8b7d886b7fc2a2` 对应 release。切换前复制普通库到 `state/backup-preview7-20260913T130603Z.sqlite`，SHA-256 为 `5d7385d32c59fc0d9978c3ad9660bdb688fb22eef0e50bfc3ff77d3d9140ec70`；schema 9、配置、demo 数据和既有服务接线未迁移或改写。preview.6 的 release、库备份、profile 备份和失败证据均保留。

部署记录为 `preview7-deployment.json`；最终汇总为 `preview7-final-verification.json`。此次版本变更只包含已合并的保守 DBF/GBBQ 原始容器证据和 preview.7 版本号，不接入 OfficialFilePipeline，不导入行情。

## 运行时验收

- `preview7-stdio.json`：实际 `mcp_connect` 经过 initialize、18 工具发现、只读空库查询；同一代理 stdin 空闲超过 32 秒后再次调用 `get_data_capabilities` PASS。普通 SQLite 保持 schema 9，`tasks`、`requests`、`data_versions`、`catalog_versions`、`scope_occupancy` 均为 0。
- `preview7-tunnel-observation.json`：data service 和 tunnel service 均 active/running、重启次数为 0；tunnel cgroup 中实际 `mcp_connect` ELF 摘要为 `bfc9e3da14431d151101ebc292e8d3761c0de7dd6c3c46fb58654e8b8c9e2326`，与当前 release 一致；healthz/readyz 及启动后的控制面轮询 PASS。
- 这证明隧道到代理的运行时接线和空库只读入口可用，不证明 ChatGPT 消费侧本次调用，也不证明真实数据来源或生产查询闭环。

## 未验证与阻塞

ChatGPT 消费侧、真实来源、#53 四股复验均为 `NOT_RUN`。`#54` GBBQ 业务十进制/QFQ 语义、`#6/#46` 目录/行业覆盖、`#71` 文件长期获取和使用边界、`.day+gbbq` 跨时点一致性、LOCAL_PIPELINE 仍未验证或未实现；系统继续 `NOT_PRODUCTION_READY`，没有生产 fallback。普通库为空是预期的 fail-closed 状态，不能把 preview.7 smoke 或隧道通过写成官方文件→解析→QFQ→daily/yearly→query/compute 完成。
