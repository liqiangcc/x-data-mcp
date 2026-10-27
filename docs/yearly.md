# 固定 daily version 的本地年线核心

> **年线语义的模型状态。** 下文「固定一个已发布 daily version」是版本链模型（schema 20 及更早，历史）。
> 当前部署的 schema 24 单版本模型中（schema 21 自 preview.32 起，历史；M1 于 2026-10-27
> 升到 schema 24），年线派生**当前日线序列**，血统记录派生时的日线摘要与事件 seq，
> 并按 fresh / behind / invalid 分类（日线追加使年线落后但仍可读，除权使年线失效）。聚合规则本身
> （自然年分组、OHLC、量额求和条件、as_of 与未结束年度）不变。见 [单版本（latest-only）行情存储模型](latest-only-storage.md)。

Issue #60 承接 #3：`yearly::aggregate` 只读取一个由调用方从已发布存储取得的
`EffectiveVersion`，以其 Asia/Shanghai 交易日期按自然年分组；支持全部或显式年度集合。
该纯函数能校验输入内容，不能证明任意内存对象确实曾发布；发布身份验证仍由存储/用例负责。

OHLC 保持原十进制精度。volume/amount 只有该年全部值已知且单位/币种证据已确认才求和；
缺失不是0，字段为unknown不影响其他字段或年度。十进制溢出或加法隐式丢失精度均拒绝。

每年输出实际首末参与交易日、base daily version和算法版本。bar.date是实际末参与交易日。
显式传入上海日期as_of，禁止比基础输入末日更早。as_of所在年度标未结束（包括12月31日）；
进入下一自然年才解除该标记。标记只描述自然年是否结束，**不证明数据覆盖完整或新鲜**。
同一输入与as_of生成相同结果，不读取系统时钟。

测试覆盖选定历史年度、多年、十进制尾数、缺失/未知单位、溢出/精度丢失和错误输入。
这些测试只证明本地聚合规则，不证明真实文件parser、QFQ语义或来源完整性。

本核心已接入 bounded 治理链路：`OfficialYearlyVerifier` 以 env 装配的官方快照源为门、对固定
已发布 daily base 重聚合验证 coverage，FG 裁定该 bounded 路径 PASS；env 未装配时生产入口仍
fail closed。#6 默认目录裁定、获取/使用授权、文件一致性与默认全目录
MCP 闭环未完成；update/backfill 有界真实源链路已经 FG 验收（#186/#192）。
后续扩展必须保留每年元数据，并验证来自同一实际已发布版本，不能把内存
fixture 或已有聚合核心当成生产来源通过。
