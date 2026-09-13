## 目标与权威
实现 #3 显式发布结果核实、重复/状态变化保护及四类核实串行；依据 #75 固定设计 2c706325aa2020382631355f802403c05b9662d8。关联 #79/#92/#94/#99。主线 7172ba5dc64b425cbde66b4950c41ae6c03b2c4f，线上 preview.4；不回退来源调查或改生产门禁。

## 设计边界
- 独立 request_id/核实任务、target_task_id，复用 verification:task:<id> 与停止核实串行，进度 evidence_check。MCP 只做形状/授权映射，不接收成功/失败/强制结论。
- 应用层判资格/结论；SQLite 以单次读快照核验 task/items、publication/执行收据、固定版本 compat/typed projection/lineage。没有收据仅表示缺少提交证据，不能断言未发布；部分成功、未执行、结果未知分别保留。
- 停止写入证据不能替代发布证据，发布证据也不自动释放原占用。不隐式发布、回滚、重试或移动 head。
- 核实结论/完整审计在条件事务记录，绑定目标 revision 和核验过的证据；重放读原结果，状态/证据改变不使用过时结论。未知运行保留保护，不递归创建异常核实任务。
- first slice 先统一真实 SQLite 发布事实端口及篡改/缺证据回归、触发 CI；再受理/决策/审计/runner/公开入口及完整审核。首个端口不等于整个核实用例完成。

## AC→测试要求
1. 已提交新版本和 no-change 复用都有可核验收据；daily/yearly/legacy publication 正确区分，固定 base 非最新不是损坏。
2. 缺收据不猜失败；成功项缺必要关联、错 item/任务/version、坏摘要/投影/lineage拒绝；核验与返回同一读快照，后续损坏停止读取。
3. 真 SQLite 同目标/跨核实类型竞争；请求重放/异参；条件 revision 与证据变化；提交前退出回滚、提交后响应丢失重查。
4. 核实不写行情/head，不释放原占用或修改已成功项；取消仅本核实；旧运行无收据不自动重跑。
5. 实际 stdio schema/权限/发现/受理/worker/结果重放及不足/错误；记录操作者时间依据结果。
6. Actions fmt/clippy --all-targets --all-features --locked -- -D warnings/test --locked/build --locked/diff-check；本机仅轻量检查，不clean、不删缓存。fixture/live明确区分。

真实来源/#53四股 NOT_RUN，已执行写入器停止协议 NOT_VERIFIED，NOT_PRODUCTION_READY。没有新采集/部署/真实库导入；完整Goal仍包括其他核实/恢复与数据闭环，不由本任务缩小。
