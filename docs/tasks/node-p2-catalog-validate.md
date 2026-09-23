# Node 迁移 P2 — 目录候选纯校验（validate）

任务：NM-029（agent-coordination/node-migration）。日期：2026-09-23。
基线：`600afc4853f3ab508904bf328022021a435f6cdf`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；`src/catalog.rs::validate_snapshot`/`validate_record`；`tests/catalog.rs`。
证据：`evidence/node-migration/p2/catalog_validate/`（probe-results.jsonl 57 行全 PASS、rust-oracle.jsonl、negative-selftest.jsonl、results.json、SHA256SUMS、README）。

**结果：57/57 PASS / 0 FAIL —— 34 例 live Rust oracle 全字节一致（含 5,224 条真实 gen-2 候选）。**

## 1. 移植：`src_node/catalog/validate.mjs`

`validateSnapshot(snapshot, previous)` → `{accepted:true}` 或 `{accepted:false, reason:<serde RejectReason>}`：

- 门序：source 空白 → completeness → 记录逐字段（symbol/name/source_security_id/source）→ source 一致 → exchange/type 范围 → industry → 重复键/源 ID → 成员消失（前版本）
- `RejectReason` serde 外部标签逐字：`"EmptySnapshot"` 单元变体为裸字符串；`DuplicateSecurity{key}`、`MemberDisappeared{members}`、`IdentityConflict{message}`（`SecurityKey` Debug 逐字：`中` 字面、`U+FEFF`→`\u{feff}`、`"`→`\"`）
- Rust `trim` = Unicode `White_Space`——`U+FEFF` **不**空白（JS `.trim()` 会误删；oracle `feff_source`/`feff_symbol` 验证）
- 成员消失排序：exchange 序（Shanghai<Shenzhen<Other）→ symbol UTF-8 字节序
- 类型边界：非 serde 形状抛 `ValidationInputError`（类型错误、未知枚举变体、多余/缺失字段、孤立代理、坏前版本形状）——23 例

## 2. 纯净性

确定性、只读；无 DB/head CAS/服务/代际/网络/发布；不从计数推断全市场完备性；来源授权/兼容性证明分离。

## 3. 验证矩阵

| 组 | 例数 | 结果 |
|---|---|---|
| source/completeness 门 | 8 | PASS |
| 记录字段/范围/industry | 13 | PASS |
| 重复键/身份冲突 | 4 | PASS |
| 前版本（消失/新增/相同） | 3 | PASS |
| 优先级 | 3 | PASS |
| 真实 gen-2（验收/消失/新增） | 3 | PASS |
| 类型边界 | 23 | PASS |
| 自测 `validate` | exit 1 | |

## 4. 范围边界

不含 publish、SQLite head CAS、服务、部署。

## 5. 验证命令

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p2/catalog_validate_probe.mjs .` | 0（57 PASS） |
| `node ... validate`（自测负证） | 1 |
| `rustc` val_oracle | 0 |
| `sha256sum -c` / `git diff --check` | 0 |
