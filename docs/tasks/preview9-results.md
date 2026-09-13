# preview.9 部署结果（角色与文件名门禁）

2026-09-13，preview.9 完成发布、切换和本机运行时验收。该记录不是生产来源批准，也不是完整目标完成声明。

## 代码、Actions 与包

- 主线发布代码为 `fca27058d6608b2d5ae4af87798cebba43cccb58`，GitHub Release 为 [v0.2.0-preview.9](https://github.com/liqiangcc/x-data-mcp/releases/tag/v0.2.0-preview.9)。
- PR #123 Rust CI run `34761480055` / job `103734972501` 与 package run `34761480056` / job `103734972630` 成功；合并后主线 package run `34761609431` / job `103735323606` 也成功。
- 主线 artifact `10319276184` 下载后完成包校验；包内 manifest、版本和代码 SHA 一致，11 项 synthetic smoke 与 peer check PASS。归档 SHA-256 为 `b223e6b2d845d7ab44bb37bff094b36e7407433695d9390718ca8ce9893b87ab`。

本版本的官方快照 manifest 同时绑定角色、官方下载 URL 和本地归档文件名（允许私有子目录）。这仍只校验声明和本地字节，不认证来源、不证明跨文件一致性、不解释业务字段，也不实现 QFQ。

## 实际切换与验收

`/opt/x-data-mcp-preview/current` 已切换到上述 release。切换前普通库复制为 `state/backup-preview9-20260913T141652Z.sqlite`，SHA-256 为 `5d7385d32c59fc0d9978c3ad9660bdb688fb22eef0e50bfc3ff77d3d9140ec70`；schema9、配置、demo 数据和旧报告均保留，没有迁移或导入行情。

- `preview9-stdio.json`：实际 `mcp_connect` 完成 initialize、18 工具发现、只读空库检查，并在超过 32 秒空闲后再次调用工具，PASS。`tasks`、`requests`、`data_versions`、`catalog_versions`、`scope_occupancy` 均为 0。
- `preview9-tunnel-observation.json`：data service/tunnel service active/running、重启次数为 0；隧道 cgroup 中实际代理 ELF 摘要与当前 release 一致；healthz、readyz 和启动后的控制面轮询 PASS。
- `preview9-final-verification.json` 汇总本次结果。ChatGPT 消费侧和 live source 均 `NOT_RUN`。

## 未验证与阻塞

`#54` GBBQ 业务十进制/QFQ 语义、`#6/#46` 目录与行业覆盖、`#71` 长期获取/使用边界、`.day`+GBBQ 跨时点一致性以及 LOCAL_PIPELINE 仍未解决；系统继续 `NOT_PRODUCTION_READY`，没有生产 fallback。preview smoke、空库和隧道通过不代表官方文件→解析→QFQ→daily/yearly→query/compute 闭环完成。
