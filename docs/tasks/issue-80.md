# Stage C 实施与契约审查

授权：用户要求执行全部开发、审核和发布；不再沿用历史“尚未派发”。
实现 base 为 PR #83 合并提交 da5c597；设计采用 2c706325aa2020382631355f802403c05b9662d8
的 generic-data-tools.md/implementation-baseline.md，以及 c2f89085850423734193f3090dd435c34f4688ec SQLite 专项。

本切片按 #80 AC-01—06，保留 #78 旧协议，新增独立 SQLite 数据入口，不注入生产 fixture。
读取能力：catalog/daily/yearly query_data（行情 single/batch）、list_data_versions、
get_data_availability、list_tasks、get_task_progress，能力目录与详情同源。
参数 schema 和输出由同一类型定义生成；嵌套输入拒绝额外字段。

冻结工程预算：batch 最多 100 对象；page 默认 100、最多 1000；实例 32 个快照；
每快照最多 100000 记录/10000 任务项、序列化 16 MiB；超额拒绝，不截断。
全部游标绑定调用者授权集合、工具/契约、规范化条件、页大小、固定版本/时间。
授权在枚举和计数前执行，每次续页重新检查，cursor 不是授权凭证。

行情版本选择和任务事实在同一 SQLite read transaction；游标保存首次结果并在续页核验全部固定版本。
版本损坏 batch 整体拒绝。availability 单项坏版本为 unavailable，坏库是工具错误；
coverage complete 本身没有显式范围证据时不能推断请求范围 available，保守 unknown。
日期不连续不构造 gap。catalog 字面筛选 AND、业务身份排序，行业 missing/conflicted 原样表达。
目录 fixture 发布复用已有 #6 校验；失败不覆盖旧 head。显式 fixture API 不在 stdio 暴露。

新工具契约 v1，旧六控制工具继续兼容；未验证来源保持 blocked，无后台采集/网络 fallback。
新写分支的完整形状、幂等规范化及旧/new 输出兼容在注册前逐项固化与测试。
本记录不宣称尚未完成处理器 implemented，不以来源门禁替代应用层验收。

验证：真实 SQLite、stdio、闭合 schema、跨工具引用、权限和分页竞争；Actions 稳定入口。
真实采集/迁移/远程认证 NOT_RUN；生产来源 NOT_VERIFIED。部署另行单独验收。
