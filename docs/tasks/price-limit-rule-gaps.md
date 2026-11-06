# 涨跌停规则书的覆盖缺口与补法（D-82 Q8，[#433](https://github.com/liqiangcc/x-data-mcp/issues/433)）

日期：2026-11-06。代码版本：main `47a8be5`。规则书：`src/trading_rules/a7-d44-v1.json`，`snapshot_id` 为「A-7/D-44/D-64 2026-10-29 min_band_ticks」，27 行涨跌停规则、2 行最小变动单位。
依据：owner 2026-11-06 对 [可交易性设计](issue-433-tradability-read.md) §8.2 Q8 的裁决（[D-82](../decisions.md)）：缺口照实返回 `unknown`，另开数据任务补规则行，不做「缺行退回 seasoned」。本文是那个数据任务的第一步：查清缺口、算影响、判断每段能不能补。

本文只写文档，没有改规则文件，没有连线上，没有联网。标「推断」的是从代码或文档推出、没有实测的结论。标「待 vm2」的数要由有权限的人在 vm2 上用附录 A 的只读 SQL 取得，本文没有这些数。

## 0. 结论

- 规则书对「市场×板块×状态类别×上市阶段」的组合，列出 **14 个缺口或疑点**（§2 表）。其中 4 行草案（G1 沪、G1 深、G2、G3a）有已批准条款作部分支撑，值可以写，范围结论是推断；G4 仓库里没有答案，要先取证；R7、N1 两段、G3b、G5、G6 维持 `unknown`；E1、E2 不是缺行，是已有行可能用错的疑问。
- **能补的缺口只涉及一小部分 bar 日。** 全部 A 股上市后第 2–5 日的 bar 数上界是 5,565 × 4 = 22,260 根，占不复权日线 18,031,689 根的 0.12%（§3，上界按全部 A 股、全部落在窗口内算，真值要查）。
- **补主板 G1 对沪市当前没有效果。** 沪市主板状态恒为 `unknown`（D-81 保持沪市 `unknown`），`invariant_band` 要四类状态都落在同一行才给确定值，只补 `normal` 行不够（§4.1）。只有写成 `status_class='all'` 才能改变沪市结果，而那等于断言「上市第 2–5 日不可能是 ST」，需要 owner 另行接受。
- **设计 §3.2 把这些日子的原因码写成 `rule_missing`，不准确。** 读 `price_limit.rs` 得出：状态已知（有 ST 区间，或深市 T3 落地后的 `normal`）才返回 `rule_missing`；状态未知时返回 `st_unknown`，`candidates` 为空（§1.2）。
- 创业板 2020-08-24 前的 ST 幅度（G4）仓库里没有答案，也没有原文。**不要先写 5%。** 先取证，再用行情锚定查询判断（§4.4）。
- 所有新增行都是推断，必须标 `OWNER_RISK_ACCEPTED`，需要 owner 批准（§6）。草案行已用现有加载器校验：加进规则书后能通过 `rule_book_from_json`，没有重叠、没有歧义（§4.9）。

## 1. 方法

### 1.1 怎么得出缺口

规则查找是 `RuleBook::price_rule(market, board, status, phase, day)`（`src/trading_rules.rs`）：通配符 `all` 匹配任何值，命中 0 行是 `Missing`，命中 ≥2 行是 `Ambiguous`，不取最后一条。输入维度来自 `price_limit.rs`：

- **板块**：`Board::of_entity` 按代码前缀分类。主板 `600/601/603/605/000/001/002/003`，创业板 `300/301/302`，科创板 `688/689`。其它前缀是 `board_unsupported`，不在本文范围。北交所、B 股按 D-79 O7 本阶段不收。
- **状态类别**：`status_class(board, status)`。`Normal`→`normal`；`OtherRiskWarning`/`DelistingRiskWarning`→`st`（科创板为 `star_st`）；`DelistingArrangement`→`delisting_arrangement`；`TerminatedOrDelisted`→`not_listed`；其余（含 `Unknown`）→ 状态未知。
- **上市阶段**：`listing_phase(calendar, listing, as_of)`。上市日为 `first_day`；上市日之后的第 2–5 个交易日（日历序号差 1 到 4）为 `first_5_days`；之后为 `seasoned`。规则书里 `first_5_days` 的 JSON 写法是 `first5_days`（serde 的 snake_case），表里是 `first_5_days`，是同一个值。

方法：对 4 个（市场，板块）× 4 种状态类别 × 3 个阶段，在每个规则起止日的两侧探测，共 1,200 个点。用 Rust 的 `price_rule` 跑一遍（临时测试，没有提交），再用独立写的 Python 匹配器跑一遍，两边结果逐点一致。临时测试已还原，工作区只有本文。

### 1.2 缺口在 `price_limit` 里怎么表现

读 `price_limit.rs` 得出（没有在生产上验证）：

| 状态 | 缺口日的返回 |
|---|---|
| 状态已知（ST 区间，或深市 T3 落地后的 `normal`） | `unknown` + `rule_missing` |
| 状态未知（沪市主板恒如此；深市在 T3 前也如此） | 先走 `invariant_band`：四类状态必须落在同一行才给确定值。缺口日只要有一类没有行，就返回 `None`，结果是 `unknown` + `st_unknown`；`candidates` 只列有行的状态类别，G1、G2 的第 2–5 日四类都没有行，所以为空，G4 的日子里 `normal` 有行，所以只有一个候选 |

所以「读面看到的原因码」取决于状态是否已知。下表的「现在返回」按这个规则写。

### 1.3 规则书里已经有的覆盖（对照用）

| 板块 | seasoned 的 normal | ST | 首日 | 第 2–5 日 |
|---|---|---|---|---|
| 沪主板 | 1996-12-16 起 10% | 1998-04-29 起 5%，2026-07-06 起 10% | 1996-12-16…2013-12-12 无限制；2013-12-13…2023-02-16 **无行**；2023-02-17 起无限制 | 2023-02-17 起无限制；之前**无行** |
| 深主板 | 1996-12-16 起 10% | 1998-04-28 起 5%，2026-07-06 起 10% | 1996-12-16…2014-06-12 无限制；2014-06-13…2023-02-16 **无行**；2023-02-17 起无限制 | 同沪市 |
| 创业板 | 2009-10-30…2020-08-23 10%；2020-08-24 起 20%（`all`） | 2020-08-24 起 20%（`all`）；之前**无行** | 2020-08-24 起无限制；之前**无行** | 2020-08-24 起无限制；之前**无行** |
| 科创板 | 2019-07-22 起 20%（`all`） | 同左 | 2019-07-22 起无限制 | 2019-07-22 起无限制 |

科创板没有缺口。`delisting_arrangement` 在主板所有时段、创业板 2020-08-24 前都没有行；创业板 2020-08-24 起、科创板由 `all` 行覆盖。

## 2. 缺口清单

「区间」是 `as_of` 的有效区间，右端不含。「现在返回」按 §1.2。编号 G 是缺口，N 是已有裁决的未编码段，R 是 R7，E 是已有行的边界疑问。

| 编号 | 市场 / 板块 | 状态类别 | 上市阶段 | 区间 | 现在返回 | 性质 |
|---|---|---|---|---|---|---|
| R7 | 沪、深 / 主板 | 全部 | 全部 | 1996-12-16 之前 | `rule_missing` / `st_unknown` | D-44 (3) `NOT_AVAILABLE`，禁止「无限制」兜底 |
| G1-SH | 沪 / 主板 | normal | 第 2–5 日 | 1996-12-16…2023-02-17 | 同上 | 缺行 |
| G1-SZ | 深 / 主板 | normal | 第 2–5 日 | 1996-12-16…2023-02-17 | 同上 | 缺行 |
| G2 | 深 / 创业板 | normal | 第 2–5 日 | 2009-10-30…2020-08-24 | 同上 | 缺行 |
| G3a | 深 / 创业板 | 全部 | 首日 | 2009-10-30…2014-06-13 | 同上 | 缺行 |
| G3b | 深 / 创业板 | 全部 | 首日 | 2014-06-13…2020-08-24 | 同上 | 与 N1 同因（推断，见 §4.5） |
| N1-SH | 沪 / 主板 | 全部 | 首日 | 2013-12-13…2023-02-17 | 同上 | D-64 `not_encoded`：144%/64% 申报范围 |
| N1-SZ | 深 / 主板 | 全部 | 首日 | 2014-06-13…2023-02-17 | 同上 | 同上 |
| G4 | 深 / 创业板 | st（含 *ST） | 全部（实际只会出现 seasoned） | 2009-10-30…2020-08-24 | 同上 | 缺行，值未知 |
| G5-SZ | 深 / 主板 | st | seasoned | 1996-12-16…1998-04-28 | 同上 | 状态不可达（推断，§4.6） |
| G5-SH | 沪 / 主板 | st | seasoned | 1996-12-16…1998-04-29 | 同上 | 同上 |
| G6 | 沪、深 / 主板；深 / 创业板（2020-08-24 前） | delisting_arrangement | 全部 | 主板 1996-12-16 起至今；创业板 2009-10-30…2020-08-24 | 同上 | 缺行，当前没有写入者产出该状态 |
| E1 | 沪、深 / 主板 | 全部 | 首日、第 2–5 日 | 2023-02-17 前后 | 返回确定值 | 已有行按 `as_of` 生效，不按上市日，可能用错（§4.8） |
| E2 | 深 / 创业板 | 全部 | 首日、第 2–5 日 | 2020-08-24 前后 | 返回确定值 | 同上 |

G5、G6 的「现在返回」只在对应状态出现时才发生；当前数据里这两种状态是否出现，要查（§3）。

## 3. 影响多大

### 3.1 上界（来自文档里已有的数，不是实测）

- 不复权日线 bar 总数 18,031,689 根（[离线度量](offline-measures-20261106.md) §0）。
- 主表 A 股 5,565 行（STATUS）。设每只股票都在窗口内上市且全部有 bar，则第 2–5 日最多 5,565 × 4 = **22,260** 根（占 0.12%），首日最多 5,565 根。这是全部 A 股的上界，不分板块，也没扣除 1996-12-16 之前上市、无 bar 或上市日不在窗口内的股票。G1、G2、G3a、N1 的真值都低于对应部分的上界。
- 创业板 ST 区间日（全部日期，到 2026-11-04）：ST 18,397 + *ST 19,092 = 37,489 根（离线度量 §3.2）。G4 只是其中 2020-08-24 之前的部分，真值更小，未单独统计。

### 3.2 要在 vm2 查的

附录 A 给出八条只读 SQL。执行方式沿用 [STATUS](STATUS.md) 的约定：`sqlite3 'file:/opt/x-data-mcp-preview/state/preview.sqlite?mode=ro'`，`PRAGMA query_only=ON`，用 `nice` 运行，包含 WAL。它们在本机的合成库上跑通过（表和列取自迁移文件），**没有在真实库上跑**，耗时未知。

| 查询 | 回答什么 | 对应缺口 |
|---|---|---|
| Q1 | 各段的 bar 日数和涉及股票数，分沪深 | G1、G2、G3a、G3b、N1 |
| Q2 | 创业板 ST/*ST 在 2020-08-24 前的 bar 日；创业板最早 ST 区间起点；主板 ST 区间起点早于 1998-04-28/29 的行数；`delisting_arrangement` 区间行数；ST 区间是否覆盖上市首日或第 2–5 日；整理期内的 bar 日 | G4、G5、G6，以及 G1/G2 能不能写成 `all` |
| Q3 | 主板第 2–5 日是否按 10% 成交 | G1 的行情锚定 |
| Q4 | 创业板第 2–5 日、ST 日是否按 10% / 5% 成交 | G2、G4 的行情锚定 |
| Q5 | 创业板首日 bar 的振幅，对 ±10% 和 +44%/−36% 各自是否成立 | G3a、G3b |
| Q6 | 上市日不在交易日历里的股票数 | 上市阶段本身是否可判（与规则书无关，顺带查） |
| Q7 | 2023-02-17 前后、2020-08-24 前后上市的新股的首日和第 2–5 日 bar 数 | E1、E2 |
| Q8 | 1996-12-16 之前的 A 股 bar 数 | R7 |

## 4. 每段能不能补

先说证据的范围：A-7 的规则文本已由 D-44、D-59 批准为 `APPROVED_SOURCE`（交易所规则库，本地非商业研究；对外再分发需书面许可，D-44 (4)）。规则原文、URL、抓取时间和 sha256 在私有 A-7 取证报告（#395），**不在仓库里**，本机上我也没有找到。所以本文只能引用 `basis_ref` 里写出的条款号，下面所有「出处」都是这样来的，没有重新读原文。

D-44 (2)(5)、D-64 给出过先例：没有原文、靠行情锚定和相邻条文推断的行，按 `OWNER_RISK_ACCEPTED` 入库，`basis_ref` 必须写明是推断（加载器 `check_basis` 强制：含「推断」或「行情锚定」字样的行必须带 `OWNER_RISK_ACCEPTED`）。下面的草案行都走这条路。

### 4.1 G1：主板上市后第 2–5 日（1996-12-16…2023-02-17）

- **缺的是什么。** 规则书只有首日的「无限制」行和 2023-02-17 起「前 5 日无限制」行；首日之后到 2023-02-16 之间的第 2–5 日没有行。
- **已批准来源能覆盖多少。** 现有行的 `basis_ref` 写到：沪 2006 3.4.13「首个交易日无价格涨跌幅限制」，深 2011/2013 3.3.17「IPO 首日不实行涨跌幅限制」，以及 R1 的 10%（`APPROVED_SOURCE`，起点 `OWNER_RISK_ACCEPTED`）。这些条款只豁免首日，首日之后适用通常幅度。**10% 的值和「豁免只到首日」都有现有 `basis_ref` 支持**；推断的部分是「第 2–5 日不在豁免内」这个范围结论，条文本身没有逐字写出。所以整行标 `OWNER_RISK_ACCEPTED`（推断）。
- **旁证（弱）。** 离线度量对 2010-09-14 之后的对照组：按 seasoned 行（主板 10%、创业板 10%/20%）、ST 一律按非 ST 算，非除权日 bar 13,843,693 根里只有 1,321 根越界（0.0095%）。该对照组含上市后第 2–5 日，所以 2010-09-14 之后第 2–5 日的越界数不超过 1,321 根。它同时含 2023-02-17 起合法无限制的新股日，不是针对本段的锚定，也没有覆盖 2010-09-14 之前，只说明「第 2–5 日大面积越界」在后一段不成立。
- **要补的证据。** 行情锚定查询 Q3：第 2–5 日、非除权日的 bar 对前收的越界数（D-64 的做法，越界线取 10.2%）。结果要写进 `basis_ref`。结果出来前不得入库。
- **一个选择（需要 owner 定）。** 草案行有两种写法：
  - **`status_class='normal'`（推荐）。** 不断言任何关于 ST 的事。深市在 T3 落地后（状态可为 `normal`）这些日子才变成确定值；**沪市主板状态恒为 `unknown`，这行对沪市现在没有任何效果**（`invariant_band` 要四类状态落在同一行）。
  - `status_class='all'`。沪市也会变成确定值，但等于断言「上市第 2–5 日不可能是 ST」。仓库里没有这条断言的证据，沪市 ST 状态在 2000 年前是 `NOT_AVAILABLE`（D-58），事件型来源也无法证明「没有」。可以先看 Q2 的 B4（ST 区间是否覆盖过任何股票的首日或第 2–5 日，预期 0），但 B4 为 0 对沪市只是「没有观察到」，不是证明。

草案行（`normal` 写法；`first5_days` 是 JSON 的写法）：

```json
{
  "rule_id": "r1-sh-main-normal-first-5-days-10pct-1996-12-16-2023-02-16",
  "market": "sh", "board": "main", "status_class": "normal", "listing_phase": "first5_days",
  "band_bp": 1000, "band_side": "both", "rounding": "half_up",
  "effective_from": "1996-12-16", "effective_to": "2023-02-17",
  "basis_ref": "R1 补缺（上市后第 2–5 个交易日）: tier=OWNER_RISK_ACCEPTED(推断); 值与起点沿用 r1-sh-main-normal-10pct; 推断依据=沪 2006 3.4.13 只豁免「首个交易日」（见 r6-sh-main-all-first-day-none-*），首日之后不在豁免内，适用 R1 的 10%; 2023-02-17 起由 r6-sh-main-all-first-5-days-none-from-2023-02-17 接替; 仅 normal，ST 不在本行; rounding=half_up (R9: 沪 3.3.17); evidence=行情锚定查询 Q3（结果待 vm2）",
  "basis_status": "verified"
}
```

深市主板的一行相同，只把 `market` 换成 `sz`，`rule_id` 为 `r1-sz-main-normal-first-5-days-10pct-1996-12-16-2023-02-16`，引用深 2011/2013 3.3.17、`r1-sz-main-normal-10pct`、`r6-sz-main-all-first-day-none-*`，R9 引深 3.3.19。

### 4.2 G2：创业板第 2–5 日（2009-10-30…2020-08-24）

- 规则书的创业板 `normal` seasoned 行（R3）本身就是 `OWNER_RISK_ACCEPTED`，起点和 2009-10-30…2011-01-16 缺口按推断（G-5，D-44 (2)）。第 2–5 日沿用它的值和起点，再加「首日豁免不延伸到第 2–5 日」的推断，所以整行是 `OWNER_RISK_ACCEPTED`（推断）。
- 原文：创业板 2020-08-24 前的首日豁免范围，仓库里没有逐字依据（R4 只写了 2020-08-24 起的规则）。要确认就得读创业板 2009–2020 的交易规则原文，这在 S-9 已批准的来源范围内（深交所规则库），但需要有人去取，本文没有取。
- 行情锚定：Q4 的 `days2_5` 一行。
- 草案行：

```json
{
  "rule_id": "r3-sz-gem-normal-first-5-days-10pct-2009-10-30-2020-08-23",
  "market": "sz", "board": "gem", "status_class": "normal", "listing_phase": "first5_days",
  "band_bp": 1000, "band_side": "both", "rounding": "half_up",
  "effective_from": "2009-10-30", "effective_to": "2020-08-24",
  "basis_ref": "R3 补缺（上市后第 2–5 个交易日）: tier=OWNER_RISK_ACCEPTED(推断); 值、起点沿用 r3-sz-gem-normal-10pct-until-2020-08-21（含 2009-10-30…2011-01-16 缺口 G-5，推断）; 推断依据=创业板 2020-08-24 前只豁免上市首日，首日之后适用 R3 的 10%（豁免范围的原文待确认）; 2020-08-24 起由 r4-sz-gem-all-first-5-days-none-from-2020-08-24 接替; 仅 normal; rounding=half_up (R9: 深 3.3.19); evidence=行情锚定查询 Q4（结果待 vm2）",
  "basis_status": "verified"
}
```

同 G1，`normal` 写法在深市 T3 落地前不改变现有返回；写成 `all` 的利弊同 §4.1。

### 4.3 G3a：创业板首日（2009-10-30…2014-06-13）

- 规则书对主板首日有「无限制」行，创业板没有。2013-12-13…2014-06-12 这段，主板深市行引的是深证会〔2013〕142号，该文以深圳市场为对象，是否逐字含创业板，仓库里没有写，未核实。
- **已批准来源不能直接覆盖**，需要先确认创业板的首日规则原文，否则只能走行情锚定。Q5 在首日 bar 上看振幅：若有首日 bar 的 high/low 超过 11/9，说明 ±10% 不成立；若有超过 144/64，说明 44%/36% 也不成立。这只能证伪，不能证明「无限制」，和 D-64 对 R6 前段的做法同级（「首日 bar 抽样行情锚定」）。
- 草案行（`all` + `none`，与主板首日行一致）：

```json
{
  "rule_id": "r6-sz-gem-all-first-day-none-2009-10-30-2014-06-12",
  "market": "sz", "board": "gem", "status_class": "all", "listing_phase": "first_day",
  "band_bp": null, "band_side": "none", "rounding": "half_up",
  "effective_from": "2009-10-30", "effective_to": "2014-06-13",
  "basis_ref": "R6 补缺（创业板上市首日，2014-06-13 前）: tier=OWNER_RISK_ACCEPTED(推断); 起点沿用 R3 的 2009-10-30; 推断依据=深 2011 3.3.17 / 2013 3.3.17「IPO 首日不实行涨跌幅限制」是否适用创业板，需对照创业板规则原文确认（本行在确认前不得入库）; 行情锚定=Q5 的首日 high/low 比; band_side=none 是已知的「无限制」，开盘集合竞价与盘中停牌是另一层约束; 2014-06-13 起创业板首日是否为 44%/36% 见 not_encoded; evidence=Q5（结果待 vm2）",
  "basis_status": "verified"
}
```

- 建议：**有条件补。** 原文确认或 Q5 无反例，且 owner 接受行情锚定的档位，才入库；否则维持 `unknown`。

### 4.4 G4：创业板 ST/*ST，2020-08-24 之前

- **仓库里没有答案。** 规则书对创业板 2020-08-24 前只有 `normal` 行。R2 的深市 5% 行的出处是深 2011/2013 3.3.15「ST/*ST 股票涨跌幅 5%」，仓库里没有写它是否适用创业板。
- 所以**不能先写 5%**。要先取创业板 2009–2020 规则原文里风险警示股票的涨跌幅条款，来源是深交所规则库（S-9 已批准来源）。同时用 Q4 的 `st_days` 一行做行情锚定：ST 日 bar 对前收越出 5.2% 的数多不多。大量越界说明不是 5%（可能是 10%），零越界才支持 5%。
- Q2 的 B1 和 B1b 给出创业板最早的 ST 起点和 ST 日 bar 数。草案行的起点取创业板开板日 2009-10-30；若 B1b 显示最早 ST 在更晚的日期，可以改起点，但不影响结果。
- 草案行，**值是候选 A（5%）；若锚定和原文指向 10%，只改 `band_bp` 和 `rule_id`**：

```json
{
  "rule_id": "r2-sz-gem-st-5pct-2009-10-30-2020-08-23",
  "market": "sz", "board": "gem", "status_class": "st", "listing_phase": "seasoned",
  "band_bp": 500, "band_side": "both", "rounding": "half_up",
  "effective_from": "2009-10-30", "effective_to": "2020-08-24",
  "basis_ref": "R2 补缺（创业板风险警示，2020-08-24 前）: tier=OWNER_RISK_ACCEPTED(推断); 起点取创业板开板日（此前没有创业板，也就没有创业板 ST 状态）; 5% 的推断依据=深 2011 3.3.15 / 2013 3.3.15「ST/*ST 股票涨跌幅 5%」是否适用创业板，需对照创业板 2009–2020 规则原文，并以行情锚定 Q4（ST 日越出 5.2% 的 bar 数）支持; *ST 与 ST 同带宽; 不置 min_band_ticks（创业板是否适用 2011 3.3.16 的一 tick 下限未取证，极低价仍是已知未编码的百分比结果，同 r2-sz-main-st-5pct-until-2026-07-05 的先例）; 2020-08-24 起由 r4-sz-gem-all-20pct-from-2020-08-24 接替; rounding=half_up (R9: 深 3.3.19); evidence=Q2 B1/Q4（结果待 vm2）",
  "basis_status": "verified"
}
```

- 状态未知的创业板日子不受这行影响：`normal` 10% 和 `st` 5%（或 10%）是两行，`invariant_band` 仍返回 `None`。若值取 10%，两行幅度相同，规则书的约定也一样：不借用一行的出处回答未知状态（`issue-391` §8.5）。
- 建议：**先取证，不补。** 这是 `unknown` 比一个猜的 5% 更诚实的一段。

### 4.5 N1 与 G3b：首日 144%/64%

- N1 是 D-64 已定的不编码段：沪 2013-12-13 起、深 2014-06-13 起到 2023-02-16，首日涨跌幅是相对发行价的 +44%/−36%，不对称，基准是发行价而不是前收。规则书的 `not_encoded` 条目写明：要两带 schema（`band_down_bp`、`price_basis`、`band_kind`）加发行价来源（G-7，未准入）。单个 `band_bp` 加 `band_side` 表达不了，用百分比近似会给出错误价。
- G3b 是创业板在 2014-06-13…2020-08-24 的首日。**推断**它与深市主板同因（深证会〔2014〕54号以深圳市场为对象），仓库里没有创业板的逐字依据。
- **不能补，也不能用现有 schema 近似。** 要取的证据：
  1. 深证会〔2014〕54号、上证发〔2013〕20号对创业板的适用范围（深交所、上交所业务规则栏目，S-9 已批准来源；私有报告里已有这两份，本文没有读）。
  2. 发行价：来源未准入。候选是交易所新股发行或上市公告、招股说明书与上市公告书；**可得性和字段形态我没有核实**，需要作为新的来源准入任务评估。
- 建议：维持 `unknown`，等 D-64 另立的「两带 schema + 发行价来源」任务。

### 4.6 G5：主板 ST，1998-04-28/29 之前

- D-64 的 R2 前段起点是深交所简称表的首个 ST 事件 000511 1998-04-28（深）和沪市 `.day` 锚定的 1998-04-29。此前没有 ST 事件。所以这一段没有可达的 ST 状态：沪市 2000 年前的 ST 状态是 `NOT_AVAILABLE`（D-58），深市简称表此前没有 ST 行。
- 确认方法：Q2 的 B2（主板 ST/*ST 区间起点早于 1998-04-28/29 的行数），预期 0。
- 建议：**不补**。补一条 1996-12-16…1998-04-28 的 5% 行等于凭空造出 ST 状态的幅度，而该状态不存在。

### 4.7 G6：退市整理期

- 规则书对 `delisting_arrangement` 在主板所有时段、创业板 2020-08-24 前没有行。`risk_warnings.rs` 的写入器只产出 `OtherRiskWarning`/`DelistingRiskWarning`（`validate_set` 放行 `DelistingArrangement`，但没有生产者），`status_of` 只读 `risk_warning_intervals`，而整理期区间（`arrangement_start/end`）只在 `security_master` 里，没有接到状态上。所以**当前读面不会产出这个状态**，这段缺口现在不可达。
- 但这带来一个**潜在误判**（推断，未验证）：处在整理期的股票，如果其 ST/*ST 区间仍覆盖该期，`price_limit` 会按 ST 的 5% 回答，而整理期的涨跌幅条款在仓库里没有证据。Q2 的 B5 给出整理期内的 bar 日数，B3 确认 `risk_warning_intervals` 里没有 `delisting_arrangement` 行。
- 要取的证据：沪深主板、创业板的退市整理期涨跌幅条款，在不同年代的版本。来源是交易所规则库（S-9 已批准来源），**本文没有取**。
- 建议：维持 `unknown`；B5 不为 0 时，作为已知风险记录，另议是否对整理期内的日子强制 `unknown`。

### 4.8 R7 与 E1、E2

- **R7**：1996-12-16 之前，D-44 (3) 的 `NOT_AVAILABLE`。D-44 和 D-59 记的依据是：已废止规则库 443 条可枚举、规则库最早版本 2006、公告索引无此类通知。加载器也拒绝任何早于 1996-12-16 的行。**维持**。Q8 给出受影响的 bar 数。
- **E1、E2 不是缺口，是已有行的正确性疑问。** 规则书按 `as_of` 的日期选行，不按上市日。上市日在新规则施行日之前几天的新股，其第 2–5 日会落在施行日之后，被 `r6-*-first-5-days-none-from-2023-02-17`（主板）或 `r4-sz-gem-all-first-5-days-none-from-2020-08-24`（创业板）命中，返回 `unbounded`。
  - 这些股票按老规则上市，施行日后是否适用「前 5 日无限制」，仓库里没有写。创业板 2020-08-24 起 seasoned 为 20%（R4 的 `all` 行），但施行日前上市、第 2–5 日跨过施行日的新股是否也适用「无限制」，**未核实**。
  - 主板 2023-02-17 前后是否有过渡安排，仓库里同样没有依据。要核实就读 2023 年全面注册制主板交易规则的衔接条款。
  - 这类错误是「把未知说成确定的无限制」，比 `unknown` 更糟，所以列在这里。范围很小，Q7 给出 bar 数。
  - 建议：先取证（衔接条款原文），再决定是给这两行加「按上市日生效」的条件（需要改 schema 或规则表达，不是只加数据行），还是接受误差并在 `limitations` 里写明。**本文不提改法。**

### 4.9 草案行的校验

把上面五行草案（G1 两行、G2、G3a、G4 候选 A）加进规则书，用现有 `rule_book_from_json` 加载（临时测试，已还原）：

- 校验通过：区间半开且正、`rule_id` 唯一、同一精确 scope 的 verified 区间不重叠、`basis_ref` 带档位、推断行带 `OWNER_RISK_ACCEPTED`、没有早于 1996-12-16 的行。
- 规则书由 27 行变 32 行。
- 在 1996-12-15、1996-12-16、2009-10-29、2009-10-30、2014-06-12/13、2020-08-23/24、2023-02-16/17 等边界日两侧探测：起点前一日仍是 `Missing`，终点日由原有的 `r4-*`/`r6-*` 接替，没有歧义。在 1996-12-15…2023-12-31 逐日对三个板块的 `normal`、`st` 与三个阶段扫描，`Ambiguous` 命中 0。

这只证明草案行在结构上能加载、不与现有行冲突，不证明它们的值正确。值正确与否取决于 §4 的取证和 Q3–Q5 的结果。

## 5. 总表

「影响」为 bar 日数，上界来自文档里已有的数，「待 vm2」表示要用附录 A 的查询取得。「证据状态」中，`已批准` 指 D-44/D-64 已批准的来源，`推断` 指需要 `OWNER_RISK_ACCEPTED`。

| 段 | 影响 | 证据状态 | 建议 |
|---|---|---|---|
| R7：1996-12-16 前，沪深主板 | 待 vm2（Q8） | `NOT_AVAILABLE`（D-44 (3)） | 维持 `unknown` |
| G1-SH：沪主板第 2–5 日，到 2023-02-16 | 全部 A 股第 2–5 日上界 22,260（0.12%）；真值待 vm2（Q1、Q3） | 值与首日豁免范围有已批准条款支撑；范围结论是推断 | 补 `normal` 行，`OWNER_RISK_ACCEPTED`（推断）。对沪市现在无效果（沪市状态恒 `unknown`），除非 owner 接受 `all` 写法 |
| G1-SZ：深主板第 2–5 日，到 2023-02-16 | 同上 | 同上 | 补 `normal` 行；深市 T3 落地后生效 |
| G2：创业板第 2–5 日，2009-10-30…2020-08-24 | 同上 | R3 本身是推断；首日豁免范围原文待确认 | 补 `normal` 行（推断），Q4 无反例后入库 |
| G3a：创业板首日，2009-10-30…2014-06-13 | 首日上界 5,565；真值待 vm2（Q1、Q5） | 原文待确认；行情锚定只能证伪 | 有条件补：原文确认或 Q5 无反例且 owner 接受锚定档位，否则维持 `unknown` |
| G3b：创业板首日，2014-06-13…2020-08-24 | 待 vm2（Q1） | 推断与 N1 同因，无逐字依据 | 维持 `unknown`，随 N1 |
| N1-SH：沪主板首日，2013-12-13…2023-02-17 | 待 vm2（Q1） | D-64 已定 `not_encoded`；缺发行价来源（G-7） | 维持 `unknown`，等两带 schema 与发行价来源任务 |
| N1-SZ：深主板首日，2014-06-13…2023-02-17 | 同上 | 同上 | 同上 |
| G4：创业板 ST/*ST，2009-10-30…2020-08-24 | 创业板 ST/*ST 全期上界 37,489；2020-08-24 前的部分待 vm2（Q2 B1） | **仓库里没有**；值（5% 还是 10%）未知 | 先取证，不补。不要预设 5% |
| G5：主板 ST，1996-12-16…1998-04-28/29 | 预期 0（Q2 B2） | 状态不可达（推断，D-64、D-58） | 不补 |
| G6：`delisting_arrangement`，主板全期、创业板 2020-08-24 前 | 当前读面不产出该状态，预期 0（Q2 B3）；整理期内 bar 日待 vm2（Q2 B5） | 仓库里没有条款 | 维持 `unknown`；B5 不为 0 时记为潜在误判 |
| E1：2023-02-17 前后上市的主板新股 | 待 vm2（Q7） | 衔接条款未核实；印象不作依据 | 先取证，再议；本文不提改法 |
| E2：2020-08-24 前后上市的创业板新股 | 待 vm2（Q7） | 同上 | 同上 |

## 6. 需要 owner 批准什么

1. **接受推断行的档位。** G1、G2（以及条件满足时的 G3a）的草案行是 `OWNER_RISK_ACCEPTED`（推断）。D-44 (2)(5)、D-64 已有同类先例，但每个新行都要 owner 点头。批准的前提是行情锚定查询无反例。**「无反例」的容忍度要 owner 定**：D-64 的先例是 0 违例，本文建议沿用。
2. **G1、G2 写成 `normal` 还是 `all`。** 推荐 `normal`（§4.1）。写成 `all` 需要 owner 接受「上市第 2–5 日不可能是 ST」这条断言，并看 Q2 的 B4。
3. **G3a 的取舍。** 原文没有确认时，是否接受只靠 Q5 的行情锚定入库。推荐不接受，先确认原文。
4. **取证排期。** G4、G6、E1、E2、G3a 的原文确认需要联网读交易所规则库。这些来源 D-44 (4) 已按 `APPROVED_SOURCE` 批准（本地非商业研究），不需要新的来源准入，但取证动作需要 owner 同意并排期。**在此之前不取。**
5. **确认「维持 `unknown`」的段。** R7、N1、G3b、G5、G6 维持 `unknown`；N1、G3b 另立两带 schema 与发行价来源任务（D-64 已定，尚未开）。
6. **G4 的值不要预设。** 若取证显示创业板 ST 在 2020-08-24 前是 10%，则行的幅度与 `normal` 相同，仍需要这一行：它让 ST 状态已知的日子有确定值和出处。

## 7. 批准后的改动范围（预估，未做）

规则是数据，改动不碰代码，但有几处硬编码的计数和快照名要同步：

- `src/trading_rules/a7-d44-v1.json`：加行；`snapshot_id` 要随内容更新，`src/trading_rules/tests.rs` 里有测试断言它含「D-64」「2026-10-29」「min_band_ticks」，要一起改。
- `src/trading_rules/tests.rs`：两处 `assert_eq!(book.price_limits.len(), 27)`（`the_frozen_d44_book_loads_and_covers_every_derivable_board`、`the_rule_source_is_admitted_not_pending`）要改计数；每个新行加边界日两侧的测试，沿用 `r2_pre_2023_segments_change_on_their_own_boundary_day` 的写法。
- `src/research/price_limit/tests.rs`：`the_declared_range_first_day_is_still_rule_missing` 等针对 N1 的测试不受影响；若有测试依赖第 2–5 日为 `rule_missing`，需要检查。
- 文档：`docs/data-catalog.md` 里「27 + 2 行」的几处（第 325、438、596 行附近）、STATUS 里的行数。
- 日更 `derived:trading_rules` 会因内容变化发布新版本，`rules_version` 随之变化；`limit-v1` 的算法版本不变（AC-M2-10：规则变更只改数据）。
- 派生结果按 `as_of` 回看的使用者会看到同一输入在新规则版本下的结果不同，这是预期行为，不是回归。

## 8. 未验证

- 所有影响数中，除 §3.1 的上界外都没有实测。附录 A 的查询只在合成库上跑过，**没有在真实库上跑**，耗时未知；同时没有核对真实库里 `market_series`、`security_master` 的行形态（例如同一代码有多行主表区间时 `MIN(listing_date)` 的取法）。
- 私有 A-7 取证报告不在仓库，我没有读到任何规则原文。§4 里关于「条款只豁免首日」「是否适用创业板」的判断，只依据规则书 `basis_ref` 的文字。
- 创业板 2009–2020 年的 ST 涨跌幅、首日规则、退市整理期涨跌幅、2023-02-17 与 2020-08-24 的衔接条款，仓库里都没有逐字依据，本文没有填测。我对其中一些的印象不作为依据，已在对应小节标明。
- §1.2 的「现在返回」是读 `price_limit.rs` 得出，没有在生产上观察。
- §4.9 的校验只证明草案行在结构上能加载，不证明值正确。
- 没有评估 E1、E2 的改法。

## 附录 A：只读 SQL（待 vm2）

日期常量（epoch 日，与 `series_bars.trade_day` 同基准，已用 Python 核算）：9846 = 1996-12-16；10344 = 1998-04-28；10345 = 1998-04-29；14547 = 2009-10-30；16052 = 2013-12-13；16234 = 2014-06-13；18099 = 2019-07-22；18498 = 2020-08-24；19405 = 2023-02-17。

所有查询只读 `trading_days`、`market_series`（`period=1, adjustment=0, instrument_class=0`，不复权日线）、`series_bars`、`security_master`、`risk_warning_intervals`、`adjustment_events`。上市日取 `MIN(listing_date)` 按代码聚合，第 2–5 日按 `trading_days` 的序号数。除权日（`adjustment_events` 有当日事件）从行情锚定里排除，原因同[离线度量](offline-measures-20261106.md) §1：不复权前收不是除权日的基准价。

### Q1 各缺口段的 bar 日数

```sql
WITH
td AS (SELECT trade_day, ROW_NUMBER() OVER (ORDER BY trade_day) - 1 AS idx FROM trading_days),
lst AS (
  SELECT exchange, code, MIN(listing_date) AS d0
  FROM security_master
  WHERE instrument_class = 0 AND listing_date IS NOT NULL
  GROUP BY exchange, code),
w AS (  -- d4 = 上市后第 5 个交易日（上市日是第 1 个）
  SELECT l.exchange AS mkt, l.exchange || '.' || l.code AS entity, substr(l.code, 1, 3) AS pfx,
         l.d0, (SELECT trade_day FROM td WHERE idx = a.idx + 4) AS d4
  FROM lst l JOIN td a ON a.trade_day = l.d0),
b AS (
  SELECT w.mkt, w.entity, w.d0, bar.trade_day,
    CASE WHEN w.pfx IN ('600','601','603','605','000','001','002','003') THEN 'main'
         WHEN w.pfx IN ('300','301','302') THEN 'gem'
         WHEN w.pfx IN ('688','689') THEN 'star' ELSE 'other' END AS board,
    CASE WHEN bar.trade_day = w.d0 THEN 'first_day' ELSE 'first_5_days' END AS phase
  FROM w
  JOIN market_series s ON s.entity = w.entity AND s.period = 1 AND s.adjustment = 0 AND s.instrument_class = 0
  JOIN series_bars bar ON bar.series_key = s.series_key
   AND bar.trade_day >= w.d0 AND bar.trade_day <= COALESCE(w.d4, w.d0))
SELECT segment, mkt, COUNT(*) AS bar_days, COUNT(DISTINCT entity) AS entities FROM (
  SELECT entity, mkt, CASE
    WHEN board='main' AND phase='first_5_days' AND trade_day >= 9846  AND trade_day < 19405 THEN 'G1 主板第2-5日 1996-12-16..2023-02-16'
    WHEN board='gem'  AND phase='first_5_days' AND trade_day >= 14547 AND trade_day < 18498 THEN 'G2 创业板第2-5日 2009-10-30..2020-08-23'
    WHEN board='gem'  AND phase='first_day'    AND trade_day >= 14547 AND trade_day < 16234 THEN 'G3a 创业板首日 2009-10-30..2014-06-12'
    WHEN board='gem'  AND phase='first_day'    AND trade_day >= 16234 AND trade_day < 18498 THEN 'G3b 创业板首日 2014-06-13..2020-08-23'
    WHEN board='main' AND phase='first_day' AND mkt='sh' AND trade_day >= 16052 AND trade_day < 19405 THEN 'N1 沪主板首日 2013-12-13..2023-02-16'
    WHEN board='main' AND phase='first_day' AND mkt='sz' AND trade_day >= 16234 AND trade_day < 19405 THEN 'N1 深主板首日 2014-06-13..2023-02-16'
    WHEN board='gem'  AND trade_day < 14547 THEN 'X1 创业板开板日之前就有 bar'
    WHEN board='star' AND trade_day < 18099 THEN 'X2 科创板开板日之前就有 bar'
    WHEN board='other' THEN 'X3 板块前缀不识别'
  END AS segment
  FROM b)
WHERE segment IS NOT NULL
GROUP BY segment, mkt ORDER BY segment, mkt;
```

X1、X2、X3 是自检行，预期为空；不为空说明板块或上市日有问题，缺口分析要重做。

### Q2 状态相关的缺口

```sql
-- B1 创业板 ST/*ST 在 2020-08-24 之前的 bar 日（G4）
SELECT 'B1 创业板ST/*ST 2009-10-30..2020-08-23' AS item, r.normalized_status AS detail,
       COUNT(*) AS n, COUNT(DISTINCT s.entity) AS entities
FROM market_series s
JOIN series_bars b ON b.series_key = s.series_key
JOIN risk_warning_intervals r ON r.exchange = substr(s.entity,1,2) AND r.code = substr(s.entity,4)
 AND b.trade_day >= r.effective_from AND (r.effective_to IS NULL OR b.trade_day < r.effective_to)
WHERE s.period = 1 AND s.adjustment = 0 AND s.instrument_class = 0
  AND substr(s.entity,4,3) IN ('300','301','302')
  AND b.trade_day >= 14547 AND b.trade_day < 18498
  AND r.normalized_status IN ('other_risk_warning','delisting_risk_warning')
GROUP BY r.normalized_status
UNION ALL
-- B1b 创业板最早的 ST/*ST 区间起点
SELECT 'B1b 创业板最早ST区间起点', date(MIN(effective_from) * 86400, 'unixepoch'), COUNT(*), COUNT(DISTINCT code)
FROM risk_warning_intervals
WHERE substr(code,1,3) IN ('300','301','302')
  AND normalized_status IN ('other_risk_warning','delisting_risk_warning')
UNION ALL
-- B2 主板 ST/*ST 区间起点早于 1998-04-28(深)/1998-04-29(沪)（G5，预期 0）
SELECT 'B2 主板ST区间起点早于1998-04-28/29', exchange, COUNT(*), COUNT(DISTINCT code)
FROM risk_warning_intervals
WHERE normalized_status IN ('other_risk_warning','delisting_risk_warning')
  AND substr(code,1,3) IN ('600','601','603','605','000','001','002','003')
  AND effective_from < CASE exchange WHEN 'sz' THEN 10344 ELSE 10345 END
GROUP BY exchange
UNION ALL
-- B3 status = delisting_arrangement 的区间行数（G6，预期 0）
SELECT 'B3 risk_warning_intervals 里的 delisting_arrangement', '', COUNT(*), COUNT(DISTINCT exchange || code)
FROM risk_warning_intervals WHERE normalized_status = 'delisting_arrangement'
UNION ALL
-- B4 ST/*ST 区间覆盖上市首日或第 2-5 日的 bar 日（G1/G2 能否写成 all；预期 0）
SELECT 'B4 ST区间覆盖上市首日/第2-5日', '', COUNT(*), COUNT(DISTINCT c.entity) FROM (
  SELECT s.entity
  FROM security_master m
  JOIN market_series s ON s.entity = m.exchange || '.' || m.code
   AND s.period = 1 AND s.adjustment = 0 AND s.instrument_class = 0
  JOIN series_bars b ON b.series_key = s.series_key AND b.trade_day >= m.listing_date
   AND b.trade_day <= COALESCE((SELECT t.trade_day FROM trading_days t WHERE t.trade_day > m.listing_date
                                ORDER BY t.trade_day LIMIT 1 OFFSET 3), m.listing_date)
  JOIN risk_warning_intervals r ON r.exchange = m.exchange AND r.code = m.code
   AND b.trade_day >= r.effective_from AND (r.effective_to IS NULL OR b.trade_day < r.effective_to)
  WHERE m.instrument_class = 0 AND m.listing_date IS NOT NULL
    AND r.normalized_status IN ('other_risk_warning','delisting_risk_warning')) c
UNION ALL
-- B5 退市整理期内的 bar 日（G6：security_master 的 arrangement_start/end）
SELECT 'B5 整理期内的 bar 日', '', COUNT(*), COUNT(DISTINCT s.entity)
FROM security_master m
JOIN market_series s ON s.entity = m.exchange || '.' || m.code
 AND s.period = 1 AND s.adjustment = 0 AND s.instrument_class = 0
JOIN series_bars b ON b.series_key = s.series_key
 AND b.trade_day >= m.arrangement_start AND b.trade_day <= COALESCE(m.arrangement_end, m.arrangement_start)
WHERE m.instrument_class = 0 AND m.arrangement_start IS NOT NULL;
```

B4 为 0 不证明沪市没有「上市即 ST」，因为沪市 ST 来源是事件级（D-58）。

### Q3 主板第 2–5 日的行情锚定（G1）

```sql
WITH
td AS (SELECT trade_day, ROW_NUMBER() OVER (ORDER BY trade_day) - 1 AS idx FROM trading_days),
lst AS (SELECT exchange, code, MIN(listing_date) AS d0 FROM security_master
        WHERE instrument_class = 0 AND listing_date IS NOT NULL GROUP BY exchange, code),
w AS (SELECT l.exchange AS mkt, l.exchange || '.' || l.code AS entity, l.d0,
             (SELECT trade_day FROM td WHERE idx = a.idx + 4) AS d4
      FROM lst l JOIN td a ON a.trade_day = l.d0
      WHERE substr(l.code,1,3) IN ('600','601','603','605','000','001','002','003')),
t AS (SELECT w.mkt, w.entity, s.series_key, bar.trade_day, bar.high_units, bar.low_units
      FROM w
      JOIN market_series s ON s.entity = w.entity AND s.period = 1 AND s.adjustment = 0 AND s.instrument_class = 0
      JOIN series_bars bar ON bar.series_key = s.series_key AND bar.trade_day > w.d0 AND bar.trade_day <= w.d4
      WHERE bar.trade_day >= 9846 AND bar.trade_day < 19405
        AND NOT EXISTS (SELECT 1 FROM adjustment_events e WHERE e.entity = w.entity AND e.effective_day = bar.trade_day)),
p AS (SELECT t.*, (SELECT x.close_units FROM series_bars x
                   WHERE x.series_key = t.series_key AND x.trade_day < t.trade_day
                   ORDER BY x.trade_day DESC LIMIT 1) AS prev_close
      FROM t)
SELECT mkt, COUNT(*) AS bars,
       SUM(high_units * 10000 > prev_close * 11020 OR low_units * 10000 < prev_close * 8980) AS beyond_10_2pct,
       SUM(high_units * 10000 > prev_close * 10520 OR low_units * 10000 < prev_close * 9480) AS beyond_5_2pct
FROM p WHERE prev_close IS NOT NULL GROUP BY mkt;
```

`beyond_10_2pct` 为 0 支持 G1 草案行；`beyond_5_2pct` 只是对照，第 2–5 日本来就不该是 5%。

### Q4 创业板第 2–5 日与 ST 日的行情锚定（G2、G4）

```sql
WITH
td AS (SELECT trade_day, ROW_NUMBER() OVER (ORDER BY trade_day) - 1 AS idx FROM trading_days),
lst AS (SELECT exchange, code, MIN(listing_date) AS d0 FROM security_master
        WHERE instrument_class = 0 AND listing_date IS NOT NULL GROUP BY exchange, code),
w AS (SELECT l.exchange || '.' || l.code AS entity, l.d0,
             (SELECT trade_day FROM td WHERE idx = a.idx + 4) AS d4
      FROM lst l JOIN td a ON a.trade_day = l.d0
      WHERE substr(l.code,1,3) IN ('300','301','302')),
t AS (
  SELECT 'days2_5' AS grp, s.series_key, bar.trade_day, bar.high_units, bar.low_units
  FROM w JOIN market_series s ON s.entity = w.entity AND s.period = 1 AND s.adjustment = 0 AND s.instrument_class = 0
  JOIN series_bars bar ON bar.series_key = s.series_key AND bar.trade_day > w.d0 AND bar.trade_day <= w.d4
  WHERE bar.trade_day >= 14547 AND bar.trade_day < 18498
    AND NOT EXISTS (SELECT 1 FROM adjustment_events e WHERE e.entity = w.entity AND e.effective_day = bar.trade_day)
  UNION ALL
  SELECT 'st_days', s.series_key, bar.trade_day, bar.high_units, bar.low_units
  FROM market_series s
  JOIN series_bars bar ON bar.series_key = s.series_key
  JOIN risk_warning_intervals r ON r.exchange = substr(s.entity,1,2) AND r.code = substr(s.entity,4)
   AND bar.trade_day >= r.effective_from AND (r.effective_to IS NULL OR bar.trade_day < r.effective_to)
  WHERE s.period = 1 AND s.adjustment = 0 AND s.instrument_class = 0
    AND substr(s.entity,4,3) IN ('300','301','302')
    AND bar.trade_day >= 14547 AND bar.trade_day < 18498
    AND r.normalized_status IN ('other_risk_warning','delisting_risk_warning')
    AND NOT EXISTS (SELECT 1 FROM adjustment_events e WHERE e.entity = s.entity AND e.effective_day = bar.trade_day)),
p AS (SELECT t.*, (SELECT x.close_units FROM series_bars x WHERE x.series_key = t.series_key
                   AND x.trade_day < t.trade_day ORDER BY x.trade_day DESC LIMIT 1) AS prev_close FROM t)
SELECT grp, COUNT(*) AS bars,
       SUM(high_units * 10000 > prev_close * 11020 OR low_units * 10000 < prev_close * 8980) AS beyond_10_2pct,
       SUM(high_units * 10000 > prev_close * 10520 OR low_units * 10000 < prev_close * 9480) AS beyond_5_2pct
FROM p WHERE prev_close IS NOT NULL GROUP BY grp;
```

`st_days` 行：`beyond_5_2pct` 大说明 ST 日不是 5%；`beyond_10_2pct` 也大说明 ST 日也不是 10%。`days2_5` 行：`beyond_10_2pct` 为 0 支持 G2 草案行。

### Q5 创业板首日 bar 的振幅（G3a、G3b）

```sql
WITH
lst AS (SELECT exchange, code, MIN(listing_date) AS d0 FROM security_master
        WHERE instrument_class = 0 AND listing_date IS NOT NULL GROUP BY exchange, code)
SELECT CASE WHEN l.d0 < 16234 THEN 'gem 2009-10-30..2014-06-12' ELSE 'gem 2014-06-13..2020-08-23' END AS seg,
       COUNT(*) AS first_day_bars,
       SUM(bar.high_units * 9 > bar.low_units * 11)   AS range_over_10pct_band,   -- high/low > 11/9：±10% 不成立
       SUM(bar.high_units * 64 > bar.low_units * 144) AS range_over_44_36_band,   -- high/low > 144/64：+44%/-36% 不成立
       MIN(bar.low_units * 1.0 / NULLIF(bar.open_units, 0))  AS min_low_over_open,
       MAX(bar.high_units * 1.0 / NULLIF(bar.open_units, 0)) AS max_high_over_open
FROM lst l
JOIN market_series s ON s.entity = l.exchange || '.' || l.code AND s.period = 1 AND s.adjustment = 0 AND s.instrument_class = 0
JOIN series_bars bar ON bar.series_key = s.series_key AND bar.trade_day = l.d0
WHERE substr(l.code,1,3) IN ('300','301','302') AND l.d0 >= 14547 AND l.d0 < 18498
GROUP BY 1;
```

### Q6 上市日不在交易日历里的股票数

```sql
SELECT COUNT(*) AS entities_listing_day_not_in_calendar
FROM (SELECT exchange, code, MIN(listing_date) AS d0 FROM security_master
      WHERE instrument_class = 0 AND listing_date IS NOT NULL GROUP BY exchange, code) l
WHERE NOT EXISTS (SELECT 1 FROM trading_days t WHERE t.trade_day = l.d0);
```

### Q7 切换日前后上市的新股（E1、E2）

```sql
WITH
td AS (SELECT trade_day, ROW_NUMBER() OVER (ORDER BY trade_day) - 1 AS idx FROM trading_days),
lst AS (SELECT exchange, code, MIN(listing_date) AS d0 FROM security_master
        WHERE instrument_class = 0 AND listing_date IS NOT NULL GROUP BY exchange, code),
w AS (SELECT l.exchange || '.' || l.code AS entity, substr(l.code,1,3) AS pfx, l.d0,
             (SELECT trade_day FROM td WHERE idx = a.idx + 4) AS d4
      FROM lst l JOIN td a ON a.trade_day = l.d0),
b AS (SELECT w.entity, w.d0, bar.trade_day,
        CASE WHEN w.pfx IN ('600','601','603','605','000','001','002','003') THEN 'main'
             WHEN w.pfx IN ('300','301','302') THEN 'gem' ELSE 'other' END AS board
      FROM w
      JOIN market_series s ON s.entity = w.entity AND s.period = 1 AND s.adjustment = 0 AND s.instrument_class = 0
      JOIN series_bars bar ON bar.series_key = s.series_key
       AND bar.trade_day >= w.d0 AND bar.trade_day <= COALESCE(w.d4, w.d0))
SELECT item, COUNT(*) AS bar_days, COUNT(DISTINCT entity) AS entities FROM (
  SELECT entity, CASE
    WHEN board='main' AND d0 < 19405 AND trade_day >= 19405 THEN 'E1 主板：上市日早于2023-02-17，第2-5日在其后'
    WHEN board='gem'  AND d0 < 18498 AND trade_day >= 18498 THEN 'E2 创业板：上市日早于2020-08-24，第2-5日在其后'
  END AS item FROM b)
WHERE item IS NOT NULL GROUP BY item ORDER BY item;
```

### Q8 1996-12-16 之前的 A 股 bar 数（R7）

```sql
SELECT substr(s.entity, 1, 2) AS mkt, COUNT(*) AS bar_days, COUNT(DISTINCT s.entity) AS entities
FROM market_series s
JOIN series_bars b ON b.series_key = s.series_key
WHERE s.period = 1 AND s.adjustment = 0 AND s.instrument_class = 0
  AND substr(s.entity, 4, 3) IN ('600','601','603','605','000','001','002','003','300','301','302','688','689')
  AND b.trade_day < 9846
GROUP BY 1;
```
