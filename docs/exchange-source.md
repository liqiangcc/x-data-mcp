# 交易所来源共用入口（M3 PR-2）

五个使用方（A-1 上市名单、A-2 终止上市名单、历史 ST、A-3 停复牌、S-6 简称历史）共用
一条「获取 → 冻结 → 校验 → 绑定」链路，设计见
[issue-446 M3 设计](tasks/issue-446-m3-delisted-industry.md) §6 与 §9.4。本文只记录
PR-2 落地的入口、边界与失败语义；各来源的 parser 与写入器分别由后续 PR 提供
（PR-3/PR-10/PR-11/PR-12）。

## 1. 代码位置与进程边界

- 模块：`src/exchange_source/`
  - `request`：规范化请求指纹、上交所/深交所分页方言、季度分段；
  - `transport`：有界重试与 HTTP 客户端（helper 子进程）；
  - `validate`：HTTP 状态、格式、分页计数、0 行策略与失败分类；
  - `freeze`：原始字节先入来源库，再写索引行；
  - `bind`：按 `source_id` 绑定快照并给出 `source_binding_digest`；
  - `index`：`exchange_source_snapshots` 行、规范化摘要与 `--scope exchange` 重放。
- 索引存储：`src/sqlite_market/exchange_source.rs`（只存索引，不存字节；Q7）。
- CLI：`source_store replay|verify ... --db <market.sqlite>`（见
  [来源库说明](source-store.md) §4）。

**联网边界与现在的 acquire 一致**：Rust 侧不持有 socket。`transport::Helper` 以子进程
运行 `scripts/exchange_fetch.py`（仅标准库），只搬运字节与响应头；`scripts/build_release.sh`
把它和 `tdx_fetch.py` 一起打进 release 的 `scripts/`。helper 路径由
`X_DATA_MCP_EXCHANGE_FETCH` 指定，未设置时只认
`<exe dir>/../scripts/exchange_fetch.py`（与 #375 的 fail-closed 规则一致）。入口由
acquire 进程调用，因此不新增 systemd 单元、socket 或沙箱规则。

## 2. 请求指纹与分页

- 指纹是请求本身的规范 JSON：`version` + `source_id` + URL + 排序后的参数（含分页与
  分段参数）。相同指纹 + 相同响应 sha256 只登记一次（§6.3）。
- 分页方言按来源固定：沪名单与公告索引用 `pageHelp.pageNo`（名单同时发
  `pageHelp.pageSize=2000`；公告索引的 `pageSize` 仍是它的自有参数），深交所用 `PAGENO`，
  `commonSoaQuery` 用 `pageHelp.pageNo` + 固定 `pageSize=25`（PR-11 实测）。名单端点不再发
  `beginPage`/`endPage`：2026-10-30 实测两种写法都只回第 1 页同一批 50 行（分页失效），
  而 `pageHelp.pageNo=1&pageHelp.pageSize=2000` 一次返回全部 1,845 行（`pageCount=1`、
  `total=1,845`，自洽）。
- 信封解析（`PagingFormat`）只读分页计数，不解释业务字段：
  - `exchange-sse-page-v2`：`pageHelp.pageNo`（缺失回退 `beginPage`）/ `pageHelp.pageCount`
    / `pageHelp.total` / `pageHelp.data`；
  - `exchange-szse-page-v1`：`[0].metadata.pagecount` / `recordcount` / `[0].data`；
  - `exchange-sse-soa-page-v1`：`pageHelp.pageNo` / `pageHelp.pageCount` /
    `pageHelp.total` / `pageHelp.data`（`commonSoaQuery` 方言）。
  这些版本号写进清单的 `parser_version`，重放用同一版本重算行数/页数；名单方言换过一次
  （`exchange-sse-page-v1` → `v2`），旧快照仍按登记的旧版本重放。
- 分段：公告索引下界 `2000-01-01`，单请求区间 ≤93 天且按自然季度对齐
  （`quarter_segments`）。超过 93 天的区间在构造时即被拒（`segment_too_long`），
  不会发出请求。

### 运行内缓存

同一次运行里多个 item 需要同一请求时，用 `RunCache`（设计 §10.1）：键是规范化请求
指纹，命中即跳过下载与冻结，直接复用已冻结快照的 `FrozenBytes` 与信封计数；因为
清单行与索引行都已存在，这一页在结果里计为 `reused`。缓存只在显式共享时生效
（`Entry::with_cache`），不共享时行为与无缓存一致。校验仍在窗口结束时整体执行，
缓存不会绕过 `truncated`/`unverified_zero` 判断。

## 3. 冻结顺序与失败语义

```text
fetch（有界重试，仅网络错误 / 429 / 5xx）
  → 非 2xx：source_unavailable（不冻结，不发解析器）
  → 2xx：blob + 清单行（fsync）——解析之前
  → 信封解析 → exchange_source_snapshots 行（一个窗口一个事务）
  → 窗口计数校验（页码/页数/total/0 行）
  → bind：source_binding_digest
```

索引是来源库的可重建加速器（§9.4.2），因此**清单里每个能解析出信封的冻结响应都会
有一行**，包括随后被判 `truncated` / `unverified_zero` 的窗口：这些是窗口裁决，不是
索引状态；把它们排除在外会让「清库重放」无法复现，AC-M3-9 就失去意义。真正的准入
由 `basis_status` 与本次获取结果决定——校验失败时入口返回错误，使用方不写任何派生
行。信封解析不出来的响应没有行数可记，进 `problems`（`source_invalid`）而不是伪造
一行。

- 解析失败不删字节：清单与 blob 保留，索引行不写（无计数可记），失败码
  `source_invalid`。
- 0 行默认失败（`unverified_zero`）：只有「整段窗口晚于今天」这一种显式声明才允许 0 行
  （`EmptyPolicy`）。「0 行」不是「没有事件」的证据（§6.4、`work/evidence-notes.md` U-1）。
- 计数不一致（`pageHelp.total`/`recordcount` 与收集行数不符、页数不符、重复页）一律
  `truncated`，入口返回失败；已冻结的页仍按上面的规则入索引。上交所名单的
  `pageHelp.total` 现为真实总数（2026-10-30 实测）；旧快照里的 `total=0` 仍按「未声明
  总数」处理而不是「总数为 0」（M2 时期 `GPLB_GP_L` 恒为 0），页数/重复页校验照旧，
  0 行仍是 `unverified_zero`。
- 单页计划（A-1/A-2 的四个沪名单）遇到第 1 页 `pageCount>1` 直接失败（`page_size_ignored`，
  detail 以 `declared total … but collected …` 开头）：请求里的 `pageHelp.pageSize` 被忽略、
  只拿到首页，不得回退成多页循环或静默去重后放行。
- 多段运行（如按季度循环）逐段隔离：成功的段照常写库；失败的段不出行，整体返回
  `partially_failed` 并列出失败窗口。已冻结快照保留。
- `basis_status` 低于写入门槛时 `bind` 返回 `source_not_admitted`，`Entry` 在发请求前
  就拒绝该计划；不会用 0 行冒充空数据。
- 写入顺序不可配置：blob + 清单 fsync 成功后才有索引行；「索引有行、blob 不可读」
  在重放与绑定读面上都是 fail-closed。

## 4. 重放 `--scope exchange`

`source_store replay --store <dir> --db <market.sqlite> --scope exchange`：

1. 选择清单里 `store_kind=http_fetch` 或 `source_id` 以 `sse_`/`szse_` 开头的记录；
2. 逐 sha 读回字节并校验摘要（不符 → `source_store_corrupt`，整个 scope 失败）；
3. 用清单登记的 `parser_version` 重算行数/页数，重建
   `exchange_source_snapshots`（先清空、再插入，单事务）；
4. 与写完后回读的摘要比较，`match` 才算成功；`--verify-only` 只重算并比较，输出
   `match` / `drift` + 前 20 条差异 `snapshot_id`。

重放与获取写出的行集合相同：能解析出信封的冻结响应都有一行，解析不出的都没有。
因此 `truncated` / `unverified_zero` 窗口留下的字节在重放后仍只产生 0 行快照，
不会凭空多出「看起来可用」的数据，也不会让摘要对不上。

同一响应字节被多条清单行登记时，取「最早 `acquired_at`、其次最小 `origin`」的那条，
因此重放是可重复的。`as_of` 取首次获取时间的 UTC 日期；`url` 与
`request_fingerprint` 从清单 `origin` 的规范指纹读回。无法解析的冻结响应不生成索引行，
在 `problems` 里以 `source_invalid` / `truncated` / `unverified_zero` 报告，scope 状态为
`partially_failed`，不会被静默跳过。

## 5. 清单字段新增

`manifest.jsonl` 增加一个可空字段 `http_etag`：交易所响应的 `ETag`。`Last-Modified`
沿用既有的 `publisher_last_modified`，而 `ETag` 没有其他落点，缺了它重放就无法重建
`exchange_source_snapshots.http_etag`（§9.4.2）。旧清单行没有该字段仍可读（可空、
additive）。

`parser_version` 现在记录信封解析器版本（`exchange-sse-page-v1` /
`exchange-szse-page-v1`），重放据此重算行数与页数。具体来源 parser 从 PR-3 起
登记自己的版本；未登记的版本按 §9.4.1 一律 fail-closed（`unknown_parser`，
scope 不重建），不会用「不认识就跳过」掩盖字节。

## 6. A-1/A-2 消费者（`derived:security_master`）

PR-3 起证券主表的交易所分支由 `src/official_security/a1a2.rs` 消费本入口：

- 四个 source_id、七个请求：沪 `sse_listing`（主板 A `STOCK_TYPE=1` / 科创 `=8`）、
  沪 `sse_delisting`（同上两种 + `COMPANY_STATUS=3`）、深 `szse_listing`
  （`CATALOGID=1110`）、深 `szse_delisting`（`CATALOGID=1793_ssgs` + `selectModule`
  `main`/`nm`）。七个请求共用一个 `RunCache`。
- 来源库位置由 `X_DATA_MCP_SOURCE_STORE` 指定（部署为 `<deploy-state>/source-store`）。
  未设置时该 item 不失败：交易所分支写 `pending_source`，只落 TDX 事实。
- 业务字段 parser 版本 `exchange-a1a2-v2`（v1 → v2：沪行改读 `A_STOCK_CODE`，缺失回退
  `COMPANY_CODE`，两者都有且不等 → `source_invalid`）与来源绑定登记在 item 的结果证据
  `exchange_source` 里；清单的 `parser_version` 仍是信封版本，因为
  `--scope exchange` 重放只重建行数/页数索引。**不要**把该证据加进 generation
  evidence：同一代的 whole-market item 共用同一个 binding digest
  （`derived:suspensions` 会重算并拒绝跨代主表），追加字段会让其余 item 失败。
- 沪名单计划是单页计划：`pageHelp.pageSize=2000` 一次取完（实测最大 1,845 行），
  `pageCount>1` 失败（`page_size_ignored`），`pageCount=1` 时每个计划只发 1 次请求。
- 失败码沿用本入口（`source_unavailable` / `source_invalid` /
  `source_not_admitted` / `partially_failed` / `page_size_ignored`）；
  `derived:security_master` 因此失败时，
  上一版主表保持可读，TDX 日更不受影响。

### A-3 停复牌消费者（`derived:suspensions`）

PR-11 起 `derived:suspensions` 的 `confirmed` 分支由 `src/official_security/a3.rs`
消费本入口：

- 一个 source_id `sse_suspension`：`commonSoaQuery.do?sqlId=GW_PL_JYTS_TFPXX`，
  `isPagination=true`、`pageHelp.pageSize=25`、`pageHelp.pageNo=<n>`，
  `startStopDate`/`endStopDate` 为 `YYYYMMDD`（带连字符会被静默读成 0 行）。
- 分段：下界 `2008-01-01`，按自然季度到**昨天**（当天不完整，不查）。
- 0 行窗口用 `AcquisitionPlan::zero_rows_not_observed()` 记成 `not_observed`
  并进入 item 证据；默认 `ZeroPolicy::FailClosed` 不变，其它消费者照旧
  `unverified_zero` 失败。业务字段 parser 版本 `exchange-a3-v1` 只进 item 证据。
- 行语义（D-49/D-60）：只有 `LXTP`（与单日 `LSTP`+`stopTime=WH`）是整日停牌，
  `stopTime=930/AM/PM` 的盘中停牌与多日 `LSTP` 不入行（计数）；`endStopDate` 原样
  作为含端点的 `end_day`，空值 = 该段末日并计未闭合；只收 6 位 `6` 开头且在主表内
  的实体，区间再按 §11.1 主表窗口裁剪（BR-M3-2）。
- 未配置 `X_DATA_MCP_SOURCE_STORE` 时仍是 `pending_source`（只有 `unresolved_gap`）；
  配置了但抓取失败 → 该 item 失败、不写 `confirmed`（§10.1）。深市没有该来源，
  首期只有 `unresolved_gap`，可交易性 `unknown`。

## 7. 本 PR 不含

- 其余具体来源的 parser 与写入器（PR-10/PR-11/PR-12；PR-3 只接入 A-1/A-2）；
- 真实联网抓取（CI 全部使用夹具；真实抓取只在私有栈验收）；
- 部署脚本改动（重放已是部署步骤，`--db` 的接线由 PR-6 的部署改动承载）。
