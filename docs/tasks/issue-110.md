## 目标与权威依据

按用户“完成所有开发、重新部署并测试”的持续授权，交付独立 Linux 本机运行入口，使显式受理任务的执行生命周期属于长期服务而不是 ChatGPT/stdio 客户端。关联 #1 首期运行与访问、#3 重启/取消/核实、#5 只读进度、#75 完整目标，以及 #108 当前预览发布。
固定整体设计 2c706325aa2020382631355f802403c05b9662d8（design.md 运行、访问与配置）；SQLite c2f89085850423734193f3090dd435c34f4688ec。实现 base main 56ae9d0b161264566889d41eb6db1163b7e66f27。
#20 是旧 JSON worker 历史切片，其自动 resume 文字不适用于现行 #3/#78；不重开或改写历史。

## 本次完整运行切片

- 新独立 `data_service <initialized-db> <private-socket>` 长期单 worker；`mcp_connect <private-socket>` 仅 stdio↔本机 IPC。复用 DataApi、TaskControl、现有 handler/worker，不复制业务决策，不新增数据类别/来源/任务 schema。
- Linux 本机 Unix socket，owner-only 目录/文件和同 UID peer 身份核对；不监听 TCP/匿名公网，不把 caller 自报 actor 当授权。tunnel 后续显式接 proxy，当前 #108 版本不变。
- 服务启动生成一个 run_id，所有会话受理与 worker 共享它；每次重启新 run。仅领取本 run 已显式受理的任务，旧非终态保留占用并按既有 read 语义待核实；不自动 reconcile/续跑/释放。
- 服务拥有单执行串行器，背景 worker 与显式 run_worker_once 共用；忙时明确 worker_busy，不开第二执行者。读、受理、取消在其他会话仍可调用。断开只结束入口会话，不取消任务。
- 同一数据库服务实例用内核文件锁排他，既有 socket 不盲目覆盖；只在取得对应排他保护并验证 owner/路径身份后处理自己的失联 runtime socket。锁/IPC 文件不是来源或停止业务写入的证明，不用它解除既有核实门禁。
- 显式工程预算：最大16会话、单行4MiB、空闲读/写30s、worker空闲轮询100ms、停止等待10s。它们是可测试资源/运行约束，不是性能承诺或业务完成条件；超限/退出不伪造任务终态。正常 stop 停接入/停止新领取，有界等在途执行；超预算进程退出后遗留事实仍待核实。
- 独立 binary 实现和 fixture CI 后审查、再在后续准确 release 中部署；本任务不替换现在线上 preview.4 或 #108 待发布包。旧 mcp_data/dev/manual 入口保留兼容开发用途，不与新 service 同库装配。

## AC 与直接验证

AC-01：真实 daemon+proxy 二进制 initialize/tools/list/capabilities 与18工具一致；参数、错误、输出复用旧 stdio 契约，无新增 wire 业务规则。
AC-02：真实 SQLite 显式受理后断开/杀死客户端，后台仍完成该任务；新会话可按原 request_id 查询唯一事实，不能只用 mock 证明。
AC-03：单 worker 与 run_worker_once 不并行；多个会话读/取消可达。使用有界 fixture 阻塞的执行器证明执行期间取消和断连行为；fixture 不接生产来源。
AC-04：服务重启的新 run 不执行旧 pending/running/cancel_requested，原占用保留；只执行新受理任务。异常退出不报告为成功。
AC-05：两个实际服务同库竞争拒绝；私有目录/owner/peer、已有非socket/符号链接/替换路径拒绝，不删除他人文件；缺库/坏库/未知schema不自动初始化或升级。
AC-06：停止不再领取；在途事实可核对，超时仍保留不确定状态；frame/会话预算有直接回归。来源仍 blocked/NOT_PRODUCTION_READY，不采集真实数据。

## 验证入口和排除项

磁盘受限，本机仅静态编辑、fmt/diff；真实二进制/SQLite/进程故障测试进入 Actions，运行 README 的 fmt/clippy --all-targets --all-features --locked -- -D warnings/test --locked/diff-check 与既有 build。提交后尽早 Draft PR，结果不明不标 PASS。不删除共享工作树/cache/旧包，不导入数据，不解除 #54/#6/#46/#71/文件一致性门禁；source/#53 four-stock/新版 ChatGPT 消费侧 NOT_RUN。
来源闭环、受理/恢复核实等其他缺项仍在完整目标内，本切片不替代它们、不关闭父目标。

## 实施首个提交

本任务 worktree `/root/x-data-mcp-service`、branch `local-data-service`；不修改 #108 的准确发布候选。
worker 轮询支持 CLI `--poll-ms 10..60000`（默认100），用于显式运行预算与真实断连回归；不是来源解锁/业务资格配置。
数据库服务排他锁直接持有实际 DB inode 的内核 flock，与 SQLite fcntl 事务锁分开，不写业务表；私有 socket owner 文件绑定相同 inode，只回收自己的失联 socket，不删除普通文件/符号链接。
现有兼容 stdio 入口保留，不与 service 同库运行；本机 owner 对库的直接修改不属于 IPC 授权协议。

首轮提交本地 fmt/diff 检查，完整 compile/test/clippy 与真实进程测试尚待 Actions。首次测试包括真实 service/proxy 客户端死亡后 source-refusal 任务完成、新启动身份保护旧非终态、同库实例冲突、坏库/未知schema/不建库、私有路径及执行 gate；fixture 阻塞执行、取消/停止预算测试继续补齐，不把未跑场景写 PASS。
