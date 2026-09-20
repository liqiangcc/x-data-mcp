# Issue #225 — 真实数据验证：派生 completion 端到端

裁定（[#225 FG](https://github.com/liqiangcc/x-data-mcp/issues/225)）：D1=A 基线相对完整性、
D2=C 读取时推导、D3 无新签发者。实现 PR #256：`observations()` 从版本
`integrity_sha256` 绑定的 `local_yearly_derivation_v1.evidence` + `yearly_bars.is_finished`
机械推导 per-year completion。

本包记录 preview 库上的真实数据端到端验证。

## 输入

- 库：`/opt/x-data-mcp-preview/state/preview.sqlite`（schema v14）
- 年度版本：`yearly-version-5e114fd1d626346307f6f87cfe9a2aacb64142e67520bd229a53598b1c987ca6`
  - `sh.600036`，2020–2026 共 7 根年 bar
  - `local_yearly_derivation_v1`：coverage=Complete，7 年全部 certified，
    绑定 base daily `range-version-c46fd9d7…` + `base_integrity_sha256` 06cc…
  - 2026 为未完结年（`is_finished=false`，as_of 2026-09-20）
- 发布模式 `historical_fixed_base`：**不推进** effective year head（`effective_year_head_versions: []`），
  必须用显式 `version_ref` 查询——这是设计语义，非数据丢失。

## 验证

`compute_metrics`（inline，consecutive_direction/min_periods=1，range 2020–2026，
输入为显式 `version_ref` + `integrity_sha256` 04ce…）：

| 断言 | 结果 |
|---|---|
| outcome | `computed`（completion 由嵌入推导证据机械派生，非 fixture 路径） |
| `excluded_tail` | `[2026]` — 未完结年在 `completed_only` 下正确排除 |
| `observations_used` | 4（2022–2025 连续已完结年） |
| `continuity_evidence` | "adjacent natural years and explicit completion checked" |
| 完整性绑定 | 输入 `integrity_sha256` 与版本 `content_digest` 一致，校验通过 |
| 写副作用 | 无（只读路径，版本不可变） |

完整 wire 输出见 `result.json`。

## 复现

preview 服务运行后，经 `run/mcp.sock` 调用 `compute_metrics`，
`inputs[].version_ref` 填 `result.json` 中的 `yearly_version` 字段值。

## 边界

- 完整性为**基线相对**（相对固定 daily 版本的重聚合核验），不是自然年/上市史声明
- `production_ready` 仍为 false；本证据只覆盖 #225 的派生路径端到端正确性
