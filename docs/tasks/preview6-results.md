# preview.6 部署结果（#108/#110/#112）

2026-09-13，执行者开发、执行者自查、发布及本机部署验证。不是独立/人工 Review，不是生产来源批准。

## 代码与 Actions

- PR #113 HEAD `fb9e997ae361905a594abe4ce771b0ebea935322`，合并后的发布代码 `7e37d27a8d85e921d8162d7e49377ad1700d6ada`。
- [主线 CI 34754288540 / rust103715927708](https://github.com/liqiangcc/x-data-mcp/actions/runs/34754288540/job/103715927708)：SUCCESS，fmt、clippy all-targets/all-features/locked/-D warnings、test locked、build locked、diff-check、离线证据及实际跨UID检查。
- [主线 package34754288529 / package103715927628](https://github.com/liqiangcc/x-data-mcp/actions/runs/34754288529/job/103715927628)：SUCCESS，11项打包检查及实际跨UID检查。主线artifact10317035431，非PR synthetic merge包。
- [v0.2.0-preview.6](https://github.com/liqiangcc/x-data-mcp/releases/tag/v0.2.0-preview.6) tag指向上述代码，archive SHA-256 `92b90092bedf0734678e7d28fb769bab1d83d9064899fd029b56cfe77da6a765`；GitHub资产digest与本机一致。
- 本机没有完整Cargo编译；实际安装包的隔离fixture 11项检查及跨UID检查PASS，不冒充真实来源验证。

## 实际部署与修正

`/opt/x-data-mcp-preview/current` 指向上述代码release目录。普通库schema9，无schema迁移、行情导入或demo重建。
备份 `state/backup-preview6-20260913T113752Z.sqlite`，SHA-256 `5d7385d32c59fc0d9978c3ad9660bdb688fb22eef0e50bfc3ff77d3d9140ec70`。
既有preview.5包、tag、库/配置备份及失败报告保留。

初次切换后真实tunnel进程检查失败：profile仍启动 `mcp_data preview.sqlite`，尽管mcp.json已改为代理。不能根据mcp.json或health成功推断装配已改变。
只备份并修改 `/root/.config/tunnel-client/x-data-mcp.yaml` 的main MCP command为：
`/opt/x-data-mcp-preview/current/bin/mcp_connect /opt/x-data-mcp-preview/run/mcp.sock`。
其他profile字段、tunnel ID、账号/密钥、reading-mcp不修改。备份 `state/tunnel-profile-before-proxy-20260913T114001Z.json` 为私有本机文件，不上传配置内容。
此次重启仅因实际接线修正，不为绕过观察超时。data_service PID3687818保持，tunnel新PID3689868，实际代理PID3689885。

## 验证与保留的失败

报告根目录 `/opt/x-data-mcp-preview/reports/`：

- `preview6-smoke.json`：隔离synthetic fixture 11项、peer检查PASS。
- `preview6-deployment.json`：记录程序切换时的备份与schema9/数据不变，不改写其当时pending状态。
- `preview6-stdio.json`：普通库实际mcp_connect，18工具集合与registry一致、只读空数据查询PASS；同一代理stdin打开32.008秒后调用get_data_capabilities PASS。任务、请求、行情/目录版本、占用均0。
- `preview6-initial-health-observation.json`：首次没有成功控制面轮询，原退出码2保留；health/ready已200。
- `preview6-tunnel-proxy-absence.json`：首次真实tunnel没有代理，FAIL保留；普通库当时走旧式入口，不把其进程当demo。
- `preview6-profile-fix.json`：实际profile最小修改及备份/摘要，数据服务PID未变。
- `preview6-tunnel-after-profile-fix.json`：修正后PASS，服务运行超过33秒时实际代理仍存在、ELF摘要匹配、health/ready200及启动后成功控制面轮询。没有声称30秒控制面延迟SLA。

实际运行data_service SHA-256 `57c9354a9090a4f54cb56ad2a62777507d599ce557ec2f7eb8e0a526ac69ba14`；代理 `46ad299e95e1532bcca2ef069e58a0745d7c3d1c7fbbb5edae065619b0b3f7aa`。
preview.5的`preview5-idle-observation.json`仍是实际直接代理空闲退出的失败，preview.6修复不改写它。

## 未验证与未完成

ChatGPT本次消费侧 NOT_RUN；真实来源/#53四股 NOT_RUN；#54/#6/#46/#71与跨文件一致性 NOT_VERIFIED。
普通库为空、来源装配仍fail closed，没有生产fallback。NOT_PRODUCTION_READY。
18工具/fixture/服务存活不代表官方文件→QFQ→发布→真实查询计算闭环完成。
其他恢复、受理核实、目录与来源任务仍由父Issue承接，本报告不关闭父需求、不把剩余工作写成完成。
