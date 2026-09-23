# Node 迁移 P2 — 跨文件一致性决策（file_consistency）

任务：NM-027（agent-coordination/node-migration）。日期：2026-09-23。
基线：`aafc20aaa7d8990839aafdc14d2b8da482f6a51c`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；`src/file_consistency.rs`；#131 Final Gate（pinned set）；`evidence/issue-131-file-set-consistency/`。
证据：`evidence/node-migration/p2/file_consistency/`（probe-results.jsonl 36 行全 PASS、rust-oracle.jsonl 28 例、results.json、SHA256SUMS、README）。

**结果：36/36 PASS / 0 FAIL —— real_131 决策与已提交 decision.json 及 live Rust 逐位一致。**

## 1. 移植：`src_node/governance/file_consistency.mjs`

纯函数 `validate({files, compatibility})` —— 无 fs/network/DB。

- 文件排序+精确去重（完整 FileFact 序：role→bytes→sha256→url→started→completed→last_modified→etag→integrity→rev→ident；Option None<Some）
- 每必需角色：missing_role / conflicting_duplicate（去重后仍 >1）
- 每文件：byte_integrity≠verified → file_integrity_not_verified；started>completed → invalid_file_interval；sha256 非 canonical → invalid_file_hash
- `compatibility:null` → missing_compatibility_evidence 早返回（refs=[]）
- 证据不完整（空 ref/版本不符/空 revision/空 identity/binding 数≠3 或缺角色/binding hash 非 canonical）→ incomplete 早返回（refs=[ref]）
- 窗口倒置 → invalid_compatibility_window（冲突早返回）
- 每角色唯一事实的绑定核对：hash_mismatch、rev/ident 不符或缺失 bound、采集超出窗口
- 理由按序列化 `{"code","role"}` 字符串字典序排序；证据 refs 排序；冲突理由主导 status——缺失/不完整证据不降级显式冲突

时间戳经共享 `src_node/governance/ts.mjs`（NM-026 提取；单一排序尺度含闰秒）——NM-026 回归 72/72 PASS 不变。

## 2. 值得记录的 oracle 语义

- 非 canonical **文件** hash 产生 `invalid_file_hash` **且** `hash_mismatch`（绑定比较照常跑）→ CONFLICT。
- 非 canonical **绑定** hash → `incomplete_compatibility_evidence`（绑定比较前检查）→ NOT_VERIFIED。
- gen-3 `consistency-{input,decision}.json` 为 0 字节占位 → NOT_RUN（results.json 标记）。

## 3. 验证矩阵（36 例）

| 组 | 例数 | 结果 |
|---|---|---|
| 合成 oracle | 26（兼容×2/无证明/缺角色/dup×2/未验证/坏 hash/坏区间/不完整×6/坏窗口/hash·rev·ident 不符/缺 bound×2/每角色独立 rev-id/窗外×2/多理由序/冲突+不完整） | PASS（完整决策逐位） |
| 真实 | 2（#131 输入→VERIFIED_COMPATIBLE；gen-2 事实+null→NOT_VERIFIED） | PASS |
| 类型边界 | 8 | PASS |
| 自测负证 | `decision` → exit 1 | PASS |

## 4. 范围边界

不含证明创建/认证、代际激活、CLI/服务接线、QFQ 发布、部署。gen-2 兼容性不得由 gen-2 清单或本决策推断——证据仅适用于其 pinned set。

## 5. 验证命令

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p2/file_consistency_probe.mjs .` | 0（36 PASS） |
| `node ... decision`（自测负证） | 1 |
| `rustc` oracle（`target/debug/fc_oracle`） | 0 |
| `node scripts/node_migration_p2/snapshot_probe.mjs .`（共享 ts 回归） | 0（72 PASS） |
| `sha256sum -c` / `git diff --check` | 0 |
