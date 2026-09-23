# Node 迁移 P2 — 目录候选纯校验（validate）

任务：NM-029（agent-coordination/node-migration）。日期：2026-09-23。
基线：`600afc4853f3ab508904bf328022021a435f6cdf`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；`src/catalog.rs::validate_snapshot`/`validate_record`；`tests/catalog.rs`。
证据：`evidence/node-migration/p2/catalog_validate/`（probe-results.jsonl 65 行全 PASS、rust-oracle.jsonl、negative-selftest.jsonl、results.json、SHA256SUMS、README）。

**结果（R3）：65/65 PASS / 0 FAIL —— 34 例 live Rust oracle 全字节一致（含 5,224 条真实 gen-2 候选）。**

## 1. 移植：`src_node/catalog/validate.mjs`

`validateSnapshot(snapshot, previous)` → `{accepted:true}` 或 `{accepted:false, reason:<serde RejectReason>}`：

- 门序：source 空白 → completeness → 记录逐字段（symbol/name/source_security_id/source）→ source 一致 → exchange/type 范围 → industry → 重复键/源 ID → 成员消失（前版本）
- `RejectReason` serde 外部标签逐字：`"EmptySnapshot"` 单元变体为裸字符串；`DuplicateSecurity{key}`、`MemberDisappeared{members}`、`IdentityConflict{message}`（`SecurityKey` Debug 逐字：`中` 字面、`U+FEFF`→`\u{feff}`、`"`→`\"`）
- Rust `trim` = Unicode `White_Space`——`U+FEFF` **不**空白（JS `.trim()` 会误删；oracle `feff_source`/`feff_symbol` 验证）
- 成员消失排序：exchange 序（Shanghai<Shenzhen<Other；Other 先比 payload UTF-8）→ symbol UTF-8 字节序
- 类型边界：非 serde 形状抛 `ValidationInputError`（类型错误、未知枚举变体、多余/缺失字段、孤立代理、坏前版本形状）——23 例

> R1（REVIEW-NM-029）：① 消失成员排序含 `Other(payload)`——payload UTF-8 先于 symbol（Rust 派生 Ord）；oracle `other_order_disappeared` 验证 Shanghai:5 < Other(a):9 < Other(z):0。② `Assigned.scheme` 为 `#[serde(default)]`——缺键视为 null（含 undefined）；未知 scheme 标签仍拒收。③ 全部时间戳字段（snapshot.records.acquired_at、previous.{published_at,acquired_at,records}）经严格 RFC3339 解析——`"junk"`/无偏移/非法日历在类型边界拒绝（原实现只查类型，会 `accepted:true` 放行）。

## 2. 纯净性

确定性、只读；无 DB/head CAS/服务/代际/网络/发布；不从计数推断全市场完备性；来源授权/兼容性证明分离。

## 3. 验证矩阵

| 组 | 例数 | 结果 |
|---|---|---|
| source/completeness 门 | 8 | PASS |
| 记录字段/范围/industry | 13 | PASS |
| 重复键/身份冲突 | 4 | PASS |
| 前版本（消失/新增/相同/Other 序） | 4 | PASS |
| 优先级 | 3 | PASS |
| 真实 gen-2（验收/消失/新增） | 3 | PASS |
| 类型边界 | 28 | PASS |
| 自测 `validate` | exit 1 | |

## 4. 范围边界

不含 publish、SQLite head CAS、服务、部署。

## 5. 验证命令

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p2/catalog_validate_probe.mjs .` | 0（65 PASS） |
| `node ... validate`（自测负证） | 1 |
| `rustc` val_oracle | 0 |
| `sha256sum -c` / `git diff --check` | 0 |

> R2（REVIEW-NM-029 R1 仅证据）：更正提交证据计数——probe-results 系在旧 34 例 oracle 上生成；重跑提交版脚本+36 例 oracle → 65 PASS。results.json 中 NM-028 回归更正为 13/13。

> R3（REVIEW-NM-029 R2 仅元数据）：results.json 的 rust_oracle 例数、groups 拆分、typed_boundary 计数与 Other 排序描述更正为与提交输出一致。
