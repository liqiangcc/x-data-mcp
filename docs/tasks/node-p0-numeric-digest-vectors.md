# Node 迁移 P0 — 精确数值与摘要 golden 向量

任务：NM-003（agent-coordination/node-migration）。日期：2026-09-23。
基线：`32c29e0c57832b74925316ec8b601c16d5696fae`（`migration/node-devin`）。Oracle 代码 SHA = 基线 SHA（44 行公共库函数真实输出 + 6 行镜像私有表达式输出 + 1 行论证 NOT_RUN 标记）。
关联：Epic #271、#262（OPEN，不授权迁移范围）。上游：`node-p0-inventory.md`、`node-p0-mcp-contract.md`。
证据：`evidence/node-migration/p0/numeric/`（vectors.jsonl 51 行、summary.json、SHA256SUMS、README 复现步骤）；harness 源码 `scripts/node_migration_p0/oracle.rs`（单文件；输入为源码字面量 + 一个已提交 fixture 文件 `tests/fixtures/issue79/legacy-records.json`，无 env/时钟/随机性）。

**Provenance 三分**（逐行 `provenance` 字段，附 `oracle_sha` 与 `run_ref` 可回溯至 summary.json 的 `runs` 命令/退出码）：

- `observed_public_api`（44 行）：公共库函数在 `nm003-oracle-run-1` 中的实际返回/错误。
- `source_derived_expression`（6 行，`amount_f32_projection`）：逐字镜像 `src/official_market.rs:396-397` 的**私有**表达式输出；外层 `materialize_daily` env 路径 NOT_RUN。这不是公共 API 输出。
- `justified_not_run`（1 行，`digest_rfc8785/reject_boundary`）：`serde_json_canonicalizer::to_string` 对 `serde_json::Value` 是全函数，输入域内不存在拒绝用例；拒绝语义由 `canonical_evidence` 层（3 个 reject 向量）承担。

## 1. 向量族与关键观察值

| 族 | 覆盖 | 关键观察值 |
|---|---|---|
| `day_parse` | 2 记录合法 + 3 拒绝 | 8×u32 LE/记录、日期严格递增；`Truncated(31)`、`InvalidDate{20241301@0}`、`DateOrder(1)` |
| `amount_f32_projection` | 6 例 | `f32::to_bits(150225000.0)` → `"150224992"`（f32 不可精确表示，shortest round-trip 为 150224992）；`0.1f32` → `"0.10000000149011612"`；NaN/±inf → `None`（`.ok()` 吞掉 from_str 失败） |
| `gbbq_factor_normalize` | 6 例 | `0.1f32`→`"0.10"`；`1.005f32`→`"1.00"`（f64 真值 1.0049999952316284，HALF_UP 不升）；`-0.001f32`→`"0.00"`；NaN/±inf→`None`（fail-closed） |
| `qfq_affine` | 3 例 | event_transform c=[2,5,0,1] → a=`0.9090…91`、b=`0.2727…27`（28dp 不取整）；compose_chain 两事件 → a=`0.7575…76`、b=`0.1439…39`；displayed_previous_close(10.00) → `9.36` |
| `qfq_forward_adjust` | 2 合法 + 1 拒绝 | 事件日 2024-01-04 除权：前两 bar 调整为 close 14.00/14.14（2dp HALF_UP），事件日 bar 不动；空事件 = 恒等；NaN 因子 → `GBBQ factor 0 is not a finite decimal input` |
| `yearly_aggregate` | 2 合法 + 3 拒绝 | 2023: open=首 10 / high=max 12 / low=min 9.5 / close=末 11.8 / volume=300 / amount=3410 精确和；`year_unfinished` 按 as_of；任一 bar `amount=None` → 该年 amount=null；MAX×4 → `InexactSum`（无回绕）；缺年 → `MissingYear(2025)`；乱序 → `InvalidInput` |
| `metric_ratio_change` | 5 合法 + 1 拒绝 | (105,100)→`0.050000000000`；(1,3)→`-0.666666666667`；BigInt 精确有理 half-even 12dp 单次取整；**平局偶商**：(8193,8192)→`0.000122070312`（10¹²/8192=122070312.5，商偶不进位）；**平局奇商**：(8195,8192)→`0.000366210938`（366210937.5→938）；anchor≤0 → `positive ratio inputs required` |
| `digest_legacy_published` | 2 合法 + 1 拒绝 | fixture record0 → `6192fbe5…b277e1`（sha256 of serde_json bytes minus `integrity_sha256`）；非对象 → `MARKET_INTEGRITY: record must be object` |
| `digest_canonical_evidence` | 1 合法 + 3 拒绝 | Version 证据 → JCS 键序 canonical_json + `x-data-evidence-v1\0` 域分离 sha256；内嵌 `records` 数组 → `evidence embeds a bars/records array`；形状不符 → `version evidence shape mismatch`；非对象 → `evidence is not a JSON object` |
| `digest_rfc8785` | 1 例 + reject NOT_RUN | `{"z":1,"a":{"y":[true,null]},"é":2}` → `{"a":{"y":[true,null]},"z":1,"é":2}`（JCS；é 不转义）；reject/boundary 经论证 NOT_RUN（全函数无域内拒绝） |
| `compact_bar` | 1 合法 + 5 拒绝 | Bar → trade_day=epoch(19724)、OHLC→整数分、amount `"150225000"` 规范十进制文本；`15.105` → `price is not exactly 2dp`；Year+r7 → `yearly bar must not carry r7_raw`；`"1.230"` → `not canonical`；`"abc"` → `not a decimal`；`"-5"` → `negative amount` |
| `segment_digest` | 1 合法 + 2 拒绝 | 2-bar 段 `sh.600006`/day → `660ecea1…2f93c1`（`x-data-segment-v1\0` 二进制帧 sha256）；空段 → `row_count out of bounds`；实体非 `sh./sz.+6位` → `non-canonical entity` |

## 2. 对 Node 实现的语义要点（从观察值提炼，非新需求）

- **f32→Decimal 投影**：`f32 bits → f64 精确提升 → 最短 round-trip 十进制文本 → Decimal`；无取整，NaN/inf 投影为 None。Node 侧需等价 `f32` 解码 + shortest-round-trip（JS `Number` 到字符串规则不同，需验证 `"150224992"` 类输出）。
- **HALF_UP（MidpointAwayFromZero）**：normalize/QFQ 输出/昨结均在 2dp 做一次；链内合成**不取整**（28dp Decimal 全精度）。
- **BigInt half-even**：ratio_change 把 (end−anchor)/anchor 化为精确整数商余，仅在 12dp 输出处做一次 half-even；平局按商奇偶进位（偶舍奇入）。512-bit 预算对 Decimal 输入（96-bit mantissa、scale≤28 → ≤~230bit）**不可达**，记为 source-derived 说明。
- **None 语义**：任一年度字段出现 None → 该年该字段为 unknown（null），不置零、不影响其他年。
- **摘要三风格**：legacy published = `sha256(serde_json_bytes minus integrity_sha256)`；compact evidence = `sha256("x-data-evidence-v1\0"+kind+JCS_bytes)`；segment = `sha256("x-data-segment-v1\0"+entity+period+count+bar帧)`。canonical_decimal 为词法规则（无前导零、小数位无尾随零、无 `-0`）。

## 3. 验证记录

| 命令 | 退出码 | 结果 |
|---|---|---|
| `cargo build --locked`（rustup 1.98.1, TMPDIR=/dev/shm） | 0 | lib rlib |
| `rustc --edition 2024 --test … tests/{qfq,consecutive,day_file,yearly,gbbq,gbbq_semantic,storage_roundtrip,sqlite_market}.rs` + 逐一执行 | 0/测试全过 | 6+7+3+6+2+7+1+19 = **51 passed, 0 failed**（yearly 内 1 ignored 计时探针） |
| `rustc … oracle.rs && ./nm003-oracle` | 0 | 50 向量 JSONL |
| `python3 -m json.tool summary.json`、逐行 json.loads vectors | 0 | 合法 |
| `sha256sum` | 0 | 写入 SHA256SUMS |
| `git diff --check` | 0 | 干净 |

环境事件：overlay 磁盘在执行期间打满（120G/126G）；cargo test 全量编译失败（ENOSPC），改用 `rustc --test` 直接对既有 rlib 编译目标测试文件（同一代码路径，记录于此）；oracle/测试二进制已删回收约 780MB。此为环境限制非 oracle 失配，不触发 BLOCKED。

## 4. NOT_RUN / 剩余缺口

- `official_market::materialize_daily` 端到端（env-pinned 官方快照路径）——投影表达式已逐字镜像并标注 source-derived。
- `metrics::consecutive::evaluate` 完整 Outcome（run 检测/边界语义）——`ratio_change` 已冻结；evaluate 经 compute_metrics 属后续故障/执行切片。
- `gbbq::decode_events`/`decrypt_record` 真实 GBBQ 字节解码（需要 retained 成员字节；结构解析已覆盖）。
- `digest_rfc8785` reject/boundary：全函数无域内拒绝，justified NOT_RUN（见 vectors.jsonl 第 42 行 not_run 标记）。
- 非 Linux 差异、官方输入、ECS：NOT_RUN/未访问。

## 5. 范围声明

仅新增 `docs/tasks/node-p0-numeric-digest-vectors.md`、`evidence/node-migration/p0/numeric/`、`scripts/node_migration_p0/oracle.rs`；未改运行时源码、既有测试、迁移、AGENTS/README、会话文件；无 push/merge/deploy/ECS/官方输入/真实 DB。性能预算（50%/1.2x）仍未冻结。
