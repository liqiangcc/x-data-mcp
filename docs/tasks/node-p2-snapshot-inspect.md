# Node 迁移 P2 — 快照清单字节检查（inspect）

任务：NM-026 R2（agent-coordination/node-migration）。日期：2026-09-23。
基线：`6c8718d48f3580688e6aa3ee258621cf55fdacb7`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；`src/snapshot.rs` + `tests/snapshot.rs` + `docs/snapshot-input.md`；gen-2 retained manifest `evidence/issue-3-acceptance-run/snapshot-manifest.json`。
证据：`evidence/node-migration/p2/snapshot_inspect/`（probe-results.jsonl 66 行全 PASS + negative-retained-sha.jsonl、rust-oracle.jsonl、results.json、SHA256SUMS、README）。

**结果（R2）：66/66 PASS / 0 FAIL —— 真实 gen-2 全 Inspection 与 live Rust 逐位一致，549MB 全程流式。**

> R1（REVIEW-NM-026）：serde Option 省略→null；strictJson 重复键拒绝（`duplicate field`）；fatal UTF-8 解码；chrono 严格 RFC3339（空格/小写分隔、必需偏移<24h、严格历法、任意分钟闰秒 :60、纳秒三位组 AutoSi）；mkdtemp + exit 钩子清理；retained pin 流式哈希；全进程 VmHWM 测量；`retained_sha` 自测 exit-1 负证。
>
> R2：null-prototype 对象（`__proto__` 未知字段拒绝）；`\u` 代理对校验（孤代理拒绝）；偏移分量边界（`+00:99`/`+23:60` 拒，`+23:59` 解析后区间判）；纳秒区间比较 ns×2 排序键（`.000000001` 反转捕获，闰秒 :60 严格位于 `:59.999999999` 与下一分钟之间）。

## 1. 移植：`src_node/governance/snapshot.mjs`

| 语义 | 实现 |
|---|---|
| 大小门限 | manifest >1MiB → `manifest exceeds 1 MiB`（先于 JSON 解析） |
| JSON/形状 | `deny_unknown_fields` + 字段类型 → `json` 类别（Node 文案与 serde 位置信息有差异——已记录） |
| 契约检查 | schema/count → dup role/path（**分量规范化相等**：内层 `.` 与重复分隔符折叠、前导 `./` CurDir + `..` ParentDir 拒收）→ 文件名按角色 → 声明 URL → 时间区间 → bytes/sha256 —— 全部 artifact 路径触碰之前 |
| URL | 去 `http(s)://` → `split_once('/')`；URL 无空白；host 不含 `@:?#`；day→`data.tdx.com.cn`，base/gbbq→`tdx.com.cn`\|`www.tdx.com.cn`；path 精确等于角色路径 |
| 文件系统 | `realpathSync` root → 每文件 realpath → root 前缀检查（外链拒绝）→ `isFile` → 64KiB 流式 sha256+计数 → 不符 → `artifact bytes do not match manifest: {PascalRole}` |
| 成功 | files 按角色序 `base_industry<day<gbbq`；`manifest_sha256` 原始字节摘要；limitation 字段逐字；`production_ready:false` |

## 2. 真实 gen-2（committed manifest + retained 字节）

manifest `b8a0d7b9…`；三个 retained zip 先对 `manifest-sha256.txt` 校验（`dcdf6b49`/`daad6290`/`c82f852a`），同文件系统硬链接挂入 mkdtemp 私有根（源只读、事后清理）→ Inspection 与 Rust 逐位一致。**有界测量**：549MB 文件 64KiB 流式 ~517ms；全进程 VmHWM 64.3MiB（含流式 retained pins）——不做性能声明。

## 3. Node 差异（已记录）

`json`/`io` 类别保持、消息文本为 Node 实现文案（serde 期望列表+行列 / `io::Error` Display 不可复刻）；时间戳重序列化 RFC3339 `Z`——纳秒三位组、闰秒字面 `:60` 重放；区间排序在 ns 尺度精确（闰秒槽位建模为 nextMin×2−1；仅同一闰秒内不同分数并列——已记录）；>9 位小数按纳秒截断。

## 4. 验证矩阵（45 例）

| 组 | 例数 | 结果 |
|---|---|---|
| 合成 oracle | 58（原 52 + `__proto__` / 孤代理 / `+00:99` / `+23:60` / `+23:59` / ns 反转；早前含 Option 省略 / dup 键×2 / 非法 UTF-8 / ts 矩阵 11 例：no-offset、空格、小写、month13、feb30、leap、leap61、offset、off24、nanos、.5fracvalid/序/嵌套/内链、超大/坏 JSON/未知字段×2、schema/计数/空、dup×2、路径×4、文件名、URL×9、区间×2、hash/size×4、缺失/非常规/外链/改字节） | PASS（invalid/mismatch 逐字；json/io 同类别） |
| 类型边界 | 3 | PASS |
| 真实 | 5（3 retained sha 流式 + Inspection + 清理） | PASS |
| 负证 | `retained_sha` selftest → exit 1 | PASS |

## 5. 范围边界

不做 inspect_snapshot CLI、source 执行器、下载、激活、SQLite、服务。

## 6. 验证记录

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p2/snapshot_probe.mjs .` | 0（66 PASS）
| `node ... retained_sha`（自测负证） | 1 |
| `rustc` oracle（`target/debug/snap_oracle`） | 0 |
| `json.tool` / `sha256sum -c` / `git diff --check` | 0 |

## 7. 范围声明

仅新增 `src_node/governance/snapshot.mjs`、`scripts/node_migration_p2/snapshot_probe.mjs`、`docs/tasks/node-p2-snapshot-inspect.md`、`evidence/node-migration/p2/snapshot_inspect/`。无既有模块/Rust 改动、live DB、ECS、deploy/push/merge。
