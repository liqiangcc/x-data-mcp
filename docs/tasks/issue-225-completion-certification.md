# Issue #225——生产级 `analysis_coverage` 认证路径：设计分析与决策点

状态：**FG 已裁定（comment-5750013426），已实现**。D1=A 基线相对完整，
D2=C 读取时推导（`sqlite_metrics::observations` 从嵌入的
`local_yearly_derivation_v1.evidence` + `yearly_bars.is_finished` 机械计算），
D3=无新签发者。本文保留为设计依据记录。

## 0. 代码事实（决定选项空间的边界）

- `compute_metrics` 正向需要每观测 `Completion`；唯一来源是版本记录上的
  `analysis_coverage_v1` 字段（`src/sqlite_metrics.rs:57`）。
- `Origin` 枚举只有 `Fixture` 一个变体；非 `fixture:` 源携带该字段被硬拒
  （`fixture coverage cannot certify a production source`）。
- yearly 派生（`src/yearly_publication.rs`）显式**不写**
  `analysis_coverage_v1`，但已嵌入 `local_yearly_derivation_v1` =
  `{annual, evidence, base_acquired_at}`——其中 `evidence` 是
  `DerivationEvidence` 逐字（含 `coverage.status=Complete` +
  reason），且整条记录由 `integrity_sha256` 绑定。
- 派生证据的签发者已存在：`OfficialYearlyVerifier`
  （`src/yearly_publication/runner.rs:26`）——env-installed 官方源门控，
  通过"对固定 base 重聚合请求年份"签发 `coverage=Complete`，reason 明确
  声明"selected range verified … not a listing-history claim"。
- `yearly_bars` 已存每 `(version_id, year)` 的
  `first_trade_date/last_trade_date/is_finished`；`is_finished=false` 当且
  仅当 `year == as_of.year()`（`src/yearly.rs:128`）。
- 仓库内**没有交易日历权威**：`calendar.rs` 只做上海时区日期换算。
  "自然年所有交易日都在场"无法用现有证据源核验。

## 1. 决策点 D1：完成度声明的证据形式

"该 yearly 版本第 Y 年完整"可以指三种强度不同的东西：

| 选项 | 语义 | 可行性 |
|---|---|---|
| **A. 基线相对完整** | 该年的 yearly bar 由固定 base 聚合而成，base 覆盖证据 Complete，且年已完结（`is_finished`） | 现有证据足够，机械可复核 |
| **B. 自然年完整** | A + base 覆盖该年全部交易日 | 需要交易日历权威——不存在；若选此语义必须先开新 Issue 解决日历来源 |
| **C. 上市史完整** | B + base 覆盖证券全部上市区间 | 同上，且额外需要上市日期权威 |

**事实约束**：B/C 在现有证据源下不可认证。A 是唯一当前可达且可审计的
语义。若 FG 认为指标消费者需要 B/C 强度，则本 Issue 前置阻塞于日历
证据源 Issue，而不是弱化 A 冒充 B。

## 2. 决策点 D2：证据载体

| 选项 | 内容 | 代价/风险 |
|---|---|---|
| **A. 扩展 `analysis_coverage_v1`** | 新增 `Origin::Derived{verifier, base_sha256,…}`；`sqlite_metrics` 的 fixture 检查改为 origin-aware | 复用现有校验骨架（年份↔记录对应、evidence_ref 非空、≤1000 年预算）；风险：fixture 语义泄漏到生产路径，需重写硬拒逻辑 |
| **B. 新字段 `analysis_coverage_v2`** | 独立 schema，只接受生产源签名 | 干净断裂，但多一条存储/校验管线 |
| **C. 不存声明，读取时推导** | `observations()` 从已嵌入的 `local_yearly_derivation_v1` + `yearly_bars.is_finished` 机械计算 completion | 零新增伪造面；语义固化在读取层 |

**关键观察**：派生记录已把 verifier 证据逐字嵌入并由
`integrity_sha256` 绑定——选项 C 下"完成度"是纯函数
`f(嵌入证据, is_finished, coverage.status)`，无需新签发者即可审计、
可离线复现。A/B 存的声明不会比嵌入证据携带更多信息，只增加一个需要
防伪/防重放的签名面。

## 3. 决策点 D3：签发者

- 若 D2=C：无新签发者。信任根仍是 `OfficialYearlyVerifier` 的
  DerivationEvidence（官方源门控 + 重聚合核验），完成度声明由读取层
  机械导出，缺失/不完整证据 → `Unknown` fail-closed。
- 若 D2=A/B：签发者 = `OfficialYearlyVerifier` 扩展输出（per-year
  status），读取侧机械复核绑定（version_id + base sha + 年集合）。
  独立验证器无必要——声明本身可由 (base, years, as_of) 重算校验。

## 4. 建议（供裁定，非结论）

**D1=A（基线相对完整）+ D2=C（读取时推导）**：

- 现有证据链不动：`DerivationEvidence` 继续只声明"所选范围已核验"，
  原因字符串原样保留"not a listing-history claim"。
- `observations()` 对携带有效 `local_yearly_derivation_v1` 的生产源
  yearly 版本：`coverage.status==Complete && !is_finished` →
  `Completion::Complete`；`is_finished=false` → `Incomplete`；证据缺失/
  不绑定 → `Unknown`。语义明确写进契约文档：Complete = "相对固定基线
  完整且年已完结"，不声明自然年/上市史完整。
- 现存拒绝路径全部保留：fixture 字段边界不动（`analysis_coverage_v1`
  仍只允许 `fixture:` 源携带），伪造 version/digest 仍硬拒。
- 效果：`compute_metrics` 在真实派生 yearly 上可达 `computed`，且每个
  Complete 判定可由记录的嵌入证据机械复算。

**代价如实记录**：该语义把"完整"定义为基线相对——若某日基线本身缺
交易日（采集缺口），该年仍会被判 Complete。B/C 强度的声明需要交易日历
与上市史权威，超出当前证据源能力，建议单独立 Issue 评估。

## 5. 若裁定通过后的最小实现面（预估，非本提案承诺）

1. `sqlite_metrics::observations` 增加派生证据读取分支（fail-closed）；
2. 契约文档写明 Complete/Incomplete/Unknown 的谓词；
3. 测试：真实派生 yearly → `computed`；篡改嵌入证据 → `integrity` 拒；
   `is_finished=false` 年 → `Incomplete`；无证据版本 → `Unknown`；
4. #219 验收口径补充一条正向断言。
