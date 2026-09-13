# preview.8 部署结果（角色路径门禁）

2026-09-13，preview.8 完成发布、切换和本机运行时验收。该记录不是生产来源批准，也不是完整目标完成声明。

## 代码、Actions 与包

- 主线发布代码为 `1f62813d45223f0926457aff07e5344c419f39f1`，GitHub Release 为 [v0.2.0-preview.8](https://github.com/liqiangcc/x-data-mcp/releases/tag/v0.2.0-preview.8)。
- PR #120 的 Rust CI run `34759992339` / job `103731030773` 与 package run `34759992349` / job `103731030726` 均成功；合并后主线 package run `34760140841` / job `103731418379` 也成功。
- 主线 artifact `10318533066` 下载后 ZIP 为 `13125437` 字节；包归档 SHA-256 为 `3c2f5756cf23ca8b51858f2a1a29ea1acfb7d88cc926b543a1a0dbe788ff714d`。包内 manifest、版本和代码 SHA 一致，11 项 synthetic smoke 与 peer check PASS。

本版本包含官方快照 manifest 的角色—路径绑定：`base_industry`、`gbbq` 和 `day` 只能声明各自记录的官方 TDX 下载路径。它仍只校验声明和本地字节，不认证来源、不证明跨文件一致性、不解释业务字段，也不实现 QFQ。

## 实际切换与验收

`/opt/x-data-mcp-preview/current` 已切换到上述 release。切换前普通库复制为 `state/backup-preview8-20260913T134531Z.sqlite`，SHA-256 为 `5d7385d32c59fc0d9978c3ad9660bdb688fb22eef0e50bfc3ff77d3d9140ec70`；schema9、配置、demo 数据和旧报告均保留，没有迁移或导入行情。

- `preview8-stdio.json`：实际 `mcp_connect` 完成 initialize、18 工具发现、只读空库检查，并在超过 32 秒空闲后再次调用工具，PASS。`tasks`、`requests`、`data_versions`、`catalog_versions`、`scope_occupancy` 均为 0。
- `preview8-tunnel-observation.json`：data service/tunnel service active/running、重启次数为 0；隧道 cgroup 中实际代理 ELF 摘要与当前 release 一致；healthz、readyz 和启动后的控制面轮询 PASS。
- `preview8-final-verification.json` 汇总本次结果。ChatGPT 消费侧和 live source 均 `NOT_RUN`。

## 未验证与阻塞

`#54` GBBQ 业务十进制/QFQ 语义、`#6/#46` 目录与行业覆盖、`#71` 长期获取/使用边界、`.day`+GBBQ 跨时点一致性以及 LOCAL_PIPELINE 仍未解决；系统继续 `NOT_PRODUCTION_READY`，没有生产 fallback。preview smoke、空库和隧道通过不代表官方文件→解析→QFQ→daily/yearly→query/compute 闭环完成。
