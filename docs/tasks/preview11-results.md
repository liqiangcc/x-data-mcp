# preview.11 部署结果（#140 流水线首次实操）

2026-09-14，preview.11 首次通过 #140 新增的 tag 触发 Release workflow 与
`scripts/deploy_preview.sh --force-git-transport` 完成发布、git 传输取包、
切换、回滚验证和重新部署。该记录不是生产来源批准，也不是完整目标完成声明。

## 代码、tag 与 Release

- 发布代码为 release-label 合并主线 `feca5e2448dc8bd7da7d8a8e530d239fc358712a`；
  lightweight tag `v0.2.0-preview.11` 指向同一 SHA。
- tag push 触发 `.github/workflows/release.yml` run `34833485806` / job
  `103941958140`（SUCCESS）：版本==tag、tag 在 main、包重建、
  `check.py`/`check_peer.py` smoke、manifest `git_sha`==tag SHA 全部验证后，
  `gh release create --prerelease --target feca5e2`。
- GitHub Release `v0.2.0-preview.11`：`targetCommitish`=`feca5e2`，prerelease，
  三项资产齐全；tarball SHA-256
  `2a69420610ae644cac6c1683434237d3b5dab118072b92db222283419d4db4d4`
  与 `.sha256` 资产一致。
- 同一 workflow 将验证过的包推到 `refs/dist/v0.2.0-preview.11`
  （commit `89f636d2bdd062b7694c1fdb4eccea3a557a0982`）。临时 bare repo
  `git archive` 取出三项资产 + `dist-manifest.json`，其
  `{tag, tag_sha, tarball_sha256, version}` 与上述值精确一致。

## AC-02 负向证据

临时 lightweight tag `v0.2.0-preview.11-ac02-mismatch`（同样指向 `feca5e2`）
推送后 Release run `34833318543` / job `103941427301` 在
"Verify tag binds to a labelled main commit" 步骤失败（tag 名 !=
`v`+Cargo 版本），未创建 release、未产生 dist ref；临时 tag 已删除。

## 部署与回滚（git transport）

部署在 `/opt/x-data-mcp-preview` 使用已合并 main 的 `scripts/deploy_preview.sh`，
`--force-git-transport` 强制走 `refs/dist` git 取包（对象 CDN 未使用）。

- 第一次部署：部署前 `current`=preview.10/`22c2255`、schema10、全部业务表与
  `task_item_execution_starts` 均为 0 行。脚本完成一致 sqlite3 备份
  `state/backup-v0.2.0-preview.11-20260914T103904Z.sqlite`、`task_sqlite`
  schema 检查（10→10 不变）、原子 `current` 切换、data/tunnel 重启、
  package smoke、live stdio、tunnel cgroup 断言；
  `preview11-deployment.json`=DEPLOYED，
  `preview11-{smoke,stdio,tunnel-observation,final-verification}.json`=PASS。
- 回滚验证：`rollback_preview.sh --from-report=preview11-deployment.json`
  恢复到 preview.10，rollback 报告 PASS（integrity、current 精确指向旧
  release、恢复 release 自带 schema 检查接受、双服务 active、tunnel cgroup
  `mcp_connect` SHA=preview.10 包）；`current` 回到 `22c2255`。
- 第二次部署：再次 `--force-git-transport` 完整成功，最终状态为 preview.11。
  第二次备份 `state/backup-v0.2.0-preview.11-20260914T104130Z.sqlite`
  （SHA-256 `5b8ff3dc29e6b7c41b985eea3c2cea0c361a13ed9af0e773582feb8088acf03a`）。

## 运行时验收（最终状态）

- `current` → `releases/feca5e2448dc8bd7da7d8a8e530d239fc358712a`；
  `mcp_data --version` 输出 `x-data-mcp 0.2.0-preview.11 feca5e2…
  NOT_PRODUCTION_READY`。
- 普通库 schema10、`integrity_check` ok、业务表与 execution-start marker 均 0 行；
  配置与 demo 未变；未导入行情。
- `preview11-stdio.json`：实际 `mcp_connect` initialize、公开工具恰为既有
  18 个、无 default-selector 工具、`production_ready=false`、同一 stdin
  33 秒空闲后复调 PASS。
- `preview11-tunnel-observation.json`：data/tunnel `ActiveState=active`；
  `data_service` 与 tunnel cgroup 内 `mcp_connect` 的 ELF SHA 均等于
  preview.11 包内二进制；healthz/readyz/metrics PASS。
- `preview11-smoke.json`（Release 资产副本）与
  `preview11-final-verification.json` PASS，tools_count=18，
  consumer/live_source `NOT_RUN`。

## 未验证与阻塞

`#54` GBBQ/QFQ 语义、`#6/#46` 目录与行业覆盖、`#71` 获取/使用边界、跨文件
一致性以及 LOCAL_PIPELINE 仍未解决；系统继续 `NOT_PRODUCTION_READY`。
preview.11 只证明发布/部署/回滚流水线本身，不代表官方文件→daily/yearly
→query 闭环完成。
