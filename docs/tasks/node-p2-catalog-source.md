# Node 迁移 P2 — 官方目录候选装配（source candidate）

任务：NM-028（agent-coordination/node-migration）。日期：2026-09-23。
基线：`6080502c1ce3d7a5d7c9d21b790dc72c50c2c9ee`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；#6 P1 ruling 5710586446；`src/catalog_tdx/source.rs` S0–S6；已验收 NM-021/022/023/024/025/026 组件。
证据：`evidence/node-migration/p2/catalog_source/`（probe-results.jsonl 8 行全 PASS、rust-oracle.jsonl、negative-selftest.jsonl、results.json、SHA256SUMS、README）。

**结果：8/8 PASS / 0 FAIL —— 真实 gen-2 全 serde 候选与 live Rust 逐位一致（5,224 条记录）。**

## 1. 移植：`src_node/catalog/source_candidate.mjs`

`assembleCandidate(target, manifestPath, inputRoot, now)` —— 纯只读，无 SQLite/网络/落盘提取/服务接线。

- 范围门最先：`target.scope != "current_sh_sz_a"` → `CATALOG_REJECTED`，不做任何 I/O
- manifest 有界读 1MiB+1 → 已验收 `snapshot.inspect`（NM-026）
- 角色→工件路径；`acquired_at` = 完成时间戳最大值，按完整 ns+闰秒排序键（非字符串比较）
- `base.zip!tdxhy.cfg` / `base.zip!infoharbor_ex.code` / `gbbq.zip!zhb.zip!incon.dat`；成员声明大小界 64MiB
- Rust `member()` 三段逐字：`unavailable` / `exceeds the bounded size limit` / `unreadable`；归档级 `{name} invalid`
- scope → 对账恒等式 → compose → 候选 + 发布前证据（published_count、reject_candidate 逐字排除、missing_industry、t_code_residuals、t_dictionary_checked、scope_partition）
- 零可发布成员 → `CATALOG_REJECTED: no publishable members; nameless scope members: <逗号表>`

候选以 Rust serde 形状输出（`Exchange` Shanghai/Shenzhen、`AShare`、`{Assigned:{code,name,scheme}}`/`{Missing:{reason}}`、`{Complete:{expected_count}}`）——即从 NM-025 内部标签映射。

新生产库 `src_node/lib/zip_member.mjs`（NM-024 R3 加固语义）：`zipOpen` 结构校验 / `zipMemberSize` 声明大小 / `zipMember` 有界提取。

## 2. 治理边界

快照字节核验不蕴含跨文件兼容性、来源授权或生产就绪——此处均不推断；不插入证明；无 publish/SQLite/CLI。

## 3. Node 差异（已记录）

内层 io/zip 错误文本与 Rust `io::Error`/`zip::ZipError` Display 不同（外层 `CATALOG_*` 前缀、成员名、解析器文本逐字）；合成夹具 zip 字节与 oracle 构建器不同 → `source`（清单绑定）对合成例归一化，real_gen2 逐字核验 `official-file:b8a0d7b9aec1a270`。

## 4. 验证矩阵（8 例）

| 例 | 结果 |
|---|---|
| ok_small（合成） | PASS（全结构比对，source 归一化） |
| unsupported_scope | PASS（I/O 前 `CATALOG_REJECTED` 逐字） |
| manifest_mismatch | PASS（`snapshot inspection failed: artifact bytes do not match manifest: Day` 逐字） |
| member_missing / member_corrupt / member_oversize | PASS（逐字） |
| empty_candidate | PASS（`CATALOG_REJECTED` + 无名成员表逐字） |
| real_gen2 | PASS（全 serde 候选逐位一致；n=5224、6 无名排除、0 缺失行业、0 残码、t 字典已查、分区 5659/5230/348/78/3） |
| 自测 `source` | exit 1 |

## 5. 范围边界

不含 publish_official/SQLite/head、下载、代际激活、服务/CLI、部署。

## 6. 验证命令

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p2/catalog_source_probe.mjs .` | 0（8 PASS） |
| `node ... source`（自测负证） | 1 |
| `rustc` oracle（`target/debug/src_oracle`） | 0 |
| `sha256sum -c` / `git diff --check` | 0 |
