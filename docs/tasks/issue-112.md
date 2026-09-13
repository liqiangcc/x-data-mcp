## 真实复现与范围

#110/PR #111 已合并，#108部署v0.2.0-preview.5（main 3dda98aa2bc8115e158674ac1a50dfc020e1448a）后的本机真实验收发现：mcp_connect 成功initialize后，调用方stdin保持打开，等待32秒，proxy_exit_code=0。记录 `/opt/x-data-mcp-preview/reports/preview5-idle-observation.json`；只做initialize，无业务写入。普通库schema9、数据为空，data_service仍运行。tunnel健康/控制面轮询正常不能替代MCP工具连接持续可用的证据。

根因：serve的read_until把正常空闲的socket读超时当成连接结束。代理只是传输Adapter，应保持原stdio可在用户思考后继续调用的行为，不能依赖未确认的外部SDK自动重建MCP会话。依据 #1/#5 的持续MCP访问、#75固定设计2c706325aa2020382631355f802403c05b9662d8的入口/执行生命周期，以及#110长期服务目标。

## 最小修复

只修IPC帧读取：没有半帧时允许空闲，仍周期检查停止；从第一字节开始对未完整帧执行有界超时，保持4MiB帧、16会话、写超时及owner/peer检查。不加新工具/业务资格/来源/schema，不自动重连、重试或重新受理业务请求。
沿用Rust数据服务/代理；现行30秒读预算修订为“未完整帧预算”，不作为空闲MCP会话寿命；记录这是运行兼容性修正。

## AC

AC-01：真实Socket+SQLite会话在短配置预算多倍空闲后仍可initialize/调用；真实打包data_service+mcp_connect在超过默认30秒空闲后仍能请求，stdin保持打开。
AC-02：半帧超时、超大帧、EOF半帧/非法UTF8仍拒绝，不写任务/请求；碎片化合法帧只分发一次，不丢片段。
AC-03：停止可打断空闲读取，有界停止/在途取消/旧非终态保护/同库锁/跨UID回归保持；客户端死亡不取消已受理任务。
AC-04：Actions稳定fmt/clippy/test/build/diff-check、packaged smoke及跨UID全部通过，执行者自查和准确SHA/证据；发布新的不可变预览版本后，本机实际>30秒空闲调用与tunnel后端进程持续性验收。v0.2.0-preview.5及旧失败记录保留，不覆盖旧tag/assets。

## 排除与状态

不清理工作区/cache/历史、不改真实来源或QFQ算法、不采集/迁移真实行情、不改reading-mcp/其他会话。#54/#6/#46/#71/文件一致性仍NOT_VERIFIED，来源/#53四股/ChatGPT消费侧NOT_RUN，NOT_PRODUCTION_READY。用户完整开发/部署/测试目标继续；不是把剩余来源闭环和其他业务任务缩减掉。
