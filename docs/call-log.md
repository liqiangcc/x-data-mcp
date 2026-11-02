# MCP 调用日志（调用审计）

**状态**：XDM-167 的代码已合入，随下个 release 部署；部署前已部署实例没有 `XDM_CALL ` 行。
本日志只用于追「谁在什么时候调用了哪个工具、结果与耗时」，**不是**业务审计链：写操作的审计事实仍在
`market_events`、`task_items` 与验证任务里（见 [latest-only-storage](latest-only-storage.md)）。
日志量级见 §4，不改 `tools/list`、capability revision 或 schema。

## 1. 一行是什么

每次 `tools/call` 结束时（成功或失败，包括缺 `name` 的调用）在 **stderr** 写一行：

```
XDM_CALL {"elapsed_ms":87,"entry":"ops","outcome":"ok","task_id":"task-9793e128cfc7b145fefa2e3484b5afed","tool":"get_task_progress"}
```

前缀固定，后面是一个 JSON 对象；systemd 把 stderr 收进 journald，stdout 仍只有协议输出
（[design](design.md)「stdout 仅协议输出，日志独立通道」）。

| 字段 | 取值 | 说明 |
|---|---|---|
| `entry` | `ops` / `research` / `maint` / `acquire` / `stdio` | 服务 socket 的 `--entry`；`acquire_service` 固定 `acquire`；stdio 开发入口（`mcp_data`、`mcp_server`）为 `stdio`，不是线上形态 |
| `tool` | 工具名 | 缺 `name` 的 `tools/call` 记 `?` |
| `request_id` | 调用方标识符 | 参数里的 `request_id`，缺失时取成功结果的同名顶层字段；两者都没有就**不出现该字段**（不是空串） |
| `task_id` | 任务标识符 | 参数或成功结果顶层的 `task_id`；没有就不出现 |
| `outcome` | `ok` 或稳定错误码 | 失败时是 `structuredContent.code` 的值，如 `task_not_found`、`invalid_arguments`、`unknown_dataset`、`storage_error` |
| `elapsed_ms` | 整数毫秒 | 从收到 `tools/call` 到响应构造完成（含响应序列化）的墙钟时间 |

`request_id` / `task_id` 只是**标识符**，不是参数内容：长度截到 38 字符，只保留
`A-Za-z0-9._:/@?-`，其它字符（含引号、反斜杠、换行）写成 `_`。因此一行最多 300 B（含高频的
`query_data`），也不会被换行注入。代价：异常长的 id 无法逐字对照，按前缀核对即可。

## 2. 不记录什么

- 除上面两个标识符外的**所有参数值**：`entities` / `securities` 列表、日期范围、过滤条件、
  cursor、selection、evidence 字符串等；
- 任何**返回数据**：记录行、版本 id、清单、证据；
- 凭据：服务本身不落 token/密钥，日志也不写。

所以从日志不能还原一次查询的内容；需要内容时用 `request_id` / `task_id` 去库里查对应
request、task 或版本。

## 3. 怎么查

```sh
# 最近一小时的调用（-o cat 只留 message 原文，不带 journald 元数据）
journalctl -u x-data-mcp-data --since -1h -o cat | grep '^XDM_CALL '

# 只看某个入口（tunnel 2026-11-02 起连 research.sock，研究调用应为 research）
journalctl -u x-data-mcp-data -o cat | grep '^XDM_CALL ' | grep '"entry":"research"'

# 只看某个工具
journalctl -u x-data-mcp-data -o cat | grep '^XDM_CALL ' | grep '"tool":"query_data"'

# 只看失败
journalctl -u x-data-mcp-data -o cat | grep '^XDM_CALL ' | grep -v '"outcome":"ok"'

# acquire 是另一个单元
journalctl -u x-data-mcp-acquire -o cat | grep '^XDM_CALL '

# 结构化解析（jq）
journalctl -u x-data-mcp-data --since today -o cat \
  | sed -n 's/^XDM_CALL //p' | jq -c 'select(.entry=="ops")'
```

用 `-o cat`（或 `-o short` 后 grep 原文），不要用 `-o json` 再 grep：那样 message 里的引号会被
转义成 `\"`。时间戳用 journald 的（`journalctl` 默认列），日志行本身不带时间字段——这样一行更短，
也不会出现两个时钟。

## 4. 日志量

每次调用一行，不聚合、不采样。行长随 id 长度与 `elapsed_ms` 位数变化，典型 **120–175 B**
（`query_data` 一次约 123 B；带 `task_id` 的 `get_task_progress` 约 132 B），硬上限 300 B。

- 一次日更按 v8 预算口径约 **22 次** `tools/call`（`plan`×2、`run`×2、轮询约 10、acquire 8），
  约 **3–4 KB/日**；轮询更密的长跑（例如 80 次调用）约 **12 KB/日**。
- 即使每次调用都顶到 300 B，22 次也只有 **6.6 KB/日**。
- 研究读面（`query_data` 等只读调用）每调用一行，同样在百字节级。

这是**估算**，不是实测流水：日更实际调用次数取决于轮询间隔与 run 时长（见 [SLO](SLO.md)
第 3 行的 write-loop 记录）。相对 journald 默认留存与生产日志，这个量不需要单独轮转或落库；
没有新增文件、没有新的数据库表。

## 5. 覆盖边界

- `tools/call` 之外的方法（`initialize`、`tools/list`、通知）不记；`run_worker_once` 由 worker
  线程内部调用时不是 `tools/call`，不记；通过 MCP 显式调用它时照常记一行。
- `mcp_connect` 只是帧转发，不产生日志行；行由被连的服务进程写，所以归属它的 `-u` 单元。
- 日志缺失（例如进程在写响应前崩溃）不改变调用结果；本日志不是完成凭证，也不替代
  `task_items` / `market_events` 的业务审计。
