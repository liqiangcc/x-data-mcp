# preview.5 发布与部署（#108）

## 基线和工作区

用户持续授权完整开发/审核/重新部署/测试；本切片只处理新预览版本，不替代完整目标。实现 base main ec50623dc385b873339273e5d8c97ad9dda12ee5（PR #107 已合并）。独立 branch preview5-release，worktree /root/x-data-mcp-preview5，磁盘受限使用稀疏 checkout；共享脏树、历史工作树和缓存均不清理。

本轮只读部署核对：current 指向 7172ba5dc64b425cbde66b4950c41ae6c03b2c4f / preview.4；x-data-mcp-tunnel.service active/running；普通 preview.sqlite schema7，在单个只读事务中 tasks/requests/data_versions/catalog_versions/scope_occupancy 都0。此事实必须在切换前重新核对，不能假定后来没有新数据。

PR #107 代码 HEAD 6aaec73 的完整稳定 CI 已通过；其额外 package run 34748791661 排队结果没有记为 PASS。代码合并与正式发布分开：本 release 必须有自身准确主线 SHA 的包和最终实测，不能把相同代码的旧包部署成新 SHA。

## 验证与操作顺序

1. 本 PR 只更新项目版本/lock 项目条目和发布状态，不升级依赖、不修改业务算法。Actions 运行已有 fmt/clippy --all-targets --all-features --locked -- -D warnings/test --locked/build --locked/diff-check、离线 evidence 与9项 packaged binary synthetic smoke。
2. 自查和合并后，核验 main 准确 SHA 的 CI/包；创建匹配 tag/prerelease，核对 archive checksum、manifest、binary SHA 与 --version。服务切换只使用该最终验证包，不在本机完整编译。
3. 在独立临时目录/显式 synthetic 新库完成 smoke；普通库不写 demo。检查可用空间，不删除缓存、他人文件或旧 release。
4. 重新核对普通库/服务；暂停仅 x-data-mcp-tunnel.service 后，备份数据库和配置。显式 task_sqlite upgrade，将受支持 schema7 升至9；检查完整性、schema 和事实保留。出现新任务/数据时依实际事实处理，不用旧备份覆盖新增事实。
5. 原子切 current 并启动原 tunnel，reading-mcp 和其他服务不变。保留旧包、备份和 demo；新 demo 若需要则独立命名。核验实际18工具、registry、schema、运行 SHA 与来源拒绝、tunnel health/control-plane poll。
6. stdio/打包 synthetic smoke、普通库事实、tunnel health 与实际 ChatGPT 消费验证分别记录。后者无法直接运行时明确 NOT_RUN；不把接口 ready 当作来源或数据闭环完成。

## 尚未完成与边界

本文件记录发布准备，tag/下载/升级/服务切换/新版线上验证尚未执行；准确结果写入 #108。来源/#53 四股/新版 ChatGPT 消费侧 NOT_RUN；#54/#6/#46/#71/文件一致性 NOT_VERIFIED，NOT_PRODUCTION_READY。不开账号/Key、不采购/联系厂商、不采集或迁移真实行情、不关闭父需求或完整目标。
