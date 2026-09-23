# Node 迁移 P1 — 原生 Linux 绑定验证（SO_PEERCRED / flock fd）

任务：NM-009（agent-coordination/node-migration）。日期：2026-09-23。
基线：`67c647992cf1d898f2f4f50f773886766e35efd7`（`migration/node-devin`）。Node v20.19.2，euid 1000。
关联：Epic #271。补 NM-008 的 SO_PEERCRED/真 fd 租约缺口。
证据：`evidence/node-migration/p1/native/`（probe-results.jsonl 11 行、results.json、SHA256SUMS、README）。

**结果：9 PASS / 1 NOT_RUN / 1 INFO / 0 FAIL —— 真内核 syscall，非模拟；非 BLOCKED。**

## 1. 实现：最小 N-API addon（~80 行 C，两个函数）

`scripts/node_migration_p1/native/linux_guards.c`：

| 导出 | 系统调用 | Rust 对应 |
|---|---|---|
| `getpeereuid(fd)` | `getsockopt(fd, SOL_SOCKET, SO_PEERCRED, &ucred, &len)` + `len==sizeof(ucred)` 形状校验 | `paths.rs::peer()` — ucred.uid==geteuid |
| `flockfd(fd, op)` | `flock(fd, op)` 返回 0 或 errno | `fs2::try_lock()` — LOCK_EX\|LOCK_NB |

构建：`gcc -shared -fPIC -O2 -o linux_guards.node linux_guards.c -I/usr/include/node`（exit 0）。
工具链事实：gcc 14.2.0 + `libnode-dev 20.19.2` 头文件（系统已装，未安装新包）；**make/node-gyp 未用**。
`.node` 产物不入库（可再生 ABI 工件）；源文件 + 构建命令 + 哈希记录于 results.json。

## 2. 实测语义

| 用例 | 证据 |
|---|---|
| live unix socket 同 UID 授权 | 已接受连接 fd=23 → `getpeereuid` 返回内核 ucred {pid, uid:1000, gid:1000} → PASS |
| 非 socket fd | getsockopt 失败 → 拒绝路径 PASS |
| 跨 UID 拒绝 | NOT_RUN（单 UID，内核 ucred 不可伪造；拒绝分支为一行 uid!=euid） |
| 获取/释放/重获取 | 0/0/0，close 释放 PASS |
| 第二 OFD 竞争 | EWOULDBLOCK(11) PASS |
| **内核强制证明** | 持锁期间外部 `flock -n` exit 1 —— 真内核租约 |
| 进程释放 | close fd 后新 fd 获取 0 |
| 跨进程竞争 | 子 Node 进程 flockfd → 11 |
| 非法 fd | EBADF(9) |

## 3. 已排除的替代（与 NM-008 一致）

- `fs-ext`：node-gyp 需 make —— 已实测失败。
- `koffi` FFI：可行备选但 addon 更轻（零依赖）；未采用。
- subprocess 持锁（NM-008 过渡）：本任务被真 fd 租约取代；`flock` 二进制仅作外部强制证明。
- PID 文件：AC 禁止，未使用。

## 4. 边界与后续

- `socket._handle.fd` 为 Node 内部访问器 —— v20.19.2 实测有效，升 Node 24 时须复验。
- 生产构建需工具链步骤（prebuildify/node-gyp 或固定 gcc 调用）；配方已记录。
- 跨 UID 授权拒绝分支在单 UID Box 上无法实测 —— 与 NM-004 边界一致，非覆盖性声明。

## 5. 验证记录

| 命令 | 退出码 |
|---|---|
| `gcc -shared -fPIC -O2 -o linux_guards.node linux_guards.c -I/usr/include/node` | 0 |
| `node scripts/node_migration_p1/native_probe.mjs /dev/shm/nm009` | 0（9 PASS/1 NOT_RUN/1 INFO） |
| `json.tool` / `sha256sum -c` / `git diff --check`（本次文件） | 0 |

## 6. 范围声明

仅新增 `scripts/node_migration_p1/native/`（.c 源码）、`scripts/node_migration_p1/native_probe.mjs`、`docs/tasks/node-p1-native-linux.md`、`evidence/node-migration/p1/native/`；无 Rust 改动、系统包安装、生产 DB、ECS、push/merge/deploy。
