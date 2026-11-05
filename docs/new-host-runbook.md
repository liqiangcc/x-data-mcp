# 新机器部署手册

**状态**：2026-11-05 把日更和主数据从 aws-node 搬到 vm2-126（D-74）时的实际做法，整理成可重复执行的步骤。
当前主机、版本、generation 与数据量以 [STATUS](STATUS.md) 为准，本文件不记录它们。试用机到期前要定去向
（STATUS 总览），到期时按本文件建下一台。

不包含：主机地址、tunnel 标识、凭据、交易所原始数据、仓库外的内部报告路径。这些由操作员在执行时从各自的受控位置取得，
不写进公开仓库（`scripts/check_public_hygiene.py` 会拒绝地址和邮箱）。

读数（耗时、行数、数量）是 2026-11-05 在一台 2 vCPU / 约 7.6 GiB / ext4 的机器上一次迁移的实测，只用来判断“差不多对不对”，
不是性能门禁；门禁见 [SLO](SLO.md)。

## 0. 前提

| 项 | 要求 | 为什么 |
|---|---|---|
| 文件系统 | `/opt` 和 socket 目录在 ext4 / XFS / Btrfs / tmpfs（`stat -f -c %T /opt`） | `src/local_filesystem.rs` 只接受这几种；不要设 `X_DATA_MCP_UNSAFE_NONLOCAL_FS`，它会削弱单写者租约 |
| 工具 | systemd、python3（脚本只用标准库）、git、rsync；`gh` 可选（`--force-git-transport` 不需要） | 机器上可以没有 `sqlite3` 命令行，检查一律用 `python3 -c 'import sqlite3'` |
| 磁盘 | 部署根所在盘剩余 ≥ 25 GB；剩余 < 5 GB 就停 | 首次日更后 `/opt` 约 4.5 GB，库约 2.2 GB |
| 仓库 | `/root/x-data-mcp` 是 clone，且含要部署的 release 提交：`git -C /root/x-data-mcp cat-file -e <sha>` | `deploy_phase_g.sh` 用 `git show <sha>:src/sqlite_tasks.rs` 读期望 schema；之后不要在这个仓库里 `git gc --prune` 或切走含该提交的分支 |
| 路径 | 部署根固定为 `/opt/x-data-mcp-preview` | 单元、tunnel profile、日更 Skill 的 `mcp.json` 都写了绝对路径，保持一致就一处都不用改 |
| 权限 | 全部以 root 运行；`state`、`run`、`generations`、`reports` 为 0700 | 运行时要求 socket 目录和库文件属于当前 UID 且不对外开放，否则租约拒绝启动 |

## 1. 克隆什么

来源机是仍在服务的旧机（只读访问）。克隆走 rsync，不停旧机服务，不抢来源库的写锁。

### 1.1 要克隆

| 对象 | 位置（部署根下） | 为什么必须克隆 |
|---|---|---|
| 来源库 | `state/source-store/`（`manifest.jsonl` + `blobs/`） | 交易所快照等事实**只能重放、不能重新抓取**；是唯一不可再生的数据 |
| generations | `generations/`（含 `generations/`、`pins/`、`staging/`、`active-generation.json`） | 当前和保留的 generation 字节；官方只发最新一份，旧代拿不回来 |
| `acquire.sqlite` | `generations/acquire.sqlite` | 代登记和退役状态；缺了 acquire 服务不认这些代 |
| 行业快照 | `/root/x-industry-archive/`（`blobs/`、`index.jsonl`、`archive.py`） | 行业快照字节的保底归档（D-26）；DB 里只有观测到的代，字节在这里 |
| 日更启动目录 | `/root/x-daily-sync/` | 启动脚本、提示词、工具白名单和 `.pi/mcp.json`，约 30 KB |
| 单元和 drop-in | `/etc/systemd/system/x-data-mcp-{data,acquire}.service` 及 `.service.d/` | 见 §3；也可按 `deploy_phase_g.sh` 缺单元时打印的正文重建 |
| ETF 退市公告导入包 | 仅当来源库里还没有 `etf_delist_pdf` 记录时（`grep -c etf_delist state/source-store/manifest.jsonl` 为 0） | D-72；从一个已含这些记录的来源库克隆则不需要 |

不克隆的东西同样要在列表里写清楚，见 §1.2。

### 1.2 不克隆

| 对象 | 处理 |
|---|---|
| `state/preview.sqlite*`（行情库） | 不搬。新机 fresh init（`deploy_phase_g.sh` 会做），再由重放和首采重建。上次的理由：搬运 2.2 GB 不比重建快，且 fresh init 本来就丢掉旧库。代价：库内只存在、来源库里没有的东西（任务历史等）不会带过来；要保留就另行决定克隆库（快照用 sqlite backup API 或 `VACUUM INTO`，不能直接拷 WAL 库） |
| `releases/*` | 新机从 GitHub 取目标 release，不拷旧 release |
| 仓库 | 从 GitHub clone |
| `reports/` | 新机生成自己的部署报告 |
| tunnel 的单元、二进制、profile、凭据 | **切换前不放到新机**，见 §5 |
| 旧机上的临时副本、性能测试目录、本地客户端配置 | 不碰 |

### 1.3 怎么拷、怎么校验

先建目录，再拷。`deploy_phase_g.sh` 会补建缺的目录，但 rsync 前先建好能保证权限：

```bash
install -d -m 700 /opt/x-data-mcp-preview \
  /opt/x-data-mcp-preview/{state,run,generations,reports,releases}
```

```bash
SRC=<来源机>     # 操作员提供，不写进仓库
R="nice -n 19 ionice -c3 rsync"
# 1) generations（含 acquire.sqlite）
rsync -a --partial --rsync-path="$R" $SRC:/opt/x-data-mcp-preview/generations/ /opt/x-data-mcp-preview/generations/
# 2) 来源库：跑两遍。第二遍补第一遍期间追加的 manifest 行和新 blob
for i in 1 2; do
  rsync -a --partial --rsync-path="$R" $SRC:/opt/x-data-mcp-preview/state/source-store/ /opt/x-data-mcp-preview/state/source-store/
done
# 3) 其余状态文件，排除行情库和来源库
rsync -a --partial --rsync-path="$R" --exclude 'preview.sqlite*' --exclude 'source-store/' \
  $SRC:/opt/x-data-mcp-preview/state/ /opt/x-data-mcp-preview/state/
# 4) 行业归档和日更目录
rsync -a --partial --rsync-path="$R" $SRC:/root/x-industry-archive/ /root/x-industry-archive/
rsync -a --partial --rsync-path="$R" $SRC:/root/x-daily-sync/ /root/x-daily-sync/
```

来源库是追加写、blob 先写临时文件再 rename，所以“拷两遍加事后校验”足够；不要对来源机的库加锁。拷 `acquire.sqlite` 前确认来源机
没有正在运行的 acquire 任务（服务常驻没关系，任务要为空）。

校验。**全部通过才进入 §2**；任何一项不过，重跑 rsync 再验：

| 对象 | 校验 | 期望 |
|---|---|---|
| 来源库 | 两侧 `wc -l manifest.jsonl`、`ls blobs \| wc -l`、`sha256sum manifest.jsonl` | 逐项相等 |
| 来源库 | 用目标 release 的二进制：`source_store verify --store <副本> --scope all` | `status=ok`、`problems=[]`、`entries` 等于 manifest 行数、五个 scope 全 `ok`。用比来源库新的二进制校验，旧二进制可能不认新来源 |
| generations | 两侧对每个文件算 sha256：`find . -type f -print0 \| sort -z \| xargs -0 sha256sum`，再 diff | 文件集合和每个 sha256 相同。不要只信 rsync 退出码，也不要只比目录大小 |
| `acquire.sqlite` | 副本上 `PRAGMA integrity_check`；两侧每张表行数 | `ok`；行数逐表相等 |
| 行业归档 | 两侧文件数、`sha256sum index.jsonl` | 相等 |

上次迁移（约 1.9 GB，两机之间经中继的链路约 1.6 MB/s）：克隆加校验约 36 分钟。

## 2. 目录骨架

部署根下的目录和模式（都是 0700，root）：

```text
/opt/x-data-mcp-preview/
  current -> releases/<sha>     # 由 deploy_phase_g.sh 建立；首次部署前不存在，是悬空的
  releases/                     # 每个 release 一个以完整 sha 命名的目录
  state/                        # preview.sqlite、source-store/、preview.sqlite.verification-guards/
  run/                          # mcp.sock research.sock maint.sock acquire.sock（及 *.owner.json）
  generations/                  # 见 §1.1
  reports/                      # <tag>-phaseg-deployment.json
```

`deploy_phase_g.sh` 的行为（2026-11-05 修复后）：

- 在新机上缺的目录（`releases/`、`reports/`、库所在目录、generation 根、socket 目录）会在**包校验通过之后、第一次写入之前**以 0700
  创建；`--dry-run` 只在输出里列出 `missing dirs:`，不创建。
- 已存在的目录不改模式、不改属主、不动内容，对已有部署无影响。
- 来源库目录**不会**被建：它是克隆来的数据，不是骨架。克隆漏了，replay 会报 `store_empty` 而不是装作正常，部署后必须核对（§4）。
- 路径存在但不是目录会以 `FAILED_AT_prepare_layout` 失败，此时尚未改动任何服务。

修复前的脚本在新机上第一次失败于 `FAILED_AT_install_release`（`releases/` 不存在），发生在任何变更之前，`rollback: not_needed`。
如果在旧版本脚本上部署，先手工 `install -d -m 700 /opt/x-data-mcp-preview/releases`。

`state/preview.sqlite.verification-guards/` 由运行时在需要时创建，克隆时已带过来的空目录保留即可。

## 3. systemd 单元

单元文件不放在仓库里：`deploy_phase_g.sh` 在缺单元时退出码 2，并打印所需的 `ExecStart`、`ExecStartPre` 和 tunnel drop-in 模板；
版本相关的 `ExecStart` 以它的输出为准，不要抄旧文档。要点：

- 所有 `ExecStart` 经 `current/bin/...`，脚本会拒绝不经 `current` 符号链接的单元。
- **`x-data-mcp-data`**：一个进程三个 socket（`mcp.sock` 的 `--entry ops`、`research.sock`、`maint.sock`），尾部 `--poll-ms 1000`。
- **`x-data-mcp-acquire`**：`acquire_service <generations> <acquire.sock> --market-db <state/preview.sqlite>`。

drop-in（逐字沿用来源机，第一次测量不调参）：

| 单元 | drop-in | 内容 |
|---|---|---|
| data | `exec-start.conf` | 三 socket 的 `ExecStart`（先写空的 `ExecStart=` 复位） |
| data | `generation.conf` | `X_DATA_MCP_GENERATION_ROOT=/opt/x-data-mcp-preview/generations`，并把该目录加进 `ReadWritePaths` |
| data | `source-store.conf` | `X_DATA_MCP_SOURCE_STORE=/opt/x-data-mcp-preview/state/source-store` |
| data | `memory.conf` | `MemoryHigh=384M`、`MemoryMax=640M`、`MemorySwapMax=512M` |
| data | `prepare-workers.conf` | `X_DATA_MCP_RUN_PREPARE_WORKERS=1` |
| data | `text-mode.conf` | `X_DATA_MCP_TEXT_MODE=full` |
| acquire | `market-db.conf` | 追加 `--market-db`，并把 `state` 加进 `ReadWritePaths` |
| data、acquire | **`timezone.conf`** | `[Service]` 下 `Environment=TZ=UTC` |

**`TZ=UTC` 必须加**：`src/data_supply/query.rs` 的 `today()` 用本地时钟，决定读面默认 `as_of` 的“今天”。新机时区若是
Asia/Shanghai，UTC 16:00–24:00 之间它会比来源机多一天，默认窗口落到明天、查不到事实。其余代码显式用 UTC 或上海时区。
journald 时间戳仍是主机本地时区，写日志和报告时自己用 `date -u`。把 `today()` 改成上海日是另一个代码任务，不在部署里做。

**tunnel 占位单元**：脚本在窗口里 `systemctl stop/start` tunnel 单元，单元不存在会使停机步骤失败。新机切换前不装真 tunnel，
装一个空单元，并让脚本指向它：

```ini
# /etc/systemd/system/xdm-tunnel-stub.service
[Unit]
Description=Placeholder so deploy_phase_g.sh can stop/start a tunnel unit
[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/bin/true
```

```bash
systemctl daemon-reload && systemctl start xdm-tunnel-stub.service
export XDM_TUNNEL_SERVICE=xdm-tunnel-stub.service
```

装完 `daemon-reload`。**此时不要 enable data 和 acquire**：`current` 还是悬空的，开机会让 `data_service` 起不来并每几秒重试刷屏。
部署成功（§4 第 1 步）后再 `systemctl enable`。

## 4. fresh init 之后的数据步骤和顺序

下面每一步都以 [daily-sync](daily-sync.md)「全新数据库」和「来源刷新」为准，这里只写顺序、依赖和这次的读数。
操作用日更驱动或 MCP 工具；一步没过就停下排查，不要往下做。写任务的步骤之间不要并行，`full` 期间不要跑其他任务（否则 `full` 的耗时不可归因）。

1. **部署。** `scripts/deploy_phase_g.sh <tag> --dry-run --force-git-transport` 通过后去掉 `--dry-run` 再跑。
   通过的标志：`status=DEPLOYED`，`source_store_replay.status=ok`（五个 scope；是 `store_empty` 说明克隆漏了来源库），
   `user_version` 等于该提交的 `COMPACT_SCHEMA_VERSION`，data/acquire 的 socket 探针全过。
   `before.schema=0`、`archived_db=null` 是新机的正常值。然后 `systemctl enable` data 和 acquire。
2. **ETF 退市公告（只在 §1.1 说需要时）。** 停 data、acquire 和 tunnel 单元，`source_store import --format etf-delisting`
   导入，再 `replay --scope all --db <库>`，起服务。写库的 replay 要服务停着，因为它要拿库的服务租约。
3. **`full` 刷新**，在**第一次日更之前**：`refresh_exchange_sources {"mode":"full"}`，用 `{"status_of":<run_id>}` 轮询到 `succeeded`。
   原因：fresh 库没有刷新 run 记录，只有 `full` 的成员表能固定页集身份并推进 `verified_through`；之后的主表发布才带上 ETF 名录。
   实测约 73 分钟（原有 7 来源 42.5 分钟、ETF 4 来源 30.5 分钟，按 D-73 分组计量）。`full` 当天跑过则当天不用再跑 `tail`。
4. **目录首采**：`start_collection(securities.catalog, {"scope":"current_sh_sz_a"})`。
5. **A 股日线首采**：`start_collection(market.daily)`，按批提交直到取完（上次每任务 100 项、共 53 批，约 10 分钟）。
   `sh.688827` 的那一批会是 `partially_failed`，见 §6。
6. **发布 `security_master`**：对 `market.daily` 跑一次 `plan_market_update` / `run_market_update`（带派生 item，不是 ETF-only）。
   这一步让 `derived:security_master` 有已发布版本。
7. **`sh_sz_a_listed_ever` 首采**：`start_collection(market.daily, v2, {"kind":"scope","scope":"sh_sz_a_listed_ever"})`。
   **依赖第 6 步**：fresh 库里还没有已发布的 `security_master` 时报
   `INVALID_COMMAND: security_master has no published version; the listed-ever scope cannot resolve`。
   上次的清单漏了这个前置，第一次失败后补做了第 6 步。
8. **年线**：`market.yearly` 的 plan-run（`start_collection` 不能用，年线一律走 plan-run / `update_data`）。
9. **ETF 日线首采**：`sh_sz_etf_current`（分批，上次 500/500/500/131），再 `sh_sz_etf_listed_ever`（128 个退市 ETF）。ETF 要等第 6 步
   发布的主表带 ETF 行才有成员。
10. **读回验收**：四个入口 `tools/list` 的工具数和 `get_data_capabilities` 的 `capability_revision` 与 [STATUS](STATUS.md) 一致；
    `source_store replay --scope all --verify-only` 见 §6；`task_sqlite check <库>` 通过；`PRAGMA quick_check` 为 `ok`。
11. **第一次日更前**：把仓库 `git pull` 到与部署版本匹配的提交。日更 Skill 钉着 `capability_revision`，仓库和运行时不一致会以
    `SKILL_CONTRACT_MISMATCH` 停下（fail-closed，不写坏数据）。日更 agent 的启动方式见 [daily-sync](daily-sync.md)。

上次实际顺序是第 6 步先于第 3 步（用 `full` 之前的来源库跑过一次主表发布），所以 `security_master` 先发布了 5,566 行（无 ETF），`full` 之后
又发布了 7,412 行，差值恰为 1,846 行 ETF。按上面的顺序把 `full` 提前，主表只需发布一次。

## 5. tunnel

- 新机切换前不装 tunnel 的二进制、profile、凭据和真单元，只有 §3 的占位单元。这样新旧两机同一 profile 同时在线在机器层面不可能。
  控制面如何处理同一 tunnel 的两个客户端无法在本地验证，所以按“绝不双在线”设计。
- 是否在新机装 tunnel 是 owner 的决定（D-74 当时选择先不装，客户端留在旧机）。
- 切换顺序（只有 owner 确认后执行）：
  1. 旧机：先停 tunnel，再停 acquire 和 data，并全部 `disable`（否则旧机重启会抢回生产）。文件全部保留。
  2. 新机：确认 data、acquire 在跑；从操作员的受控位置安装 tunnel 二进制、profile（命令是绝对路径
     `…/current/bin/mcp_connect …/run/research.sock`，路径一致就不用改）和凭据；装真 tunnel 单元，
     以及 `deploy_phase_g.sh` 打印的 drop-in 模板（`Wants=`/`After=` data、`ExecStartPre` 探针、`TimeoutStartSec=180`）；
     去掉占位单元，`unset XDM_TUNNEL_SERVICE`。
  3. `systemctl enable --now` tunnel；用 `scripts/wait_service_ready.py` 对三个 data socket 和 acquire socket 做探针；看 tunnel 客户端的健康文件。
- tunnel 单元有 `Restart=always`，且 `Wants=` data。停机窗口里必须先停 tunnel，否则它会在约 10 秒后把 data 拉起来。

## 6. 已知的预期读数

下面这些在上次迁移和之后的核对里都出现过，原因已查明，不要当故障处理。条件是**原因和范围都对得上**；对不上就是新问题。

| 读数 | 在哪看到 | 原因 | 什么时候不再是预期 |
|---|---|---|---|
| `sh.688827` 缺成员 | 第 5 步那一批 `partially_failed`，错误 `INVALID_MARKET_COMMAND: member absent`；plan 里 `absent=1`、`eligible` 比 catalog 少 1 | 目录里有，官方 `hsjday.zip` 里没有这个成员（2026-11-05 在新旧两代都缺） | 失败实体不止它，或错误不是 `member absent`。官方补上成员后会自己消失 |
| 86 只 ETF 没有日线 | ETF 首采后 `absent_count`；主表 `security_master` 的 ETF 行比有日线的序列多 | 沪市 526 / 551 / 589 号段在 `hsjday` 里没有成员 | 缺的代码落在这些号段之外；比 86 多出来的先看是不是当天新上市（迁移当天 `sz.158081` 就是，87 只，次日官方 generation 里有了） |
| `risk_warnings` 的 `replay --verify-only` 报 `drift` | `source_store replay --scope all --verify-only` 中仅 `risk_warnings` 为 `drift`，其余四个 scope（industry、exchange、generation_facts、security_names）为 `ok` / `match` | 同一公告在多个查询页出现时，replay 与日更派生的页顺序不同，引用了不同的快照；2026-11-05 只有 105 行沪市记录的 `source_document_sha256` 不同，日期、状态、`known_at` 都相同。旧机上也一样，不是搬迁造成的 | 其他 scope 也 drift，或差异不是这一个列。修复由代码任务处理，见 [决策记录](decisions.md) 和 [STATUS](STATUS.md) |

和旧机比对（可选）：`known_at` 的逐日分布（`security_name_history`、`risk_warning_intervals`）应与旧机一致，旧行不变，新增行只来自新快照；
`market_series` 的差异只应落在已知的 `REVISION_UNVERIFIED` 实体和新上市实体上。导出 `market_series` 做比较时 `content_digest` 是 BLOB，
转成十六进制再比，否则 JSON 序列化会失败。

## 7. 回滚

按失败发生的位置分三种：

1. **部署脚本中途失败。** 看 stderr 的 `FAILED_AT_<步骤>` 和 `rollback:` 字样，报告写在 `reports/<tag>-phaseg-deployment.json`。
   - 变更之前的失败（包校验、`prepare_layout`、`install_release`、`expected_schema`）：`rollback: not_needed`，修好直接重跑。
   - 变更之后的失败（停服务之后）：脚本按 tunnel→acquire→data 停，尝试把 `current` 指回旧值、把归档的旧库改名放回，再按
     data→acquire→tunnel 起，结果记在报告的 `rollback_outcome`。**新机没有旧 release 也没有旧库**，所以没有东西可还原：
     `current` 保持原状（首次部署前是悬空的，切换之后失败则指向新 release），失败时新建的空库留在原处。
     这时服务是否被拉起取决于失败点，以 `rollback_outcome` 和 `systemctl is-active` 为准，不要假设。
     修好原因后重跑：留下的空库会被当作旧库归档成 `preview.sqlite.v<N>-archived-<时间>`，再 fresh init；确认后手工清理这个空归档。
2. **新机上线后发现问题，要退回旧机。** 旧机的文件和库保留不动，只是不再跑日更：
   1. 回退日更 Skill 的 `capability_revision` 钉值（旧机若是更旧的版本，仓库要先回到匹配的提交，否则 `SKILL_CONTRACT_MISMATCH`）。
   2. 新机停 tunnel（如已装）、acquire、data，并 `disable`。
   3. 旧机 `enable --now` data、acquire，探针通过后再起 tunnel。
   4. 数据缺口：旧机缺新机上线以来的天数。`hsjday.zip` 是累积全量，一次完整日更即可补齐；缺口超过 14 天先 `full`，否则四个同源 item 会
      `source_refresh_overdue`。新机期间产生的来源快照留在新机的来源库里当证据，不回灌旧机。
3. **旧机也没了。** 只靠来源库备份：库可以由来源库重放加首采重建，来源库不能重建。所以：
   - **每次日更之后**把新机的来源库增量备份到另一个位置（第一次不等日更，`full` 跑完就备份），用 `--link-dest` 指向上一份做硬链接增量，
     首次没有上一份时不加 `--link-dest` 和 `--delete`。
   - 备份的校验用**新版本**二进制放到备份机上的副本：`source_store verify --store <备份> --scope all` 应 `status=ok`、`problems=[]`，
     并且两侧 `manifest.jsonl` 的 sha256 相等。旧版本二进制不认新来源（`sse_*` / `szse_*`），会误报。
   - 退役的 generation 字节不可恢复，回滚只能恢复软件和库面。

## 8. 机器到期前

试用机到期前要在 [STATUS](STATUS.md) 里定去向，备选：续期、迁到下一台（按本文件）、退回旧机。迁走前的最小动作：

1. 在到期前至少留一份通过 §7 第 3 种校验的来源库备份，以及 `generations/` 和 `acquire.sqlite`。
2. 行业归档目录再增量拉一次，在新机跑一次 `archive.py`（幂等）。行业快照字节只有这里保底；DB 里缺的天数需要另开回填任务。
3. 决定行情库是重建还是克隆（§1.2）。重建的参照耗时：上次从克隆完成到验收全部通过约 2 小时 15 分到 2 小时 40 分，
   其中 `full` 约 73 分钟。
4. 停服务前先做一次日更并备份，避免白白丢一天。
