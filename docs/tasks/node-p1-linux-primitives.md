# Node 迁移 P1 — Linux 原语适配层验证

任务：NM-008（agent-coordination/node-migration）。日期：2026-09-23。
基线：`2364c7de8513891395cb476cac2d356ade77f3cd`（`migration/node-devin`）。Node v20.19.2，euid 1000。
关联：Epic #271。上游 oracle：NM-004 恢复/守卫实测（`evidence/node-migration/p0/recovery/`）。
证据：`evidence/node-migration/p1/linux/`（probe-results.jsonl 21 行、results.json、SHA256SUMS、README）。

**结果：18 PASS / 2 NOT_RUN / 1 INFO / 0 FAIL —— 与 NM-004 Rust fail-closed 行为零失配。**

## 1. 原语 → Node 路径选择（实测驱动）

| 原语 | Node 路径 | 状态 | 证据 |
|---|---|---|---|
| fstatfs 白名单 | `fs.statfsSync().type`（内建，Node ≥18.15）对照 `supported()` 魔数集 {0xef53, 0x58465342, 0x9123683e, 0x01021994} | PASS | tmpfs 放行、overlayfs（0x794c7630）拒绝、全表与 Rust 单测一致 |
| private_metadata | `fs.lstatSync`：uid==geteuid() 且 (mode&0o077)==0 且非 symlink | PASS | 0700 放行；0755 目录/文件、symlink 目录/文件逐字拒绝 |
| O_NOFOLLOW | `fs.openSync(O_RDONLY\|O_NOFOLLOW)` | PASS | symlink → ELOOP |
| flock 租约 | `/usr/bin/flock -n <file> sleep infinity` 辅助进程持锁 | PASS | 竞争方即时 exit 1（= Rust "operation would block"） |
| inode 身份 | `statSync` dev+ino 比较 | PASS | 文件替换即变 |
| socket owner/no-clobber | `node:net` + `node:fs` 复刻 `Endpoint::bind` 顺序 | PASS | 7 子例：未注册文件拒、owner 标记篡改拒、异库身份拒、SIGKILL 陈旧 socket 清理重绑（0600）、活跃端点拒、非私有父目录拒、裸相对路径拒 |
| SO_PEERCRED | **无内建 API**；选 N-API/neon 小助手（待工具链） | NOT_RUN | 详见 §3 |

被拒绝的替代：`fs-ext`（node-gyp 需 make，缺失）、`proper-lockfile`（语义错误：无 inode 身份绑定）、PID 文件（AC 禁止降级）、system package 安装（禁止）。

## 2. 与 Rust 行为的对应关系

- 拒绝文案逐字复刻：`runtime path must be owned by this UID, private, and not a symlink`、`unregistered existing endpoint; no clobber`、`endpoint belongs to another database`、`existing endpoint is not a socket`、`endpoint already active`、`service requires a supported local filesystem`。
- `Endpoint::bind` 顺序镜像：父目录私有 → owner.json noclobber 创建或校验（私有元数据 + O_NOFOLLOW ≤1024B 读 + 严格 {device,inode} 形状 + 身份相等）→ 既有 socket 私有元数据 + isSocket + connect 探测（活跃拒/ECONNREFUSED 陈旧 → inode 复验后删除）→ bind + chmod 0600 + 记录身份。
- SIGKILL 场景：Node `server.close()` 会删除 socket；陈旧 socket 以 SIGKILL 子进程制造（与 NM-004 Rust 探针同构），重绑成功。

## 3. SO_PEERCRED —— 诚实缺口

- Rust `Endpoint::authorize` = `getsockopt(SO_PEERCRED)` → `ucred.uid == geteuid`，Node 无对应 API，Box 上 `make` 缺失无法构建原生 addon。
- 已选路径：**最小 N-API/neon 助手**（单函数 `getpeereuid(fd)`），工具链到位时补做；不降级为 /proc 扫描（竞态）。
- 过渡不变式：socket 位于 0700 私有目录 → 跨 UID 无法遍历到达；SO_PEERCRED 是纵深防御层，parity 仍需助手。
- 同类未测：跨 UID 拒绝用例（Box 单 UID）→ NOT_RUN，与 NM-004 边界一致。

## 4. 边界声明

- 租约由**辅助进程**持有而非主进程 fd——互斥语义等价，崩溃生命周期不同（辅助进程随父退出），已记录。
- fstatfs 为路径基（Rust 为 fd 基）：挂载点在检查瞬间换 fs 的极小窗口存在，实际不可发生（无权限）——记录为差异。
- 不主张生产等价；Linux 原语之外的 business 语义未迁移。

## 5. 验证记录

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p1/linux_probe.mjs /dev/shm/nm008` | 0（18 PASS/2 NOT_RUN/1 INFO） |
| `npm install fs-ext`（拒绝候选验证） | 1（gyp: not found: make — 拒绝证据） |
| `json.tool` / `sha256sum -c` / `git diff --check`（仅本次文件） | 0 |

## 6. 范围声明

仅新增 `scripts/node_migration_p1/linux_probe.mjs`、`docs/tasks/node-p1-linux-primitives.md`、`evidence/node-migration/p1/linux/`；未装系统包（flock 为既有 util-linux）；无 Rust 改动、生产 DB、ECS、push/merge/deploy。
