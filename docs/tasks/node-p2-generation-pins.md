# Node 迁移 P2 — 代际 pin 生命周期

任务：NM-036（agent-coordination/node-migration）。日期：2026-09-24。
基线：`1c5c34087cc17dc0d26f1017f04a0698b64ef2f8`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；#260 代际激活；`src/generation/mod.rs::{pin,pins,unpin,sweep_pins,Pin}`。
证据：`evidence/node-migration/p2/generation_pins/`（probe-results.jsonl 26 行全 PASS、rust-oracle.jsonl、negative-selftest.jsonl、results.json、SHA256SUMS、README）。

**结果：26/26 PASS / 0 FAIL —— 19 例 live Rust oracle 逐字段一致。**

## 1. 移植（`generation_store.mjs` 扩展 + `Pin` 类）

- `pin(gid, holder)`：代际目录存在门 → holder 校验（空/`/`/`\`/NUL → `snapshot inspection failed: invalid pin holder` 逐字）→ `pins/<gid>` 创建 → holder 文件 write+create+truncate → 文件 fsync → 目录 fsync → 返回 `Pin`
- `pins(gid)`：仅常规文件、wellformed 名称、**UTF-8 字节序**排序（`Mango`<`apple`<`zebra`<`\ue000`<`😀`，oracle 实测）
- `unpin`：ENOENT 幂等 + pin 目录 fsync（目录缺席 → `io`，逐字）
- `sweepPins`：keep 谓词反向删除，返回删除列表
- `Pin.release()`/`Symbol.dispose`：Rust Drop 语义——Node 20 无 `using`，显式 release 为确定性路径；GC 终结器刻意不用

## 2. 严于 Rust（已记录）

- holder marker 打开 `O_NOFOLLOW`——预置符号链接拒绝且外部目标不变（`holder_symlink_reject`）
- 代际 ID 边界检查；holder 孤立代理拒绝

## 3. 验证矩阵（26 例）

| 组 | 例数 | 结果 |
|---|---|---|
| oracle（缺代际/坏 holder×4/首 pin/marker 元数据/重复/RAII drop/排序/缺代际列表/unpin×2/列表/清扫/缺席清扫/无目录 unpin/跳过子目录） | 19 | PASS |
| RAII（release/Symbol.dispose/幂等）+ holder symlink + surrogate + 存活 marker | 7 | PASS |
| 自测 `pin` | exit 1 | |

## 4. 范围边界

合成 root；不实现 retire（存活 marker 作为未来 retire 前置证明）；无并发策略（调用方单写者语义）；reopen 非崩溃持久性证明。

## 5. 验证命令

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p2/generation_pin_probe.mjs .` | 0（26 PASS） |
| `node ... pin`（自测） | 1 |
| NM-035/034/033/032/026 回归 | 0（23/45/38/28/72） |
| `rustc` pin_oracle | 0 |
| `sha256sum -c` / `git diff --check` | 0 |
