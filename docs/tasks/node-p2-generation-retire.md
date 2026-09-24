# Node 迁移 P2 — 仅证据代际字节退役

任务：NM-037（agent-coordination/node-migration）。日期：2026-09-24。
基线：`ce120d2fd5f067d6e4a6a8d316ceea98eff4097f`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；#260 代际激活；`src/generation/mod.rs::retire_bytes`。
证据：`evidence/node-migration/p2/generation_retire/`（probe-results.jsonl 12 行全 PASS、rust-oracle.jsonl、negative-selftest.jsonl、results.json、SHA256SUMS、README）。

**结果：12/12 PASS / 0 FAIL —— 9 例 live Rust oracle 逐字段一致。**

## 1. 移植

- `retireBytes(gid)`：fd 固定代际目录（`O_DIRECTORY|O_NOFOLLOW` 祖先）→ `unlinkat` 逐一删除 base.zip/gbbq.zip/hsjday.zip（ENOENT 幂等、EISDIR/其他 → `io`）→ fsync 固定 fd
- 保留：snapshot-manifest.json / consistency-decision.json / compatibility-evidence.json / 目录本体（oracle listing 逐字段一致）
- 新增原生导出 `unlinkat_file`：unlink 不跟随末级符号链接——符号链接工件按链接删除，外部目标字节不变
- **不检查** active/pin/授权（与 Rust 一致，调用方前置）

## 2. 严于 Rust（已记录）

- gid 边界拒绝 `../x`（Rust 解析后为 not_installed）
- 符号链接代际目录拒绝 not_installed（Rust is_dir 跟随）

## 3. 验证矩阵（12 例）

| 组 | 例数 | 结果 |
|---|---|---|
| oracle（工件集/缺代际/首退役/保留清单/幂等/部分缺失/目录工件 io/坏 gid） | 9 | PASS |
| 符号链接工件删除 + 符号链接代际目录拒绝 + 外部不变 | 3 | PASS |
| 自测 `retire` | exit 1 | |

## 4. 范围边界

无服务确认令牌、active/pin 策略、DB 更新、生产就绪声明；崩溃中途退役 NOT_RUN。

## 5. 验证命令

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p2/generation_retire_probe.mjs .` | 0（12 PASS） |
| `node ... retire`（自测） | 1 |
| NM-036/035/034/033/032/026 回归 | 0（35/23/45/38/28/72） |
| `rustc` retire_oracle | 0 |
| `sha256sum -c` / `git diff --check` | 0 |
