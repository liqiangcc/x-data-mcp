# 来源刷新运维 runbook（G2 记录）

**状态**：代码 schema 26 / `generic-data-v9` 的运维步骤；已随 preview.42 fresh init 部署（2026-11-02），
preview.43 只换二进制，当前生产为 schema 26 / `generic-data-v9`（见 [STATUS](STATUS.md)）。本文件只写
**怎么查**，不改代码、不写库；门禁口径与裁决见 [SLO](SLO.md) 第 2 行、D-66、D-67，
刷新行为见 [exchange-source](exchange-source.md) 与 [v9 契约](contracts/generic-data-v9.md)。

## 1. 什么时候用

- 部署 v9 后测 G2：`tail` <300 s（仅未触发整表）、触发整表时每张表各自 <600 s、`full`
  <60 min。
- 日更的四个同源 item 报 `source_refresh_overdue` / `source_snapshot_not_found` /
  `snapshot_incomplete` 时，先确认最近一次成功 `full` 的日期，再按
  [daily-sync](daily-sync.md)「来源刷新」分诊。
- 复核 `refresh_exchange_sources` 的结果：谁在跑、跑了多久、改了哪些窗口、哪些来源没完成。

## 2. 记录在哪

schema 26 有三张只追加的表（[026_source_refresh.sql](../migrations/tasks/026_source_refresh.sql)）：

| 表 | 一行是什么 | G2 用到的列 |
|---|---|---|
| `source_refresh_runs` | 一次 `tail`/`full` run | `run_id`、`mode`、`status`、`started_at`、`finished_at`、`requests`、`verified_through`、`detail` |
| `source_refresh_run_sources` | 该 run 里一个来源的统计 | `source_id`、`class`、`complete`、`requests`、`snapshots`、`elapsed_ms`、`verified_through` |
| `source_refresh_run_members` | 该 run 为每个请求指纹选中的快照页 | 重放/选择用，G2 不读 |

`started_at`/`finished_at` 是 UTC RFC3339（秒精度，结题的 run 才有 `finished_at`）；
`elapsed_ms` 是该来源从第一个请求到结束的墙钟毫秒（向上取整）。`running` 期间的计数器
与 `verified_through` 都是 0/空，**不是进度**。

## 3. 查一次 run（MCP，推荐先做）

`ops` 入口：

```json
refresh_exchange_sources {"status_of":"<run_id>"}
```

返回 run 行（mode/status/scope/起止时间/requests/reused/changed/failed_windows/
unobserved_windows/`verified_through`/失败列表），不启动新 run；未知 id 是
`refresh_run_not_found`。`full` 的 <60 min 门禁就用这里的
`finished_at − started_at`（不要用 MCP 调用耗时：`full` 是异步的，调用只返回 `running`）。
`tail` 是同步调用，MCP 调用墙钟可作旁证，官方读数仍取 run 行。

`status_of` 不返回 per-source 行，所以“每张被触发的表各自 <600 s”必须查库（下一节）。

## 4. 查 G2 记录（只读库）

路径按部署配置，不写死；生产是 `<部署根>/state/preview.sqlite`。用 Python 的 `sqlite3`
以 `mode=ro` 打开，**不要加 `immutable=1`**（未 checkpoint 的提交只在 `-wal` 里，会读到旧
计数）：

```python
import sqlite3

db = "file:<部署根>/state/preview.sqlite?mode=ro"
con = sqlite3.connect(db, uri=True)

# 最近 10 次 run：full 的 <60min、未触发整表的 tail 的 <300s 看 seconds。
for row in con.execute("""
    SELECT run_id, mode, status, started_at, finished_at,
           CAST(round((julianday(finished_at) - julianday(started_at)) * 86400) AS INTEGER)
             AS seconds,
           requests, verified_through
    FROM source_refresh_runs
    ORDER BY started_at DESC
    LIMIT 10
"""):
    print(row)

# 某个 run 的每来源行：触发整表时看两张表的 elapsed_ms。
run_id = "<run_id>"
for row in con.execute("""
    SELECT source_id, class, complete, requests, snapshots, elapsed_ms, verified_through,
           failed_windows, unobserved_windows
    FROM source_refresh_run_sources
    WHERE run_id = ?
    ORDER BY source_id
""", (run_id,)):
    print(row)
```

判定表（也是 [SLO](SLO.md) 第 2 行的 G2 口径）：

| 场景 | 读什么 | 判定 |
|---|---|---|
| `tail`（succeeded），没有触发整表 | 两张探针表都没有 `complete=1` 的行，也没有 `snapshots>1` 的行 | `source_refresh_runs` 的 `finished_at − started_at` <300 s |
| `tail`（succeeded），探针触发了整表 | `source_refresh_run_sources` 中 `source_id ∈ {szse_listing, szse_short_name}` 且 `complete=1` 或 `snapshots>1` 的行 | 每行 `elapsed_ms < 600000`；run 总时长**不设门禁** |
| `full`（succeeded） | `source_refresh_runs` 的 `finished_at − started_at` | <60 min |

- **怎么判断“触发”**：只看 `source_refresh_run_sources` 的 `complete` 与 `snapshots`。安静探针
  是 `complete=0`、`snapshots=0`（探针页不写成员）；触发并成功的整表是 `complete=1`（页集通常也
  `snapshots>1`）。
  **不要用 `requests`**：它是 `client.attempts()`，包含重试，没触发的探针重试一次也会是 2，
  会被误判成触发。触发但失败时该来源仍 `complete=0`，由 run 的失败列表（或该来源
  `failed_windows>0`）指出；这种 run 不参与门禁，但必须按要求记录失败。便宜的整表源
  （`sse_listing`/`sse_delisting`/`szse_delisting`）本来就在每次 `tail` 的计划里，属于正常
  tail 成本，不按 D-67 的“被触发表”单独计量。
- **门禁只用成功样本，失败必须逐条记录**：只有 `status='succeeded'` 的 run 参与 G2 门禁；
  `failed`/`process_restarted` 的 run 不参与判定，但测量记录里必须逐条列出每个这类 run 的
  `run_id`、耗时与失败码，不得省略。`running` 还没有 `finished_at`，只记录状态与
  `started_at`，它也不推进 `verified_through`。
- **`verified_through` 要看来源行**：`source_refresh_runs.verified_through` 只表示“这个 run
  至少有一个来源完整”，不能拿它当某个窗口源已完整。窗口源在 `tail` 里只看了两个窗口
  （该来源行 `complete=0`），它的 H=14 天闸门读的是 `source_refresh_run_sources` 里该来源
  `complete=1` 的最新日期；fresh init 后必须先有一次成功的 `full`，四个同源 item 才有输入。
- **失败读取**：`source_refresh_runs.detail` 是失败列表 JSON（`source_id`、窗口、
  `code`、`detail`）；per-source 的 `failed_windows`/`unobserved_windows` 分别表示失败窗口与
  已观测的 0 行窗口（后者是覆盖 gap，不是失败）。不要用 `update_data` 补来源数据，也不要
  `retry_task` 那四个同源 item；重跑顺序见 [daily-sync](daily-sync.md)「来源刷新」。

## 5. 记录测量

按 [SLO](SLO.md) §3 记录主机、代码 SHA、二进制 digest、DB 大小与页缓存冷热、测量时
`MemAvailable` 最低值，并注明是否独占；G2 的每条读数附 `run_id`、`mode`、`status`、
`started_at`/`finished_at`（或触发表的 `elapsed_ms`）。门禁只用 `status='succeeded'` 的 run
判定；同时必须逐条列出每个 `failed` 或 `process_restarted` 的 run（`run_id`、mode、起止时间、
耗时、失败码），不得省略。门槛判定由 owner 按原始数据做，runbook 只负责取数。
