# Node 迁移 P3 — 市场记录编解码与校验（compat::check 差异验证）

任务：NM-050（agent-coordination/node-migration）。日期：2026-09-24。
基线：`7d749bfcfd3bbea659475f6fc43fcff2ca3fa1f1`（`migration/node-devin`）。Node v20.19.2。
关联：Epic #271；`src/market_storage/compat.rs`、`src/publish.rs` 验证器、`src/market_data.rs` 类型。
证据：`evidence/node-migration/p3/market_record_codec/`。

**结果：9 场景 / 73 操作 / 0 DIFF（market_records 14 ops 全 MATCH）。**

## 1. 范围

`src_node/market/record_codec.mjs`：

- `legacyDigest`：clone Value → 移除 `integrity_sha256` → sha256(serde 序字节)；`DIGEST_FORMAT` 逐字；issue79 三个 fixture 字节级一致。
- `check`：逐字顺序——digest 校验 → `EffectiveVersion` 解析 → `batch` 解析（错误包装 `publication evidence missing:`）→ 100000 行预算 → `validate_and_deduplicate` → unit_currency → adjustment_evidence → 全量 identity 交叉检查。
- serde 忠实类型解析：必填/Option/`#[serde(default)]`、枚举值（`Day or Year` 两值 `or` 句式；三值以上 `one of`）、Decimal 值语义等值、NaiveDate 日历有效性、未知字段容忍。
- `validateAndDeduplicate`：自然键去重（day=days_from_ce/year）、同内容计数、冲突冲突 `conflicting duplicate market data key`、排序 + ValidationSummary。

## 2. 差异验证

`check_record` op 加入 NM-049 两侧 runner：读同一记录 JSON → `compat::check` vs `check` → ok(version_id)/err(code,message) 比对（serde 错误尾部 `at line N column M` 剥离）。

14 个用例：valid daily/null/yearly、坏 digest、symbol mismatch、冲突/相同 dup、high<low、unit+adjustment evidence 缺失、yearly 无 derivation、serde missing-field/unknown-variant、超预算。

## 3. 范围边界

无 storage 读写路径（read_record/range/stage/publish）、无 envelope 展开、无 compact backend、无服务。

## 4. 验证命令

| 命令 | 退出码 |
|---|---|
| `node scripts/node_migration_p3/diff_harness_probe.mjs .` | 0（9 场景 MATCH） |
| `node ... diff`（自测） | 1 |
| NM-049..NM-035 回归 | 0 |
| `sha256sum -c` / `git diff --check` | 0 |
