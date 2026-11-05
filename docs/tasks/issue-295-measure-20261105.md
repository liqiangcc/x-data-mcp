# #295 / XDM-14：全市场筛选耗时测量（schema 27，`generic-data-v10`）

测量时间 2026-11-05 18:29–18:33 CST（上海），主脚本 `harness.py` 正式运行 4 分 23 秒，另有两个短探针。
只读：只用 `research.sock`（经 `mcp_connect`）和 `file:…?mode=ro` 的 SQLite 连接，没有调用写工具，没有编译。

## 1. 测量口径

| 项 | 值 |
|---|---|
| 主机 | vm2-126（生产机，2 vCPU，7.6 GiB RAM，boot_id `b471573e-ddeb-4ec1-ab4e-118e40bb4412`） |
| 部署版本 | `0.2.0-preview.45`，`git_sha f6de633dffcd04467fd790463291f301223adc72`（`current/manifest.json`），rustc 1.99.1 |
| capability revision / schema | `generic-data-v10` / 27 |
| active generation | `c642468f85e29dce79fe4460bda6a8d1175ef60be3c0afd2520cb1c75e7544d6`（2026-11-05T08:40:27Z 激活）；测量前后相同，`market_series.updated_at` 最大值前后不变（`2026-11-05T09:01:58Z`），数据在测量期间没有变化 |
| 数据规模 | `state/preview.sqlite` 2,216,542,208 字节；`market_series` 18,117；`series_bars` 37,831,020；当前 catalog 成员 5,225；日线序列（前复权）5,562；年线序列 5,224；最新交易日 2026-11-05 |
| 客户端 | 单个 Python 3.12.3 进程，串行请求，一个 `mcp_connect` 子进程（MCP stdio），SQLite 3.45.1 |
| 并发 | 1。没有并发测试。测量期间没有日更：`daily` agent 状态 done，`tasks`/`acquisition_tasks` 无非终态任务，load average ≈ 0.2。每个场景前 `harness.py` 重新检查这些条件和 `MemAvailable`（测量期 ≥5.2 GB） |
| 缓存状态 | **热**。`mincore` 实测 DB 文件 87–93% 页在 page cache，服务进程已预热（RSS 233→352 MB）。**冷缓存没有测**：要 drop cache 会拖慢同机生产读取，不做。第 1 次重复和后两次的差别（见下）是热缓存下的抖动，不是冷启动 |
| 重复 | 每个场景 3 次，逐次数字在 `results.json`；下表给 最小/中位/最大 |
| 服务端耗时 | `journalctl -u x-data-mcp-data` 的 `XDM_CALL` `elapsed_ms`，每次运行后逐条和调用数核对（`journal_match: true`，全部相符） |
| 客户端分段 | `wall` = 写请求到读完整行（含 `mcp_connect` 中转）；`parse` = `json.loads`；`fold/screen` = 把列式结果折成字典、再算筛选；`wall − 服务端` = 传输与中转 |

脚本：`mcpc.py`（MCP 客户端）、`harness.py`（场景与校验）、`probe_scaling.py`、`probe_bytes.py`。原始数据：`results.json`、`probe_scaling.json`、`probe_bytes.json`、`run.log`。
脚本 sha256：mcpc `77497cb6…`，harness `c4fb5d1e…`，probe_bytes `f5e2ae05…`，probe_scaling `7a9e8a3b…`。
本仓库 HEAD 是 `44b9320`（只含文档），部署二进制对应 `f6de633`；我没有读源码做机制归因，下文凡写机制的地方都标明是推断。

## 2. 筛选定义

- 年线：对 catalog 当前沪深 A（5,225 只）取 2022–2025 四个完整自然年（`period=2`，前复权），两个口径各算一次：
  - A「三年收阴」：2023、2024、2025 每年 `close < open`（要求 2022 也存在，保证四年齐全）。
  - B「连续三年收盘下跌」：2023/2024/2025 每年收盘低于上一年收盘。
  - 结果：A **231** 只，B **241** 只。所有路径给出同一结果（排序后 SHA-256 前 16 位 `47ee7e73bca62d64`，MCP 全字段、MCP 投影、SQL 下推、SQL 取行后 Python 计算四者一致）。
- 日线截面：catalog 当前沪深 A，最近 5 个交易日（2026-10-30、11-02、11-03、11-04、11-05）的 open/high/low/close/volume/amount。
  `get_market_snapshot` 在本仓库和 research 入口里不存在（`tools/list` 只有 5 个只读工具），我按字面「最近 5 个交易日的全市场截面」实现，不代表某个已有工具的形态。
  - 结果 26,085 行（5,225 × 5 = 26,125，少 40 行为无该日 bar）。MCP 批量（全字段）、MCP universe、SQL 三者的逐格 Decimal 值一致（SHA-256 前 16 位 `69ca7584c1efd6bc`）；MCP 只取 close 与另两者的 close 列一致（`78c22a732525c809`）。

## 3. 年线「连跌三年」

`market.yearly` 的 `selection` 只接受 `single`/`batch`（`market` 与 `universe` 都被拒，`invalid_arguments`），`max_batch_entities=100`。所以调用序列是：`securities.catalog` 分页取成员（`page_size` 1000，6 次）+ 53 个 100 只的 `batch`，共 **59 次调用**。

| 场景（各 3 次，min/中位/max） | 调用 | 总字节 | 墙钟 s | 服务端 `elapsed_ms` 合计 s | JSON 解析 s | 折叠+筛选 s |
|---|---|---|---|---|---|---|
| MCP，全字段 | 59 | 16,818,480 | 10.50 / 10.79 / 11.71 | 10.26 / 10.55 / 11.48 | 0.067 / 0.071 / 0.073 | 0.020 + 0.008 |
| MCP，`fields=[open,close]` | 59 | 13,402,964 | 10.73 / 11.26 / 11.45 | 10.53 / 11.06 / 11.26 | 0.052 / 0.053 / 0.054 | 0.017 + 0.009 |
| SQL 下推（`mode=ro`，窗口函数/聚合） | 0 | 0 | 0.092 / 0.093 / 0.094 | — | — | — |
| SQL 取 19,977 行 + Python Decimal 筛选 | 0 | 0 | 0.066 / 0.068 / 0.070 | — | — | — |

分解（全字段，中位）：墙钟 10.79 s = 服务端 10.55 s（97.8%）+ 传输/中转 0.24 s（2.2%）；之外 JSON 解析 0.07 s、折叠+筛选 0.03 s（合计约 1%）。客户端 CPU 0.14 s，客户端最大 RSS 30 MB。
59 次里前 6 次是 catalog（0.18 s），其余 53 个 batch 平均 215 ms/次、约 317 KB/次。

字节中 47.7% 是 MCP 响应里 `content[0].text` 对 `structuredContent` 的重复（每次响应两份，8.02 MB / 16.82 MB）。一个 100 只的年线 batch 响应共 318,893 字节，其中结构化部分 152,815 字节：`versions` 106,578 字节（70%），`columns` 38,164 字节（25%）（`probe_bytes.json`）。

## 4. 日线最近 5 个交易日截面

| 场景（各 3 次） | 调用 | 总字节 | 墙钟 s min/中位/max | 服务端合计 s | JSON 解析 s | 折叠 s |
|---|---|---|---|---|---|---|
| MCP `batch`×100 + `cross_section`，全字段 | 59 | 10,279,070 | 3.81 / 4.11 / 4.26 | 3.59 / 3.89 / 4.04 | 0.046 | 0.057–0.066 |
| MCP `batch`×100，`fields=[close]` | 59 | 7,693,200 | 3.73 / 3.84 / 4.07 | 3.59 / 3.69 / 3.93 | 0.028 | 0.016–0.021 |
| MCP `universe`，每个交易日一次 `cross_section`，按 `next_cursor` 翻页，全字段 | 30（5 天×6 页） | 291,877,364 | 47.58 / 47.69 / 48.19 | 45.57 / 45.66 / 46.14 | 0.80 | 0.14–0.15 |
| SQL（`mode=ro`，一条 JOIN） | 0 | 0 | 0.126 / 0.131 / 0.138 | — | — | — |

- `universe` + 5 个交易日的 `cross_section` 一次请求会失败：`INVALID_MARKET_COMMAND: snapshot budget exceeded`（2.2–2.9 s 后返回；上限是单快照 16 MiB / 100,000 行）。2 个交易日能过，3 个及以上失败；`fields=["close"]` 也失败。所以改为每天一次（每天 6 页，每页 1,000 只）。
- `universe` 每天的首页约 3.4 s，其余 5 页约 1.15–1.24 s；每页响应约 9.7 MB（含重复），其中 `versions` 4.51 MB，`columns` 0.18 MB。每页服务端进程 RSS 在这个场景内从 242 MB 涨到 352 MB 并保持。
- `batch` 路径的 59 次调用同样含 6 次 catalog 分页。

## 5. 服务端耗时随什么增长（`probe_scaling.py`，热缓存，每格 5 次，服务端 `elapsed_ms` 中位）

| 请求 | 服务端 ms | 响应字节 |
|---|---|---|
| yearly 2022–2025，1 只 | 5 | 6,604 |
| yearly 2022–2025，10 只 | 21 | 34,936 |
| yearly 2022–2025，50 只 | 99 | 161,030 |
| yearly 2022–2025，100 只 | 214 | 318,548 |
| yearly 2025 一年，100 只 | 114 | 251,218 |
| yearly 4 年，100 只，`fields=[close]` | 213 | 243,500 |
| daily `cross_section` 5 天，100 只 | 74 | 182,744 |
| daily `cross_section` 1 天，100 只 | 71 | 136,732 |
| daily `daily` 250 天，100 只（第 1 页 1000 行） | 119 | 273,706 |

- 服务端耗时与实体数近似线性（年线约 2 ms/只，日线约 0.7 ms/只）。`fields` 缩列不改变服务端耗时，只减字节。
- 日线 1 天和 5 天几乎相同（71 vs 74 ms），说明日线 batch 的耗时主要是按实体的固定成本，不是行数。年线 4 年比 1 年多约 100 ms/100 只，行数相差 4 倍；这与「按行成本」或「按窗口成本」都相容，我没有区分。
- 对照 SQL：同一批 5,225 只的年线数据 SQL 取行加计算 0.07 s，即约 0.013 ms/只；MCP 约 2 ms/只。差距的来源（逐实体的版本头/manifest 校验、序列化、manifest 构造等）**我没有做剖析，不能归因**。

## 6. 与旧数字的关系

旧值 383 s 来自 preview.28 / schema 20（D-19 记录）。本次同类年线筛选在 schema 27 上为 10.8 s（热缓存、单客户端、程序化批量调用），相差一个数量级以上，但两次的筛选定义、数据规模、环境都不同（旧值为 preview.28 当时数据和私有 perf 栈），所以只能说旧数字不再代表现状，不能写成「提速 N 倍」。

## 7. 本次没有测的，以及结论的边界

- 冷缓存、并发（≥2 个客户端同时读）、与日更并行的读，都没测。
- 客户端是脚本，不是 LLM agent。agent 通过 `mcp_connect` 逐页取数时，每页响应（年线批约 319 KB，universe 日线页约 9.7 MB，且 `content.text` 与 `structuredContent` 各一份）都会进入上下文；这个成本不在上面的墙钟里。
- 只测了前复权的 `latest_per_entity`；没有测 `known_at`/`as_of`、`market.index`、ETF 范围。
- 数字来自 2026-11-05 一个下午、一份 generation，且是 vm2-126 的 2 vCPU。
- 正式运行用 `nohup` 从 pi 会话启动，没有用 `systemd-run`（约 4 分钟、纯只读）。

## 8. D-19 四个方向的判断依据（不选方向，不写设计）

**窄投影**
- 支持：`fields` 缩列可减字节：年线 `[open,close]` 少 20%（16.82→13.40 MB），日线 `[close]` 少 25%（10.28→7.69 MB）；JSON 解析时间少 25%（年线）和 39%（日线）。
- 反对：墙钟没有改善（年线 10.79→11.26 s，日线 4.11→3.84 s，都在重复运行抖动范围内）；服务端耗时对 `fields` 不敏感（100 只年线 214 vs 213 ms）。客户端解析本来只占墙钟约 0.6%。
- 当前 `fields` 不能去掉的字节：每个实体一行的 `versions`（年线批 70%，日线批 67%，universe 日线页 95% 的结构化字节），以及重复的 `content.text`。
- 没测：去掉 `versions`/去掉重复文本后的墙钟和 agent 上下文占用。

**放宽批量和分页**
- 当前上限：batch 100 只（`max_batch_entities`）、`page_size` ≤1000、单快照 ≤100,000 行 / ≤16 MiB。年线必须 53 个 batch；日线 universe 一次最多 2 个交易日。
- 支持：5 个交易日的 universe 请求被 snapshot budget 拒绝，只能拆成 30 次 / 292 MB / 47.7 s，慢于 batch 路径的 59 次 / 10.3 MB / 4.1 s。
- 反对：服务端耗时近似与实体数成线性（1 只 5 ms，100 只 214 ms），把 batch 从 100 放宽不会按比例减少服务端总时间；调用次数的固定成本很小（单只请求 5 ms，53 次的固定部分上限约 0.3 s）。
- 没测：batch 超过 100 时的实际服务端耗时（上限由服务端拒绝，无法测）；universe 路径慢的原因（首页 +2.3 s、后续页 1.15 s/1000 只）。

**服务端筛选**
- 支持：同一结果 SQL 下推 0.093 s，MCP 10.79 s（约 116 倍）；MCP 路径 97.8% 的墙钟在服务端；59 次调用/16.8 MB 的往返被一次调用取代。
- 反对/不确定：SQL 数字绕过了 `query_data` 的完整性校验（D-19 明确不允许把 SQL 旁路放进产品路径），所以 0.093 s 是下限，不是服务端筛选的预期值。服务端筛选如果仍逐实体走现在的读路径，服务端 10.5 s 不变，只省掉约 3%（传输 + 解析 + 客户端计算 ≈ 0.35 s）和往返字节；只有读路径本身变轻才会接近 SQL。
- 没测：逐实体服务端成本里校验与序列化各占多少；谓词下推实现的实际耗时。

**降为低优先级**
- 支持：年线全市场筛选 10.8 s，日线 5 天截面（batch）4.1 s，均为热缓存、单客户端、脚本调用；与旧值 383 s 相比下降一个数量级以上；结果与 SQL 逐值一致，没有正确性问题。
- 反对/不确定：仓库没有定义可接受阈值（issue 明确不预设）；universe 路径 47.7 s / 292 MB 且只能逐日取；agent 客户端的上下文成本未测；冷缓存和并发未测；每个日更周期之外的使用频率没有数据。
