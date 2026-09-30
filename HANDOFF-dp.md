# DP Handoff — perf/320-typed-crossgen-append

## 状态

- 分支：`perf/320-typed-crossgen-append`
- PR：#321，`https://github.com/liqiangcc/x-data-mcp/pull/321`，已 open；本收尾只 commit，不 push/merge/deploy。
- 最新已推送提交：`96b26fee2b87ef89ecfc549d5cb6f6d85f42f34f`
- 本地即将产生的 WIP 提交：只包含本 handoff 文件。
- preview.29 未部署、未重置、未修改。

## 已完成事实

1. #320 typed cross-generation append 已实现：
   - `TypedVersionMetadata.generation_id` 来自 `TdxRangeProvider.generation_id` 的 task/session-bound generation id。
   - `generation_manifest_sha256` 仍是 inspected source manifest digest。
   - compact repo 不再断言 `generation_id == generation_manifest_sha256`。
2. `all_tail_in_range=false` 独立 parity 测试已添加并通过：
   - typed metadata staging 与 envelope merge candidate byte-identical。
3. 审核要求回归已覆盖并通过：
   - unit drift；
   - non-Complete coverage；
   - `.day` member 非前缀；
   - security-event digest drift；
   - 单日 tail 超出 accepted range；
   - unsupported case 不进入 metadata override。
4. `ops_delta_draft_fields` 的 per-item base binding/evidence read 未优化，只测量。
5. Retained-generation release harness 已测：
   - code SHA：`6f9f0a4c73b2f1cc987d1c6e6aeb4a1535bf8acd`
   - initial：5,222 entities / 16,676,083 bars / 151.51s
   - cross-generation update：5,222 items / 53 tasks / 96.36s / 18.45ms per item
   - typed metadata append：5,165
   - typed no-change：12
   - envelope fallback：45
   - fallback published：21
   - fail-closed：24，均为 `NEEDS_VERIFICATION: RevisionUnverified`
   - integrity check：`ok`
6. 分阶段 profile 已测并作为 PR comment 提交：
   - code SHA：`499ccbcf3f6420642f875136359034689638eeb6`
   - initial wall：156.55s
   - same-generation no-change wall：63.93s
   - cross-generation append wall：93.52s
   - perf sampled run：129.45s，observer overhead 1.384× unprofiled 93.52s
   - top sampled self-time：libc `malloc` 5.85%，sha256 compress 4.56%，inflate 3.14%，`sqlite3VdbeExec` 2.74%，`cfree` 2.43%。
7. 文档 README/AGENTS/docs/daily-sync 已用 bounded/conditional wording 记录离线 harness 结果；未写成 preview.29 deployed timing 或 production SLO。

## 未完成项

- PR #321 尚未 merge；等待审核。
- Hosted GitHub `rust` check 在 job 启动阶段失败且无日志，症状与此前 billing/spending-limit 相同；未在本机复现代码失败。
- 未做 deployed preview.29 runtime measurement。
- 未做 drop-caches cold-start measurement。
- 未做任何 profile-driven optimization。
- 未验证生产 SLO、生产 readiness、外部分发授权。
- `HANDOFF-dp.md` 提交后仍需新的 Lead/Worker 决定是否 push 该 WIP commit。

## 已知风险/边界

- 根盘接近满：收尾前 `df -h /` 显示约 518MiB 可用。
- `/root/x-data-mcp-issue320-profile/` 含独立 harness DB/baseline/result/perf 数据，占用磁盘；如需清理由后续 owner 决定。
- 45 个 fallback 中 24 个 item 是 `RevisionUnverified` fail-closed，不是 metadata override 成功。
- perf 采样只说明 sampled self-time 分布；不能单独推导优化优先级。
- capacity/profile harness 是离线独立库，不是 MCP service 或 preview.29 live path。

## 已跑命令与退出码

- `cargo fmt --check`：exit 0
- `cargo clippy --release --all-targets --all-features --locked -- -D warnings`：exit 0
- `cargo test -j1 --release --locked`：exit 0；718 passed / 10 ignored / 0 failed
- `sqlite_range_update::two_fifty_day_append_growth_stays_bounded`：exit 0；release 222.11s
- `cargo check --release --all-targets --locked`：exit 0
- capacity initial/same-generation/cross-generation runs：exit 0
- `perf record -F 99`：exit 0，6,203 samples
- `perf report --stdio --no-children`：exit 0
- `python3 -m json.tool` on profile evidence：exit 0
- `python3 scripts/check_current_facts.py`：exit 0（PR 前状态）
- `python3 scripts/check_daily_sync_eval.py`：exit 0（PR 前状态）
- `git diff --check`：exit 0

## 证据文件

- `evidence/issue-320-typed-crossgen-append/result.json`
- `evidence/issue-320-typed-crossgen-append/capacity-20260930-crossgen.json`
- `evidence/issue-320-typed-crossgen-append/profile-20260930.json`
- `evidence/issue-320-typed-crossgen-append/perf-report.txt`

## 当前收尾动作

- 不新开测试、profile 或优化。
- 仅提交本 handoff 为 WIP commit。
- 提交后在终端输出 `DP-HANDOFF-DONE <commit sha>` 并停止。
