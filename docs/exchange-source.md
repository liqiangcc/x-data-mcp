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
- 分页方言按来源固定：上交所只用 `beginPage`/`endPage`（`beginPage == endPage`），
  深交所用 `PAGENO`；`pageSize` 既不发送也不依赖（实测被忽略，§17 第 9/10 条）。
- 信封解析（`PagingFormat`）只读分页计数，不解释业务字段：
  - `exchange-sse-page-v1`：`pageHelp.pageCount` / `pageHelp.total` / `pageHelp.data`；
  - `exchange-szse-page-v1`：`[0].metadata.pagecount` / `recordcount` / `[0].data`。
  这两个版本号写进清单的 `parser_version`，重放用同一版本重算行数/页数。
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
  → 信封解析与计数校验
  → 校验通过：exchange_source_snapshots 行（一个窗口一个事务）
  → bind：source_binding_digest
```

- 解析失败不删字节：清单与 blob 保留，索引行不写，失败码为
  `source_invalid` / `truncated` / `unverified_zero`。
- 0 行默认失败（`unverified_zero`）：只有「整段窗口晚于今天」这一种显式声明才允许 0 行
  （`EmptyPolicy`）。「0 行」不是「没有事件」的证据（§6.4、`work/evidence-notes.md` U-1）。
- 计数不一致（`pageHelp.total`/`recordcount` 与收集行数不符、页数不符、重复页）一律
  `truncated`，不写任何索引行。
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

## 6. 本 PR 不含

- 任何具体来源的 parser 与写入器（PR-3/PR-10/PR-11/PR-12）；
- 真实联网抓取（CI 全部使用夹具；真实抓取只在私有栈验收）；
- 部署脚本改动（重放已是部署步骤，`--db` 的接线由 PR-6 的部署改动承载）。
