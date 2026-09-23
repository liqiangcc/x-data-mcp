# Node 迁移 P1 — 原生 addon 打包与可复现性

任务：NM-010（agent-coordination/node-migration）。日期：2026-09-23。
基线：`a6338e812b50c0978614170bfd4a76bec624e4a0`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271。上游：NM-009 原生绑定。
证据：`evidence/node-migration/p1/native-packaging/`（probe-results.jsonl 7 例全 PASS、results.json、两次构建原始输出、SHA256SUMS、README）。

**结果：7/7 PASS —— 可复现构建 + fail-closed 停止规则全部实测成立；非 BLOCKED。**

## 1. 打包结构

```
scripts/node_migration_p1/native_packaging/
  build.mjs   — gcc/头文件存在性检查 → 编译 → 输出 {status,sha256,node,napi_version,modules,compiler}
  index.mjs   — fail-closed loader：.node 缺失即抛错（无 shell flock/PID 替代）
  verify.mjs  — 安装后 smoke（flock 获取/释放 + SO_PEERCRED euid 检查），exit 0/1
```

package.json scripts：`build:native`、`verify:native`、`postinstall → build:native`。

## 2. 实测

| 用例 | 结果 |
|---|---|
| 字节级可复现 | 3 次构建（NM-009 工件 + 两次全新）sha256 全为 `11289943e6…` |
| 缺头文件 | `NODE_INCLUDE=/nonexistent` → exit 1 `BLOCKED`（列出缺失 node_api.h） |
| 缺编译器 | `CC=/nonexistent-cc` → exit 1 `BLOCKED` |
| 缺工件 | `index.mjs` import 抛错 exit 3 —— 无替代机制 |
| `npm ci` 端到端 | 39 包，postinstall 构建 .node，`verify:native` PASS，0 漏洞 |
| ABI 元数据 | node v20.19.2 / napi_version 9 / node_module_version 115 随构建输出 |

## 3. 设计裁决

- **postinstall 硬失败即停止规则**：缺 gcc/头文件时 `npm ci`/`install` 失败——正确行为，不静默降级；纯 JS 场景可 `npm ci --ignore-scripts`，随后调用原生功能时 loader 仍会 fail-closed 抛错。
- **零新增运行时依赖**：打包层只用 `node:fs`/`node:crypto`/`node:child_process` 与既有 gcc+libnode-dev；无系统包变动。
- **不提交 .node 工件**：可再生 ABI 工件；源 + 配方 + 哈希入证据。字节级确定性在本工具链成立；跨编译器版本以 `verify:native` 功能检查为准（已记录）。

## 4. 边界

- 字节哈希确定性限于本工具链（gcc 14.2.0）；不同编译器字节可能不同，功能验证兜底。
- `socket._handle.fd` 内部访问器随 NM-009 记录，Node 24 需复验。
- 未做交叉编译/多发行版验证；生产分发（prebuildify 等）是后续工具链决策。

## 5. 验证记录

| 命令 | 退出码 |
|---|---|
| `npm run build:native` ×2 | 0（字节一致） |
| `NODE_INCLUDE=/nonexistent … build.mjs` / `CC=/nonexistent-cc …` | 1（BLOCKED） |
| `import index.mjs`（无 .node） | 3（抛错） |
| `npm ci` | 0（postinstall 构建成功） |
| `npm run verify:native` | 0 |
| `json.tool` / `sha256sum -c` | 0 |

## 6. 范围声明

仅新增 `scripts/node_migration_p1/native_packaging/`、package.json scripts 字段、`docs/tasks/node-p1-native-packaging.md`、`evidence/node-migration/p1/native-packaging/`。无运行时业务代码、ECS、live DB、deploy、push/merge。
