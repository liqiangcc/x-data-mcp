# 规范采用与实践反馈

## 权威来源与版本

本项目采用 [AI Engineering Conventions](https://github.com/liqiangcc/ai-engineering-conventions)，初始基线为远程已发布提交 [bcddf961f1800897f0b13538b6f96f64b462c8a2](https://github.com/liqiangcc/ai-engineering-conventions/tree/bcddf961f1800897f0b13538b6f96f64b462c8a2)，采用日期为 2026-09-09。

固定版本用于使实现和 Review 可复核，避免上游变化后无法确定任务当时遵循的规则。后续升级需检查差异与迁移影响，通过独立规范采用任务更新本节；不把未发布的本地提交视为其他开发环境可取得的基线。

## 按任务阅读

以下链接均指向采用版本。只读任务需要的专题，不复制完整规范到本仓库。

| 关注点 | 规范入口 |
|---|---|
| 职责与依赖边界 | [关注点分离](https://github.com/liqiangcc/ai-engineering-conventions/blob/bcddf961f1800897f0b13538b6f96f64b462c8a2/docs/principles/separation-of-concerns.md)、[复杂度预算](https://github.com/liqiangcc/ai-engineering-conventions/blob/bcddf961f1800897f0b13538b6f96f64b462c8a2/docs/principles/complexity-budget.md) |
| 需求与任务 | [需求到 Issue](https://github.com/liqiangcc/ai-engineering-conventions/blob/bcddf961f1800897f0b13538b6f96f64b462c8a2/docs/workflow/requirement-to-issue.md)、[研发 Issue 模板](https://github.com/liqiangcc/ai-engineering-conventions/blob/bcddf961f1800897f0b13538b6f96f64b462c8a2/docs/templates/development-issue.md) |
| Issue 操作与进度 | [Issue CLI](https://github.com/liqiangcc/ai-engineering-conventions/blob/bcddf961f1800897f0b13538b6f96f64b462c8a2/docs/workflow/issue-cli.md)、[中文阶段标签](https://github.com/liqiangcc/ai-engineering-conventions/blob/bcddf961f1800897f0b13538b6f96f64b462c8a2/docs/workflow/issue-progress.md)、[工作计划](https://github.com/liqiangcc/ai-engineering-conventions/blob/bcddf961f1800897f0b13538b6f96f64b462c8a2/docs/workflow/issue-planning.md) |
| 身份与历史 | [UC / BR / BC / INC](https://github.com/liqiangcc/ai-engineering-conventions/blob/bcddf961f1800897f0b13538b6f96f64b462c8a2/docs/traceability/identity-system.md)、[Git 历史](https://github.com/liqiangcc/ai-engineering-conventions/blob/bcddf961f1800897f0b13538b6f96f64b462c8a2/docs/traceability/git-history.md) |
| 正确性与运行证据 | [测试与验证](https://github.com/liqiangcc/ai-engineering-conventions/blob/bcddf961f1800897f0b13538b6f96f64b462c8a2/docs/testing/README.md)、[验证结果语义](https://github.com/liqiangcc/ai-engineering-conventions/blob/bcddf961f1800897f0b13538b6f96f64b462c8a2/docs/schemas/verification-result.md) |
| 自动执行与 Review | [自动化](https://github.com/liqiangcc/ai-engineering-conventions/blob/bcddf961f1800897f0b13538b6f96f64b462c8a2/docs/automation/README.md)、[Code Review](https://github.com/liqiangcc/ai-engineering-conventions/blob/bcddf961f1800897f0b13538b6f96f64b462c8a2/docs/workflow/code-review.md) |
| 规则演进 | [规范治理](https://github.com/liqiangcc/ai-engineering-conventions/blob/bcddf961f1800897f0b13538b6f96f64b462c8a2/docs/reference/governance.md)、[迁移记录](https://github.com/liqiangcc/ai-engineering-conventions/tree/bcddf961f1800897f0b13538b6f96f64b462c8a2/docs/migrations) |

## Rust 与 MCP 的适用范围

跨语言的关注点分离、需求来源、稳定身份、分层测试、自动化、历史追溯和验证状态约定适用。Java 类后缀、注解、Spring 包组织与 ArchUnit 不作为 Rust 的强制实现方式。

Rust 的 crate / module 边界、错误类型、异步调用及身份元数据如何表达，应在真实用例中确定并验证。不预先创建大量空 crate、自定义宏或规则 DSL。

MCP 契约测试负责证明协议 Adapter 兑现契约；核心业务规则和用例分别验证。仅当存在 HTTP 边界时加入相应 HTTP 验证，不把 stdio MCP 测试冒充 HTTP 验证。部署在任务范围内时，继续执行目标实例验证。

## 实践反馈

本项目同时用于验证规范在 Rust 数据治理项目中的可用性。发现问题时，在相关研发 Issue 中记录：

- 采用版本和具体规则链接；
- 触发问题的用例、代码、测试或实际操作；
- 歧义、冲突、重复事实、导航困难、验证困难或不必要成本；
- 对交付的影响、可复现证据及候选修正方式。

项目特有选择在本仓库维护。涉及通用规范的修正，在规范仓库单独提出并关联原始实践证据；不混入业务行为提交。跨仓库写入仍遵循任务授权范围。

不为每个任务强制新建复盘文档；只有实际出现问题时记录，已有 Issue 与证据直接引用。规范调整后，再通过本项目的采用任务记录升级与验证结果，不在两处维护两份当前规则。
