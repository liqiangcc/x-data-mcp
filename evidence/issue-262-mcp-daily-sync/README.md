# Issue #262 — 100-item MCP functional smoke (Phase G evidence)

Real-source, 100-item functional run on ECS (`ecs-node`, Ubuntu 24.04,
2 vCPU / ~2 GiB / 40 GiB). Acquisition and data workflow calls used
`acquire.sock` + `mcp.sock` via `mcp_connect`; no shell/SSH/SQLite was used to
decide or perform those business actions. The later service-restart recovery
probe was an operator action using `systemctl`, not part of the MCP-only Skill
workflow.

This evidence proves a bounded functional slice only. It is not full-market
evidence and does not satisfy #259 capacity or #262 production DoD. Current
authoritative corrections:

- [#259 physical audit](https://github.com/liqiangcc/x-data-mcp/issues/259#issuecomment-5756919342)
- [#262 capacity status](https://github.com/liqiangcc/x-data-mcp/issues/262#issuecomment-5756920960)

## Commits under test

| artifact | value |
|---|---|
| release commit | `bfa61ec6c3d3b22864fc9b0354152e9107793134` (data legs at `3be3f1d`, yearly leg at `bfa61ec` — yearly binding fix) |
| package tarball SHA-256 | `02e09926f210b540422760cbd7e18d7e9189dcfc2942f24bbf6dd8ac235cfa5c` (local `dist/` == ECS `/tmp/`) |
| deployed `data_service` | `a0289ca239db1bd6f5204d70a3c51d1e93eb42d790feac129702d7b4dfc43031` == `SHA256SUMS` |
| deployed `acquire_service` | `fcba744336c00ded3296763a965a6c8d4e5c64df8c09d11ce4ca9b67357c766a` == `SHA256SUMS` |
| DB schema | 18 |
| capability revision | `generic-data-v2` |

## Functional sequence and expected refusal

1. `check_official_updates` — three official sources reachable, no state created.
2. `acquire_generation` → generation `868f4fa1…5d` (547 MB immutable store) — ~570 s download.
3. `verify_generation` → `consistency_verified`.
4. `activate_generation` → `active` (`active-generation.json` above).
5. `start_collection` catalog scope `current_sh_sz_a` → catalog `catalog-1789971581105859146` (10.0 s).
6. A repeated `start_collection` against 100 entities that already had daily
   heads failed 100/100 as required; existing heads must use `update_data`.
7. `update_data` market.daily, explicit 100-item subset → **100/100 succeeded in 165.3 s**
   (≤3 min target at 100-item scale; full-market breadth tracked separately —
   `MAX_DEFAULT_SECURITIES` caps any single task at 100 items).
8. `get_data_availability` sh.600000 → integrity `verified`, records 1629,
   2020-01-02 → 2026-09-18.
9. `update_data` market.yearly (3 entities, `current_head` intent) →
   **3/3 succeeded in 16.7 s**; `query_data` returns the derived yearly record.
10. Operator-only `systemctl restart` of both services → `sync_status` still reports generation
   `868f4fa1` active; task state intact (`succeeded`); capabilities unchanged.

Reports: `phase-g-data.json`, `phase-g-yearly-restart.json`.

## Expected semantic refusal (not a defect)

`first_collection` on entities with existing daily heads failed with
`INVALID_MARKET_COMMAND: existing daily head requires the update_data range
path` — the contract requires `update_data` for existing heads. Recorded as
expected fail-closed behavior.

## Observed footprint (not #259 capacity PASS)

| metric | value | Scope |
|---|---|---|
| `/opt/x-data-mcp-preview` total | 1.1 GiB | 100-item functional deployment only |
| disk free on `/` | 5.9 GiB | Observed after bounded run |
| `preview.sqlite` | 200 MB | — |
| `preview.sqlite-wal` | 0 B (checkpointed) | — |
| `generations/` (acquire store) | 547 MB | — |
| `acquire.sqlite` | 52 KB | — |

Subsequent `dbstat` audit of this database found `segment_bars=104.38MiB` and
`payloads=81.14MiB`. All 100 daily update results were `no_change=1`, but audit
`segment.bars` references still retained about 84.9MB of full-history payloads.
Therefore neither the first-collection ≤2.5GiB AC nor the daily ≤8/16MiB AC is
met. The deployment-directory and free-disk observations above must not be
used as substitutes for those AC.

## 250-day micro-soak (local, earlier evidence)

`tests/sqlite_range_update.rs::two_fifty_day_append_growth_stays_bounded` —
8 fixture symbols by default, one multi-item update task/day through
`prepare_range`/`commit_range`:
early mean 74.8 KB/day, late-20-day mean 76.2 KB/day, ratio ≈1.02 (≤1.20 gate).
This proves bounded growth-rate shape for that fixture. It does not exercise
the production provider/full-history no-change path, 5,221 symbols, update-time
WAL, or the absolute 8/16MiB daily limit.

## Scope notes

- 100-item daily update is a subset timing, not full-market evidence.
- `production_ready` remains `false`; runtime capability reports the structured
  blocker `declared_scope_non_production` (plus source/generation blockers when
  applicable). The prose reference to #225 is not a substitute for those
  machine-readable reasons.
- `market.yearly` accepts explicit items only (no default selection).
