# preview.12 本机预览部署结果（local-adapted)

2026-09-18,preview.12 在本机（Box 容器，无 systemd/tunnel）完成发布包
获取、校验、部署与 smoke。部署形态为**本地适配**：`data_service` 以普通
后台进程运行于 unix socket,DB/socket 置于 `/dev/shm`(tmpfs)——runtime
guard 对 overlayfs(`/home`)fail-closed，与测试约束同一机制。Tunnel 明确
不在本次范围（用户裁定）。该记录不是生产来源批准。

## 代码、tag 与 Release

- 发布代码为 release-label 合并主线 `18a4d42017cf30aa299ef980239eccbac8e3778e`;
  lightweight tag `v0.2.0-preview.12` 指向同一 SHA。
- release.yml run `35320184867`(SUCCESS,3m37s)：版本==tag、tag 在 main、
  包重建、`check.py`/`check_peer.py` smoke、manifest `git_sha`==tag SHA 后
  `gh release create --prerelease --target 18a4d42`。
- Release 资产齐全：tarball + `.sha256` + `smoke-report.json`;
  `refs/dist/v0.2.0-preview.12` = `1d643a46`。
- 本机 `gh release download` 正常（对象 CDN 从本机健康）。

## 校验链（全部 PASS)

- tarball `sha256sum -c` OK;`f07bb05185e80c9a98a30a7a538ebb88ee48e58b60d447bb37cf8d042cd0d98c`;
- 包内 `SHA256SUMS` 全部 OK;
- `bin/mcp_data --version` 内嵌 `18a4d42` + `0.2.0-preview.12`;
- release `smoke-report.json` = PASS 且绑定同一 SHA/version。

## 部署与 smoke

- `releases/18a4d42/` 安装；`task_sqlite init` → `task schema 12 ready`（新库，
  无升级路径）;`current` symlink 切换；`data_service` 直接起进程。
- runtime guard 实测：`/home`(overlayfs）拒起 → DB+socket 迁 `/dev/shm`
  (tmpfs,700 私有目录）后正常——guard 行为与契约一致。
- stdio smoke（`reports/preview12local-stdio.json`)**PASS**:initialize +
  tools/list + capabilities + 3 数据集空读 + catalog 查询 + list_tasks +
  **33s idle 回归**（同 stdin 保持）+ `production_ready=false`。
- 部署后库计数：tasks/requests/data_versions/catalog_versions/scope_occupancy
  全 0(schema 12,bounded 空库形态）。

## 工具面漂移（部署切片发现的真实问题）

preview.12 暴露 **19** 个工具，比 preview.11 多 `verify_data_gap`(#187 gap
验证契约）。服务器 `deploy_preview.sh` 硬编码 `EXPECTED_TOOLS`=18 个，
原样部署会 fail-closed 在 stdio smoke。**已由 #211 修复合并**(`77308aa`)。

## 报告工件

`evidence/preview12-local-deploy/`:

| 文件 | 内容 |
|---|---|
| `preview12local-deployment.json` | 获取/校验/安装/schema/服务 全链 PASS + 适配项 verbatim |
| `preview12local-stdio.json` | 19 tools + idle 回归 + 空读 + 二进制 sha256 + 库计数 |
| `preview12local-final-verification.json` | 逐环节 PASS/NOT_RUN 汇总 |
| `smoke-report.json` | release job 打包级 smoke（引用，非本地产出） |

NOT_RUN:tunnel-observation（不在范围）、rollback（无旧版可回）、
consumer、live_source。`production`=`NOT_PRODUCTION_READY`。

## 与服务器部署的关系

本部署验证了"发布包可安装、可起服务、查询面正确、门禁原样"——与服务器
`deploy_preview.sh` 断言面等价的部分全部通过；systemd/tunnel/rollback/
备份恢复四环节为服务器特有，本机如实 NOT_RUN。服务器部署（若需要）走
同一 tag，且 #211 已排除工具面失配。
