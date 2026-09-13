后续实现范围（#99，编码前记录）：为“从未领取/未开始执行”建立真实 SQLite 证据，保持已执行任务的真实停止协议未验证。

基于 main 7e134397fba2ab584b36b63a344a63caabffe7fc；分支 never-started-stop-proof，隔离工作树 /root/x-data-mcp-never-started。主线 CI 34740134872、package 34740134845 已 SUCCESS。

证明条件：仅已审计的 legacy compound / daily update/backfill / yearly update/backfill 契约；原状态 pending、revision=0、owner 空、所有项 pending 且无 outcome/result；同一 SQLite 读快照内核验完整任务与全部 publication/result 关联不存在。应用层解释这些事实，SQLite 只读取事实。最终仍由既有双任务 revision/occupancy 条件事务收尾；旧执行器必须先成功 Claim 再调用 provider，故领取先提交则核实拒绝，核实先提交则旧执行器不能开始。不是 PID 消失或 run_id 变化证明。

不外推至 running/cancel_requested、未知契约、已有结果、未来外部副作用写入器；这些继续 insufficient / integrity error。不会把“没有数据库 publication”单独当停止证据。旧 JSON 入口不共享此 SQLite 证明。

测试：真实 SQLite 清洁初态、receipt/绑定异常、已领取/未知契约拒绝；两连接 Claim 与收尾竞争，证明只有一个条件提交成功；旧执行器落败时 provider 调用计数为零；提交后响应丢失沿用 #100 收据重查；实际 stdio 后续接线回归。先提交证据端口和基础测试触发 Actions，再接入默认核实器并更新能力/文档及完整回归。真实正在运行写入器的跨进程停止、外部副作用协议仍待后续交付，不能以本切片关闭 #99 或完整目标。


## 默认接线与证据边界

开发默认 worker 现使用 NeverStarted，SQLite Adapter 在同一个 read 事务内读取完整任务和四种发布/执行关联的存在性；应用层负责资格与证明解释。未新增 schema、PID/文件锁、外部请求或调用者批准参数。所有受支持执行器必须先 Claim/StartItem，所有任务状态推进增加 revision；最终提交比较完整目标快照，不只相信先前读出的“没有写入”。

真实 SQLite 两连接竞态回归覆盖旧 Claim 与停止提交只有一个成功；stale_selected_executor_is_never_called_after_never_started_interruption 直接断言预先选中的旧执行器不会调用源端。四种关联表分别注入异常，均须拒绝。已领取旧任务的实际 stdio 仍检查不足与保留占用；包 smoke 的未领取旧任务现在经真实默认入口收尾 interrupted。

这只证明无执行曾开始，不能核实一个正在执行或结果未知的写入器。未知契约、revision 非零（包括仅取消过）、owner 非空及已有 outcome 一概不凭本规则批准。能力总览保持 blocked，描述明确部分适用性，受理不是承诺检查通过。关联存在性查询可能扫描表，尚无规模/延迟性能证据；不声称高性能或恒定成本。完整停止证据、其他核实类型及生产来源链路仍未交付。本分支尚未部署。
