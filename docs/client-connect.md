# 客户端连接：ssh + mcp_connect（vm2 只读研究入口）

**状态**：2026-11-07 在 aws-node 客户端上实测通过。本文只记录**连接契约、前置条件、限制和已测读数**；
主机、版本、generation 等运行事实以 [STATUS](STATUS.md) 为准，工具清单以运行实例的
`tools/list` 与 `get_data_capabilities` 为准。

这条路不改变 [D-74](decisions.md)：vm2-126 仍然**没有 tunnel**，aws-node 的 tunnel 继续作为 fallback。
新增的是客户端经 ssh 直连 vm2 上只读 `research.sock` 的方式，不需要在 vm2 开 tunnel，也不改生产配置。

## 1. 连的是什么

客户端在本机启动一个 `ssh` 进程，远端执行 `mcp_connect <socket>`；`mcp_connect` 把 stdio 上的
JSON-RPC 原样代理到 vm2 上的 Unix socket。对 MCP 客户端来说，这就是一个普通的 **stdio server**：
`stdin`/`stdout` 是协议通道，`stderr` 只有代理和服务的诊断信息。

| 项 | 值 |
|---|---|
| 入口 | `research.sock`（`data_service --entry research`） |
| 性质 | **只读研究面**；不采集、不建任务、不写库 |
| 已部署工具数 | 17（preview.46；以 `tools/list` 为准，数量不是稳定契约） |
| capability | `generic-data-v11`（`production_ready=false`，owner 个人研究，不对外分发） |
| 协议 | JSON-RPC 2.0，**一行一个 JSON 对象**（不是 `Content-Length` 分帧） |
| 鉴权 | ssh 登录账号；远端 `mcp_connect` 还要求账号 UID 等于 socket 属主（当前部署为 `root`） |

不要用这条路连 `mcp.sock`（`ops`，含写工具）、`maint.sock` 或 `acquire.sock`。本方案只用于读。

## 2. 命令

```bash
ssh -o BatchMode=yes vm2-126 \
  '/opt/x-data-mcp-preview/current/bin/mcp_connect /opt/x-data-mcp-preview/run/research.sock'
```

- `current` 是部署脚本维护的符号链接，版本切换后路径不变；不要写死 `releases/<sha>`。
- `vm2-126` 是 Tailscale 主机名/ssh 别名；客户端按自己的 ssh 配置替换，仓库里不写地址。
- 远端命令是**一个参数**。在通用 stdio 客户端里，把整条 `mcp_connect …` 作为 `ssh` 的最后一个
  `args` 元素，不要拆成两个（拆开也能工作，但引号和空白更容易出错）。

最小冒烟（手工验证协议，不经过任何客户端）：

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' |
ssh -o BatchMode=yes vm2-126 \
  '/opt/x-data-mcp-preview/current/bin/mcp_connect /opt/x-data-mcp-preview/run/research.sock'
```

期望第一行响应的 `result.serverInfo.version` 是当前部署版本，第二行 `result.tools` 是 17 项
（`sh.510050` 的查询示例见 §6）。

## 3. 通用 stdio 客户端配置

只依赖客户端的“stdio server（command + args）”能力，不绑定具体产品。概念配置如下：

```json
{
  "mcpServers": {
    "x-data-mcp-research-vm2": {
      "command": "ssh",
      "args": [
        "-o", "BatchMode=yes",
        "vm2-126",
        "/opt/x-data-mcp-preview/current/bin/mcp_connect /opt/x-data-mcp-preview/run/research.sock"
      ]
    }
  }
}
```

要点：

- 不需要 `env`、HTTP 头或 token；凭据就是 ssh 自己用的密钥/agent。
- `BatchMode=yes` 让 ssh 不弹密码、口令或主机密钥确认；因此**首次连接前**必须已经能非交互登录。
- 如果客户端把 `args` 当 shell 字符串拼接，需自行保证最后一个参数整体加引号；本仓库不提供
  具体客户端的配置模板，因为未逐一验证。

## 4. 前置条件

| 条件 | 怎么确认 | 期望 |
|---|---|---|
| 客户端在同一个 tailnet，能到 vm2 | `ssh -o BatchMode=yes vm2-126 true` | 退出码 0，无提示；同时证明主机密钥已信任（BatchMode 不会接受未知主机密钥） |
| ssh 账号有权限且密钥可用 | 同上；`ssh -v` 可看用的是哪把 key | 非交互登录；账号是 socket 属主（当前 `root`） |
| socket 存在 | `ssh -o BatchMode=yes vm2-126 'test -S /opt/x-data-mcp-preview/run/research.sock && echo socket-ok'` | `socket-ok` |
| 服务 active | `ssh -o BatchMode=yes vm2-126 'systemctl is-active x-data-mcp-data'` | `active` |

如果 socket 不存在或服务不是 `active`，先按 [新机器部署手册](new-host-runbook.md) / [STATUS](STATUS.md)
排查服务，不要反复重试客户端。目录和 socket 是 owner-only（`0700` / `srw-------`），换一个非属主账号
登录会在连接阶段被拒绝（`forbidden peer UID` 或权限错误），不是数据问题。

## 5. 限制

- **ssh 会话断开即无数据。** 传输就是这条 ssh；连接一断，MCP 客户端看到 EOF。客户端要自己重连并
  重新发起查询；未取完的分页不要跨会话复用 `cursor`。
- **BatchMode 是非交互的。** 没有密码提示、没有口令提示、没有未知主机密钥确认。需要密钥已经在
  agent 里或无需口令，且 `known_hosts` 已就绪。
- **只读。** `research` 入口不注册写工具；不要为了“顺手更新”把 ssh 指向 `mcp.sock` 或
  `acquire.sock`。本方案不承担写操作、采集或日更。
- **鉴权在 ssh 和 socket 属主，不在 `mcp_connect`。** `mcp_connect` 只是传输代理，不额外做用户
  隔离或工具过滤；能登录该账号的人就拥有该 socket 的全部工具面。
- **每个 stdio 会话一个 ssh 进程。** 延迟包含 ssh 通道；大页读取会受 ssh 加密/转发和分页大小影响。
  用 `page_size`/`cursor` 正常翻页，不要用单次超大请求硬扛。
- **仍然不是对外服务。** `production_ready=false`、`declared_scope` 为 owner 个人研究、local-only；
  这条路不改变 #71 的不对外分发边界。

## 6. 实测证据（2026-11-07）

客户端在 aws-node，经 Tailscale 内网到 vm2-126；以下为同一天复跑同一条 §2 命令的读数。原始会话
记录在仓库外，这里只保留命令与数字。

| 项 | 读数 |
|---|---|
| `initialize` | `serverInfo.name=x-data-mcp`、`serverInfo.version=0.2.0-preview.46`、`protocolVersion=2025-06-18` |
| `tools/list` | 17 项（`research` 入口） |
| `get_data_capabilities` | `capability_revision=generic-data-v11`、`production_ready=false` |
| `query_data`（`sh.510050`，`adjustment=raw`） | **7 根 bar**，`price_scale=3`；窗口 `2026-10-29`–`2026-11-06`，close 依次为 `2.871, 2.862, 2.871, 2.884, 2.923, 2.930, 2.902` |
| 延迟 | 同一 ssh 会话内连续 5 次 `query_data`，**中位 117 ms**（5 次均为 117 ms，含 ssh 通道） |
| 建连开销 | 每个新 stdio 会话另起一个 ssh，5 次独立会话实测中位约 275 ms（约 0.16 s 是建连/进程固定开销，不含在 117 ms 内） |

复跑用的 `query_data` 输入（`query_data` 不接受 `request_id`，多传会得到 `invalid_arguments`）：

```json
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
  "name":"query_data",
  "arguments":{
    "dataset":"market.daily",
    "contract_version":"v2",
    "adjustment":"raw",
    "selection":{"kind":"single","entity":"sh.510050"},
    "filter":{"kind":"daily","start_date":"2026-10-29","end_date":"2026-11-06"},
    "version":{"kind":"latest_per_entity"},
    "fields":["open","high","low","close"],
    "page_size":100
  }
}}
```

ETF 省略 `adjustment` 会返回 `adjustment_required_for_etf`；`qfq` 对 ETF 当前 fail-closed
（`adjustment_unavailable`）。这是数据契约，不是连接问题，见 [generic-data-v11](contracts/generic-data-v11.md)。

## 7. 与 aws-node tunnel 的关系

| 项 | vm2-126（本方案） | aws-node（fallback） |
|---|---|---|
| 客户端入口 | ssh + `mcp_connect run/research.sock` | 既有 tunnel（`research.sock`） |
| 部署版本 | preview.46，schema 27 | preview.43（冻结），schema 26 |
| capability | `generic-data-v11` | `generic-data-v9`（冻结） |
| 数据 | 生产日更和主数据 | 冻结在 2026-11-04 |
| 适用 | **新客户端** | 既有客户端 / vm2 不可用时的 fallback |

- **fallback 仍连 aws-node**：tunnel、data、acquire 单元仍在 aws-node 上；它没有 2026-11-04 之后的
  数据，也没有 vm2 上 11-05 起补的 ETF 等数据。回退时接受“读到的是旧快照”。
- **新客户端应连 vm2**：通过本文的 ssh 路径取生产数据。两台机器的库不是同一份：vm2 是 fresh init
  后由来源库重放/首采重建的，aws-node 是原地保留的冻结副本；同一查询得到的日期范围、版本和
  工具面都可能不同，不能把 fallback 当一致性副本。
- **不要双写**：这条路径只读，不存在“两个入口同时写”的问题；日更和写操作仍只在 vm2 的
  `mcp.sock` / `acquire.sock` 上，由原来的编排入口执行。
- vm2 上没有 tunnel 单元（D-74），所以 ssh 是 vm2 目前唯一的外部客户端路径；tunnel 是否搬过去
  仍由 owner 决定（见 [新机器部署手册](new-host-runbook.md) §5）。
