# Issue #4 — `market.daily` 全目录首次采集验收证据

## 结论

**13/13 PASS**(`results.json`,release build,HEAD `bed5305`)。

5224 已发布 catalog 成员经 53 片 explicit selection 受理（`Catalog`
选择命中 `resolved selection budget 1..100`，如实记 BLOCKED_ON_IMPL);
5219 成功发布 + 恰 5 只无 `.day` member 证券逐 item 失败
（`member absent or ambiguous` verbatim，不阻塞）;
16,629,206 daily_bars;D9 幂等重采全量 verbatim
`existing daily head requires the update_data range path`;
env-absent 腿与三门禁 verbatim 原样。

## 运行历史（如实记录）

| 运行 | 代码 | 构建 | 结果 |
|---|---|---|---|
| v1 debug | `2b7a0c4` | debug | 中止于 3289/5224(~16.2h)——运行测出发布路径 O(N²) 瓶颈，见 #204 方案文档 |
| v2a release | `bed5305`(#204) | release | 12/13:D8 断言失败为**脚本比较方法问题**(pin 字符串 vs 投影 TEXT 小数归一化 `7.70`/`7.7`);compat_json 里 `"7.70"` verbatim 与 pin 一致，数值 replay PASS |
| v2b release | `bed5305`(#204) | release | **13/13 PASS**——D8 改数值比较后由 harness 重新生成 |

## 文件

- `PLAN.md` — freeze plan(§8 为 v2 修订：中止记录 + release 说明）
- `run_daily5230_acceptance.py` — 验收驱动（v2b 版，D8 数值比较）
- `results.json` — v2b 断言结果（13/13)+ run_identity + blocked_paths + summary
- `run-log.jsonl` — v2b verbatim 运行日志
- `results-release-v2a.json` / `run-log-release-v2a.jsonl` — v2a 证据（12/13 + D8 工件说明）
- `run-log-aborted-debug.jsonl` — v1 中止运行日志
- scratch:`/home/box/acceptance-run-issue4/`(v2b `db.sqlite` + 保留的 v1/v2a 库 + `ABORTED.md`)

## 门禁状态

不变：`GBBQ_QFQ_NOT_VERIFIED`、`FILE_ACCESS_USE_NOT_VERIFIED`、
`FILE_SET_CONSISTENCY_NOT_VERIFIED`、`NOT_PRODUCTION_READY`。
本验收证明首采链端到端可行，不构成生产就绪声明。
