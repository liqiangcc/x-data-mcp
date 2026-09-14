# Issue #131：官方文件跨文件一致性契约

状态：实现了独立的 fail-closed domain validator；当前保留的官方文件快照仍为 `NOT_VERIFIED`，没有接入生产 pipeline。

## 边界

`snapshot::inspect` 负责逐文件字节核验并输出 `FILE_BYTES_VERIFIED`。`file_consistency::validate` 负责另一个问题：只有一份明确、不可变的兼容性证明同时绑定 base/hsjday/GBBQ 的角色、精确 SHA-256、兼容集合自身的 identity/revision 和适用时间窗时，才返回 `VERIFIED_COMPATIBLE`。证明中的 `compatibility_set_identity` / `compatibility_set_revision` 是审计过的组合事实，不是声称三个独立文件共享同一来源版本；每个 `FileFact` 的可选 `source_identity` / `source_revision` 仍表示该角色自身的来源事实。相同下载日期、URL、ETag、Last-Modified 或分别正确的字节摘要不会被推断为可组合。

验证结果为 `VERIFIED_COMPATIBLE`、`NOT_VERIFIED` 或 `CONFLICT`。缺少角色、缺少兼容性证据或证据字段不足会稳定返回 `NOT_VERIFIED`；明确的摘要、身份、修订或时间窗矛盾返回 `CONFLICT`，且冲突优先于证据不足。决定会按角色排序并去除完全相同的重复事实，同时保留角色、摘要和来源元数据，保证输入顺序稳定。

`VERIFIED_COMPATIBLE` 只允许进入后续 #54/#46/#71 门禁，不代表 GBBQ 规范化、沪深 A/行业目录、服务器使用范围或生产就绪。`CompatibilityEvidence` 只能由已验证、可审计的 evidence fact 构造；validator 不把调用方填写字符串当作事实，也不从缺失字段猜测批准。没有新增 MCP 工具、fallback/force 开关、网络获取、真实行情导入或生产装配。

## AC → 实现/测试

- AC-01/02/03：`file_consistency::validate` 要求三种角色和显式 proof；`retained_issue_74_snapshot_has_byte_evidence_but_no_cross_file_approval` 证明既有 #74 清单即使逐文件字节已核验，仍返回 `NOT_VERIFIED`。
- AC-04/05：`explicit_hash_revision_identity_and_window_are_required_for_compatibility`、`missing_role_is_not_verified_and_does_not_infer_latest`、`matching_metadata_without_explicit_proof_remains_not_verified`。
- AC-06/07：`proof_hash_revision_identity_or_window_conflict_is_rejected` 覆盖摘要、修订、时间窗矛盾；缺少逐文件 byte proof 会保持 `NOT_VERIFIED`。
- AC-08：`order_and_exact_duplicates_do_not_change_decision_or_evidence`、`divergent_duplicate_role_is_conflict` 覆盖顺序稳定、等价重复稳定和冲突重复拒绝。
- AC-09：决定保留完整 role/hash/source 元数据和 evidence refs；不把兼容状态提升为 production-ready。
- AC-10：本模块为领域层，未修改 MCP、SQLite、官方来源路径或既有 #54/#46/#71 结论；完整 fmt/clippy/test/diff-check 由 CI 执行。

## 当前证据与未解除门禁

`evidence/issue-74-snapshot-input/retained-manifest.json` 只有逐文件输入事实，没有跨文件修订关联证明，因此本轮明确为 `NOT_VERIFIED`。#54 GBBQ 业务十进制语义、#46/#6 目录映射、#71 文件服务器使用边界、归档成员与跨文件业务可组合性仍需各自证据；本 PR 不猜测、不放行。
