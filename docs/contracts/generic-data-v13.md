# generic-data-v13 契约（长轮询、最近一次 full、获取进度，#625）

**状态**：代码面契约，**未部署**（[#625](https://github.com/liqiangcc/x-data-mcp/issues/625)）。
本文件记录 2026-11-09 生产日更三个现象（无间隔轮询、重复的每周 `full`、下载期间
`bytes_done` 恒为 0）的服务端修法。设计与裁决见
[issue-625](https://github.com/liqiangcc/x-data-mcp/issues/625) 正文与 owner 2026-11-09 的批准评论。

本文件是相对 [generic-data-v12](generic-data-v12.md) 的增量；v12 的读面、任务面、错误码
与部署约束继续有效。**当前部署仍是 `generic-data-v11`**（preview.46，vm2-126），v12 与
v13 都在代码面。

## 1. 相对 v12 的增量

| 项 | v12 | v13 |
|---|---|---|
| `capability_revision` | `generic-data-v12` | `generic-data-v13` |
| data 工具数 | 37（ops 16 / research 17 / maint 9） | **不变** |
| acquire 工具数 | 10 | **不变** |
| compact schema | 27 | **不变**（没有迁移） |
| `refresh_exchange_sources` 的输入 | `mode`/`sources`/`dry_run`/`status_of` | **追加可选 `latest`、`wait_seconds`**（§2、§3） |
| `get_acquisition_task` 的输入 | `contract_version`/`task_id` | **追加可选 `wait_seconds`**（§4） |
| 获取任务的 `bytes_done` | 只在阶段结束时写入 | 下载过程中按块写入，节流到每秒最多一次（§5） |
| 响应形状 | | **逐字节不变**：新参数只出现在 inputSchema，不进入任何响应 |

不改 schema，不改任何现有返回字段，只加可选入参和一个新的只读查询。

## 2. `refresh_exchange_sources({"status_of":…,"wait_seconds":N})`

```text
refresh_exchange_sources {"status_of":"refresh-full-1794120245537117945","wait_seconds":45}
  → 与 status_of 完全相同的 RefreshReport，
    但在 run 到达终态（succeeded / failed）后立即返回，
    或超时（`wait_seconds` 上限）后返回当时的记录。
```

- `wait_seconds` 缺省或为 `0`：行为与 v12 完全相同（一次读，立即返回）。
- `wait_seconds > 0`：服务端最多等待这么久，**上限 50 秒**（与 `get_task_progress` 的
  `MAX_WAIT_SECONDS` 相同，超出按 50 处理，不报错）。
- 超时**不是错误**：返回的是超时时刻的记录（`status` 仍可能是 `running`），下一次调用接着读。
- 轮询间隔 300~500 ms；每次迭代只读一次 `source_refresh_runs` 行并在 sleep 前释放读作用域，
  不跨越 sleep 持有读事务，因此后台刷新线程写 run 记录（`finish_source_refresh_run`）不会被阻塞。
- `run_id` 不存在仍立即返回 `refresh_run_not_found`，不会被等待。
- `wait_seconds` 只能和 `status_of` 一起用；单独出现（或与 `mode`/`sources`/`dry_run` 一起）
  返回 `invalid_arguments`。

## 3. `refresh_exchange_sources({"latest":"full"})`

只读查询，返回最近一次 `status=succeeded` 的 `full` run 记录：

```json
{"run_id":"refresh-full-1794120245537117945","started_at":"2026-11-08T06:44:05.537Z",
 "finished_at":"2026-11-08T08:02:04.112Z","requests":3876,
 "failed_windows":0,"unobserved_windows":4}
```

- 一次 `full` 都没有成功过时返回 `null`（字面量 `null`，不是空对象）。
- `tail` 不算：它只看开立/上一自然季度的窗口与两张探针首页，没有确认整个 fingerprint 集合。
  `failed` 的 `full` 也不算：它观测了整个集合但没有确认它。
- 排序按记录的 `finished_at`（缺省回退 `started_at`）取最新的一条，所以「最近一次完成的」
  优先于「最近一次开始的」。
- 只读：不写 run 行，不触碰 store，不占用单飞锁（`refresh_already_running` 与它无关）。
- `latest` 只接受 `"full"`；其它值返回 `invalid_arguments`。
- `latest` 不能与 `mode`、`sources`、`dry_run`、`status_of` 同时使用，组合时返回
  `invalid_arguments`（与 `status_of` 的互斥校验写法相同）。

用途（Skill 侧，另见 #625 的文字 PR）：每周 `full` 之前先查这条记录，距今不到 7 天就跳过
并在报告里写出那次的 `run_id`。

## 4. `get_acquisition_task({"task_id":…,"wait_seconds":N})`

- `wait_seconds` 缺省或为 `0`：行为与 v12 完全相同。
- `wait_seconds > 0`：**状态有变化就立即返回**，变化指 `state`、`stage`、`bytes_done`
  任一项与进入等待时不同；任务已到终态（`succeeded`/`failed`）时立即返回；否则到
  `wait_seconds` 上限（同样截断到 50 s）时返回当时的记录。
- 轮询间隔 300~500 ms；每次迭代是一次独立的短读，`sleep` 期间不持有任何连接或事务，
  所以 worker 写任务行（包含 §5 的进度写）不会被等待挡住。
- `task_id` 不存在仍立即返回 `task_not_found`。

## 5. 获取进度（`bytes_done`）

下载过程中，`bytes_done` 按块向前推进，节流到**每秒最多一次**写入（并跳过没有增长的写）：

- 值的口径不变：仍是该任务在 `detail.files` 里已记录字节数的合计，阶段结束时的写入照旧是
  权威值；新增的是下载进行中的值，等于「已完成的文件」+「当前文件已落盘字节」。
- 单调不减：恢复下载时报告的较小值不会让它回退。
- 写失败不影响下载：进度写失败只记一行日志，任务继续。
- `bytes_done` 推进不代表阶段变化，也不改变 `state`/`stage`。

实现：服务仍然只托管一个 socket、不自己下载字节。`scripts/tdx_fetch.py` 本来就是按
1 MiB 分块读取写入的循环，现在每落盘一块就在 stdout 打印并 flush 一行 `{"bytes":N}`，
结果对象照旧最后打印。字节搬运、`bytes`/`sha256`/`Content-Length` 校验、temp+rename+fsync
与 manifest 契约全部不变，helper 仍然只用标准库、不新增依赖，release 包也不新增二进制。

服务侧（`src/acquire/fetch.rs`）必须**边跑边读**：用 `Command::spawn()` 配
`Stdio::piped()`，在 `BufReader::lines()` 上逐行读 stdout，每读到一行进度就上报；
stderr 由单独线程收集，避免管道写满互相阻塞。`Command::output()` 会等子进程退出才返回，
用它就只能在下完之后拿到全部进度行，`bytes_done` 在下载期间仍是 0。

进度行只是进度：结果对象永远是最后一行，只读结果的调用方取最后一行即可。

## 6. 能力 revision：v12 → v13

本修订升 `capability_revision`，理由：

- 输入契约变化是客户端可见的（两个工具的 inputSchema 多了一个可选参数，另有一个新的
  只读查询），属于「客户端要知道什么时候能用」的变化；
- 部署面与代码面因此又多一档（部署 v11、代码 v13），而 `check_current_facts.py` 的
  `EXPECTED_DATA_CAPABILITY_REVISION` 与 `EXPECTED_DEPLOYED_CAPABILITY_REVISION` 本来就是
  分开维护的一对常量，升代码常量不动部署常量即可；
- Skill 文字 PR 会钉 `generic-data-v13`，两个 PR 合起来才完整（Skill 侧过渡口径见 #625
  评论：日更不主动发起每周 `full`，由 operator 周末单独跑）。

工具数不变，schema 不变，`tools/list` 的条目集合不变。

## 7. 部署顺序与部署前的影响

1. 部署二进制与 `scripts/tdx_fetch.py` 即可生效，无需迁移、无需新增包内容。
2. 不部署时，行为与 v12 完全相同：`latest`/`wait_seconds` 会因 `deny_unknown_fields` 被判
   `invalid_arguments`，客户端在发请求前即可用 `capability_revision` 区分。
3. helper 与服务的发布节奏必须一起走：进度行是 helper 打印的，新服务配旧 helper 只会
   拿到最后的结果行（`bytes_done` 退回 v12 行为，不报错），旧服务配新 helper 则把多余的
   进度行当解析错误拒绝。`scripts/build_release.sh` 已经把 helper 打进同一个 release 包。

## 8. 未验证

- 未在 vm2-126 上部署，也没有用真实交易所/官方下载量过进度写频率；进度节流与单调性由
  单元测试（注入 fetcher）覆盖，实际 550 MB 归档的写入次数待部署后采集。
- 真实官方下载（550 MB 日档）的网络路径未在 CI 中跑（CI 不出网）；helper 的进度行与
  服务的边跑边读分别由本地 HTTP 夹具与真子进程夹具覆盖。
