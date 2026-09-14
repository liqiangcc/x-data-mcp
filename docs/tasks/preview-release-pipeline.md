# 预览发布链路：把大文件搬运从服务器移到 Actions

关联 Issue：[#140](https://github.com/liqiangcc/x-data-mcp/issues/140)。

状态：方案与验收标准，供执行者按 Issue 迭代。不改变来源门禁、NOT_PRODUCTION_READY 状态或任何 MCP 契约。

## 观察到的问题（2026-09-14，preview.10）

preview.3 → preview.10 两天内发布 8 次，每次链路相同：

1. label PR（`Cargo.toml`/`Cargo.lock` 两行 bump）合入 main，得到 exact SHA；
2. 等 main 的 `CI` 与 `Preview package` 成功；
3. **服务器下载** package artifact（13 MB zip）；
4. 服务器打 lightweight tag，`gh release create` 并 **服务器上传** 同一 13 MB tarball；
5. 服务器再 **下载** release asset，解包到 `/opt/x-data-mcp-preview/releases/<sha>/`；
6. 备份普通库与配置、必要时 `task_sqlite upgrade`、`ln -sfn` 切 `current`、重启 service；
7. 手工拼命令生成 `reports/previewN-{deployment,stdio,tunnel-observation,smoke,final-verification}.json`；
8. 写 `docs/tasks/previewN-results.md` 并开 PR，Issue comment。

今天服务器到 GitHub 对象 CDN 的吞吐退化到 ~10 KB/s（artifact zip 下载 ~20 分钟；release tarball 上传卡住数分钟仍未完成），而同一时刻：

| 端点 | 实测 | 说明 |
|---|---|---|
| `speed.cloudflare.com` | ~1.3 MB/s | 出口带宽正常 |
| `api.github.com` JSON | ~150 KB/s | REST 正常 |
| `git clone --depth 1 github.com/...` | 2.8 MB / 5 s | git smart-HTTP 正常 |
| `objects.githubusercontent.com`（release asset / artifact zip / codeload） | 10 KB/s ~ 35 B/s | 对象 CDN 受限 |
| `uploads.github.com`（release asset 上传） | 长时间无进展 | 同类受限 |

因此瓶颈不是 GitHub 也不是服务器整体网络，而是**服务器与 GitHub 对象存储 CDN 之间**的路径。现有链路让同一个 13 MB 包在这条路径上走三趟（下载、上传、再下载）。此外服务器磁盘 93%（剩 2.7 GB）、2 GB 内存，本机 `cargo build --release` 不是可选替代。

步骤 6–8 每次都由执行者 source-first 从上一版 results 文档重新推导命令；这是重复劳动，也是最容易在细节（备份命名、schema 升级顺序、report 字段）上漂移的地方。

## 目标

- 服务器不再承担任何 release asset 的上传，且对象 CDN 下载至多一次、可替换为 git 传输。
- tag → release 的绑定（tag SHA = 构建 SHA = manifest `git_sha` = release `target_commitish`）由 Actions 机器校验，不靠人工核对。
- 部署与 5 份 report 由仓库内脚本产出，执行者只提供 tag；脚本 fail-closed，任何前置不满足即停止且不改 `/opt`。
- 不引入新的托管服务、账号或密钥。

## 方案

### A. Release 由 Actions 创建（去掉服务器上传）

在 `.github/workflows/preview-release.yml` 增加 tag 触发的 `release` job（或新增 `release.yml`）：

- `on.push.tags: ['v*']`，`permissions.contents: write`（仅此 job）。
- checkout tag → 校验：`Cargo.toml` version == tag 去掉 `v` 前缀；tag 所指 commit 必须是 `origin/main` 的祖先（`git merge-base --is-ancestor`），否则 fail。
- 复用 `scripts/build_release.sh` 与现有 `check.py`/`check_peer.py` smoke；产物 `dist/*.tar.gz`、`.sha256`、`smoke-report.json`。
- `gh release create "$GITHUB_REF_NAME" --prerelease --target "$GITHUB_SHA" --title "x-data-mcp <version>" dist/*`；已存在则 `gh release upload --clobber`。
- 在 job summary 打印 tarball SHA-256、manifest `git_sha`、tag SHA 三者，并断言相等。

沿用 lightweight tag 惯例；tag 由 Coordinator 或执行者在 label PR 合入后推到 exact main SHA。推 tag 只有几百字节，不受 CDN 影响。

保留 `Preview package` 的 PR/push artifact 作为门禁证据；release 资产以 tag job 的产物为准，两者都绑定同一 SHA。若担心两次构建字节不同，release job 可改为从同 SHA 的 main artifact 直接搬运（`actions/download-artifact` 跨 workflow 需 `run-id`），但 Actions 与对象存储之间不受本服务器网络影响，重建更简单；manifest `git_sha` 与 smoke 结果是绑定依据，不是字节相等。

### B. 服务器获取 release 的旁路：git 传输

对象 CDN 受限时，git smart-HTTP 仍快。release job 在创建 GitHub Release 后，额外把 `dist/` 推到一个专用分发仓库或本仓库的独立 ref：

- 首选：新建仓库 `liqiangcc/x-data-mcp-dist`（仅存二进制包，按 `refs/tags/<tag>` 一个 orphan commit 一版），避免主仓库历史膨胀。需要一个仅对该仓库 `contents: write` 的 fine-grained token 作为 Actions secret；这是**新增密钥**，由用户决定是否接受。
- 备选（零新密钥）：推到本仓库 `refs/dist/<tag>`（非 branch、非 tag 的 ref），`GITHUB_TOKEN` 即可写。服务器 `git fetch origin refs/dist/<tag>:refs/dist/<tag> --depth 1` 后 `git archive`/checkout 取包。部署完成后由脚本 `git push origin :refs/dist/<tag>` 删除，不进入 clone 默认 refspec，不影响 main 历史大小；但对象仍在远端仓库中直到 GC，属于可接受的临时成本。

服务器侧脚本先尝试 `gh release download`，若 60 s 内速率低于阈值则回落到 git 旁路；两条路径拿到的 tarball 都必须与 release 的 `.sha256` 资产及 Actions job summary 中的 SHA-256 一致，否则 fail-closed。

### C. 部署脚本 `scripts/deploy_preview.sh <tag>`

把 preview7/8/9 results 文档与 `/opt/x-data-mcp-preview/reports/preview9-*.json` 中已验证的步骤固化为脚本，全部 `set -euo pipefail`，只读前置检查失败则不触碰 `/opt`：

1. 解析 tag → release JSON（`gh release view --json`），取 `targetCommitish` 与资产摘要；
2. 获取 tarball（B 中两条路径），`sha256sum -c`，解包到 `releases/<sha>/`，包内 `sha256sum --check SHA256SUMS`，`bin/mcp_data --version` 必须包含 `<sha>`；
3. 记录 before：`readlink current`、`PRAGMA user_version`、`schema_migrations`、`tasks/requests/data_versions/catalog_versions/scope_occupancy` 计数、service/tunnel `ActiveState`/`NRestarts`；
4. 备份 `state/preview.sqlite{,-shm,-wal}` → `state/backup-<tag>-<UTC>.sqlite*` 并记 SHA-256；配置快照到 `state/config-before-<tag>-<UTC>/`；
5. `systemctl stop x-data-mcp-data.service`；若新包 schema 目标 > 当前 `user_version`，用**新包**的 `bin/task_sqlite upgrade` 升级，随后校验 `user_version`、migration checksum、新表/触发器存在、执行标记行数（对既有库应为 0）；任一不符即恢复备份、不切 `current`、退出非零；
6. `ln -sfn releases/<sha> current`，`systemctl start x-data-mcp-data.service`，等待 tunnel `ExecStartPre` 就绪；
7. smoke：包级 `check.py`（隔离合成库）、live `mcp_connect run/mcp.sock`（initialize、tools/list 数量、>30 s idle 后再次调用）、tunnel observation（NRestarts、cgroup 内 ELF 摘要 == 新包、healthz/readyz/控制面轮询）；
8. 输出 `reports/<tag>-{deployment,stdio,tunnel-observation,smoke,final-verification}.json`，字段与 preview9 同名；`final-verification.status` 只有全部 PASS 才为 `DEPLOYED`，否则如实为 `FAILED_AT_<step>`；`consumer`/`live_source` 固定 `NOT_RUN`。

回滚：`scripts/rollback_preview.sh <previous-sha>`，只做 `ln -sfn` 回旧 release + 从备份恢复库 + 重启，并写 `reports/<tag>-rollback.json`。

脚本用 bash + python3 标准库（服务器无 `sqlite3` CLI，用 `python3 -c "import sqlite3"` 只读连接 `mode=ro`），不引入新依赖。

### D. 文档与证据

- `docs/tasks/previewN-results.md` 保留人工撰写，但"代码、Actions 与包"和"实际切换与验收"两节直接引用脚本产出的 JSON 字段，不再手抄摘要。
- README「预发布包与本机检查」增加一段：发布由 tag 触发，服务器部署命令为 `scripts/deploy_preview.sh <tag>`；仍标注 NOT_PRODUCTION_READY。
- 首次使用新链路发布（preview.11）时，在 Issue 中同时记录旧链路 preview.10 的耗时/卡点作为对照证据。

## 验收标准

- AC-01：推送 `v0.2.0-preview.N` tag 后，Actions 自动创建带三项资产的 prerelease，release `target_commitish` == tag SHA == manifest `git_sha`；服务器全程零上传。
- AC-02：tag 版本与 `Cargo.toml` 不一致、或 tag 不在 main 上时，release job fail，不创建 release。
- AC-03：`deploy_preview.sh` 在 release 资产缺失、SHA 不符、schema 升级校验失败、service 未 active 任一情况下退出非零，`current` 与普通库保持原样，并留下 `FAILED_AT_*` report。
- AC-04：对象 CDN 受限时（可用 `--force-git-transport` 模拟），部署仍可完成，且 tarball SHA-256 与 release 资产一致。
- AC-05：一次完整部署产出 5 份 report 且字段集合 ⊇ preview9 对应文件；`results.md` 引用而非重复这些字段。
- AC-06：不新增 MCP 工具、不改 schema、不触碰来源门禁；CI 现有四项验证入口全部通过。

## 执行顺序（建议）

1. 先合并 A（workflow）并用一个不部署的 dry tag（例如在 fork 或 `v0.2.0-preview.10-rc.0` 后立即删除）验证 AC-01/02；若不希望产生临时 tag，则只在 PR 中用 `workflow_dispatch` 输入 tag 名跑到 release create 之前一步。
2. 再做 C（脚本）：先用 `--dry-run` 只输出计划与 before 状态，与 preview9 report 逐字段对照，再正式用于 preview.11。
3. B 的备选（`refs/dist/*`）零密钥可先做；首选（独立 dist 仓库）等用户决定。
4. 最后 D。

## 不做

- 不在服务器本机做 release 构建（磁盘/内存不足，且失去 Actions 构建证据）。
- 不用 PR 分支 artifact 替代 main/tag 产物。
- 不把 report 或 results 文档改为自动 commit 回 main（避免为证据再触发发布 workflow）。
- 不为绕过 CDN 引入第三方镜像或代理。
