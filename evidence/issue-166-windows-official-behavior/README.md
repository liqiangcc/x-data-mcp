# issue-166-windows-official-behavior

Windows 实机官方客户端行为验证，为 #46/#54 的 `BLOCKED_EXTERNAL_EVIDENCE` 补齐外部证据入口。
机械证据，不声称契约；结论状态以 result.json 为准。

## 交付物

- `plan.json` — 观察任何客户端行为输出前冻结的计划（样本、方法、绑定规则、状态集）
- `environment.json` — Windows / 客户端 / 安装包 / TQ 组件事实
- `inputs.json` — 官方数据文件快照 hash
- `raw/` — 非敏感原始导出、记录字节、截图及其 sha256
- `result.json` — 机器可复核对比结果
- `determinism.json` — 重放一致性

## 状态

进行中：plan 已冻结，行为采集中。
