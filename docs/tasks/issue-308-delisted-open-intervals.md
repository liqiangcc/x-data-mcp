# 开区间但已退市的证券：读面实测（PR #579，只查不改）

执行：2026-11-06，VM-1-208（开发机，**不是** vm2-126，也没有 preview 库）。代码版本：PR #579 分支
（`risk-warnings-v3` / 解析器 `v7`）。全程只读：没有改动任何生产库，没有访问线上。

## 0. 结论

1. **`market.risk_warnings` 读面（`get_security_state` / `query_data`，`fact=risk_warnings`）不按退市日截断。**
   退市日当天及其后任意一天，开区间照样返回 `delisting_risk_warning`。这是**错误的已知值**，和本 PR 修的
   标题误判是同一类问题（对证券不存在的日子断言了一个状态）。**需要新任务**（见 §4）。
2. **研究读面（`member_state` / `tradability` / `price_limit` / `status_of`）是安全的**：退市日之后
   `membership = Out(AfterDelisting)`、`status = Unknown`、`tradability = NotTradable(Delisted)`、
   `price_limit = Unknown(not_listed)`。没有任何一处读成 `ST` 或 `normal`。
   一个次要瑕疵：`price_limit` 的原因码是 `not_listed`，与「从未上市」同码，分不出「已退市」；答案仍是
   `unknown`，不是错值，可以并入新任务一起改。

## 1. 为什么用夹具而不是线上库

取证（`gp29-compare-20261106.md` §7）说这批行是沪 104、深 68（`effective_to IS NULL` 且
`security_master.delisting_date ≤ 2026-11-05`），但那台机器（vm2-126）上的 preview 库不在本机，本任务
不访问线上。这两条读路径的答案**只取决于行形状**（开区间 + `delisting_date`），与具体代码无关，所以
用真实读路径函数跑一个复刻该行形状的夹具等价，§3 给出在 vm2 上复核的只读命令。

取样（各 5 个，按审查要求避开本 PR 会关闭的那 7 个 600076/600315/600368/600469/600656/600822/600966）：

- 沪（来自 GP29 §7 的 14 条取证清单）：`sh.600146`、`sh.600193`、`sh.600301`、`sh.600403`、`sh.600421`
- 深（真实已退市 A 股）：`sz.000013`、`sz.000015`、`sz.000047`、`sz.000588`、`sz.000621`

夹具：每个代码一条 `effective_from=2014-12-01, effective_to=NULL` 的
`delisting_risk_warning` 区间（沪 `exchange_announcement`、深 `szse_name_history`，与真实 `source_kind`
一致），`security_master.delisting_date` 统一设为退市日。

## 2. 实测

### 2.1 `get_security_state`（`market.risk_warnings`）

夹具退市日 = `2019-06-28`（`data_supply` 的 `seed_security_master` 固定 `effective_from=2018-01-01`，
所以退市日必须晚于 2018 才能让 `security_master` 也有行）。

| `fact` | `as_of` | `row_count` | `normalized_status` |
|---|---|---|---|
| `risk_warnings` | 2019-06-27（退市前一日） | 10 | 10 × `delisting_risk_warning` |
| `risk_warnings` | 2019-06-28（退市日） | **10** | 10 × `delisting_risk_warning` |
| `risk_warnings` | 2020-06-30（退市后一年） | **10** | 10 × `delisting_risk_warning` |
| `security_master` | 2020-06-30 | 10 | `delisting_date=2019-06-28` |

命令（临时测试，已回滚）：

```rust
let page = f.api.call("get_security_state",
    json!({"securities": entities, "fact": fact, "as_of": as_of})).unwrap();
println!("READ_PATH fact={fact} as_of={as_of} row_count={} columns={}",
         page["row_count"], serde_json::to_string(&page["columns"]).unwrap());
```

原始输出（节选）：

```
READ_PATH fact=risk_warnings as_of=2019-06-27 row_count=10 columns={"effective_from":["2014-12-01",…],"normalized_status":["delisting_risk_warning",…]}
READ_PATH fact=risk_warnings as_of=2019-06-28 row_count=10 columns={"effective_from":["2014-12-01",…],"normalized_status":["delisting_risk_warning",…]}
READ_PATH fact=risk_warnings as_of=2020-06-30 row_count=10 columns={"effective_from":["2014-12-01",…],"normalized_status":["delisting_risk_warning",…]}
READ_PATH fact=security_master as_of=2020-06-30 row_count=10 columns={"delisting_date":["2019-06-28",…]}
```

**读成什么**：退市日之后仍然读到 `delisting_risk_warning`（一个已知的 ST 值），**没有**读成 `normal`，
但也不是 `unknown`。原因在代码：`get_security_state` 把 `as_of` 折成
`Filter::Daily{start=end=as_of}` 后交给 `data_supply::query`，`market.risk_warnings` 的列存只按
`effective_from/effective_to` 判定覆盖（`src/data_supply/read_tools.rs:579`、`src/data_supply/query.rs:105`），
**整条链路都没有读 `security_master.delisting_date`**。

### 2.2 研究读面

夹具退市日 = `2026-07-08`（`src/research` 的夹具窗口）。

| 代码 | `as_of` | `membership` | `status`（`status_of` 的结果） | `tradability` | `price_limit` |
|---|---|---|---|---|---|
| `sh.600146` | 2026-07-07 | `In(ListingDate)` | `DelistingRiskWarning` | `Tradable`/`tradable` | `Known`/`known` |
| `sh.600146` | 2026-07-08 | `Out(AfterDelisting)` | **`Unknown`** | `NotTradable(Delisted)`/`delisted` | `Unknown`/`not_listed` |
| `sh.600146` | 2026-07-10 | `Out(AfterDelisting)` | **`Unknown`** | `NotTradable(Delisted)`/`delisted` | `Unknown`/`not_listed` |
| `sz.000013` | 2026-07-07 | `In(ListingDate)` | `DelistingRiskWarning` | `Tradable`/`tradable` | `Known`/`known` |
| `sz.000013` | 2026-07-08 | `Out(AfterDelisting)` | **`Unknown`** | `NotTradable(Delisted)`/`delisted` | `Unknown`/`not_listed` |
| `sz.000013` | 2026-07-10 | `Out(AfterDelisting)` | **`Unknown`** | `NotTradable(Delisted)`/`delisted` | `Unknown`/`not_listed` |

命令（临时测试，已回滚）：

```rust
let state = member_state(&c, entity, &UniverseRequest { temporal, strict: false })?.unwrap();
let trad  = tradability_in(&ctx, entity, temporal)?;
let limit = price_limit_in(&ctx, entity, temporal)?;
println!("RESEARCH entity={entity} as_of={as_of} membership={:?} status={:?} \
          tradability={:?}/{} price_limit={:?}/{}",
         state.membership, state.status, trad.state, trad.reason_code(),
         limit.state, limit.reason_code());
```

**机制**：`membership_of` 在 `as_of >= delisting_date` 时返回
`Out(NotMemberReason::AfterDelisting)`（`src/research/universe.rs:746`）；`select_from` 只对
`membership.is_member()` 的行调用 `status_of`，否则直接写 `RiskStatus::Unknown`
（`src/research/universe.rs:626`）；`tradability_in` 与 `price_limit_in` 都在看到
`Membership::Out` 时提前返回（`src/research/tradability.rs:216`、`src/research/price_limit.rs:396`）。

**`status_of` 本身没有退市日截断**（`src/research/universe.rs:865`，只看 `effective_from/effective_to`）。
它今天安全**只因为调用点先做了 membership 判断**。以后若有人绕过 `select_from` 直接调它，开区间会在
退市后继续返回 `DelistingRiskWarning`。

## 3. 在 vm2 上复核的只读命令（不在本机跑）

```bash
# 只读打开 preview 库
sqlite3 'file:/opt/x-data-mcp-preview/state/preview.sqlite?mode=ro' <<'SQL'
.mode line
-- 开区间且已退市的样本（沪 5 + 深 5）
SELECT w.exchange, w.code, w.effective_from, m.delisting_date, w.normalized_status
FROM risk_warning_intervals w
JOIN security_master m ON m.exchange=w.exchange AND m.code=w.code
WHERE w.effective_to IS NULL AND m.delisting_date <= '2026-11-05'
  AND w.code IN ('600146','600193','600301','600403','600421',
                 '000013','000015','000047','000588','000621');
SQL
```

然后用同一份库起一个只读 MCP 实例，对每个代码在「退市日 + 1 年」调
`get_security_state{"securities":[code],"fact":"risk_warnings","as_of":"<delisting+1y>"}`，预期与 §2.1
相同（`row_count=1`、`normalized_status=delisting_risk_warning`）。这一步本任务没有执行。

## 4. 建议的新任务

**要不要新任务：要。**

- 范围：`market.risk_warnings` 读面（`src/data_supply`）对 `effective_to IS NULL` 的区间按
  `security_master.delisting_date` 截断（或把退市后的日子报成 `unknown`）。影响面按取证是沪 104 行 +
  深 68 行。
- 需要先定口径：截断是在导出/列存阶段（写一次，所有读者一致），还是在 `query` 过滤阶段（读时判断）。
  前者改导出身份，后者改读契约，两者都要 owner 定，本 PR 不动。
- 顺带：`price_limit` 在 `Out(AfterDelisting)` 时把原因码从 `not_listed` 改成 `delisted`
  （`src/research/price_limit.rs:396` 的 `Membership::Out(_) => return Ok(answer)` 分支）。答案不变，
  只改标签，需要读契约的版本决定。
- 本 PR 不改这两处，也不改 schema。
