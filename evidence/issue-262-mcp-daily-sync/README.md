# Issue #262 — MCP-only daily market-data sync (Phase G evidence)

Real-source, MCP-only end-to-end run on ECS (`ecs-node`, Ubuntu 24.04,
2 vCPU / ~2 GiB / 40 GiB). No shell/SSH/SQLite used in the workflow path —
all steps went through `acquire.sock` + `mcp.sock` via `mcp_connect`.

## Commits under test

| artifact | value |
|---|---|
| release commit | `bfa61ec6c3d3b22864fc9b0354152e9107793134` (data legs at `3be3f1d`, yearly leg at `bfa61ec` — yearly binding fix) |
| package tarball SHA-256 | `02e09926f210b540422760cbd7e18d7e9189dcfc2942f24bbf6dd8ac235cfa5c` (local `dist/` == ECS `/tmp/`) |
| deployed `data_service` | `a0289ca239db1bd6f5204d70a3c51d1e93eb42d790feac129702d7b4dfc43031` == `SHA256SUMS` |
| deployed `acquire_service` | `fcba744336c00ded3296763a965a6c8d4e5c64df8c09d11ce4ca9b67357c766a` == `SHA256SUMS` |
| DB schema | 18 |
| capability revision | `generic-data-v2` |

## MCP-only sequence (all green)

1. `check_official_updates` — three official sources reachable, no state created.
2. `acquire_generation` → generation `868f4fa1…5d` (547 MB immutable store) — ~570 s download.
3. `verify_generation` → `consistency_verified`.
4. `activate_generation` → `active` (`active-generation.json` above).
5. `start_collection` catalog scope `current_sh_sz_a` → catalog `catalog-1789971581105859146` (10.0 s).
6. `update_data` market.daily, explicit 100-item subset → **100/100 succeeded in 165.3 s**
   (≤3 min target at 100-item scale; full-market breadth tracked separately —
   `MAX_DEFAULT_SECURITIES` caps any single task at 100 items).
7. `get_data_availability` sh.600000 → integrity `verified`, records 1629,
   2020-01-02 → 2026-09-18.
8. `update_data` market.yearly (3 entities, `current_head` intent) →
   **3/3 succeeded in 16.7 s**; `query_data` returns the derived yearly record.
9. `systemctl restart` both services → `sync_status` still reports generation
   `868f4fa1` active; task state intact (`succeeded`); capabilities unchanged.

Reports: `phase-g-data.json`, `phase-g-yearly-restart.json`.

## Expected semantic refusal (not a defect)

`first_collection` on entities with existing daily heads failed with
`INVALID_MARKET_COMMAND: existing daily head requires the update_data range
path` — the contract requires `update_data` for existing heads. Recorded as
expected fail-closed behavior.

## Capacity / storage

| metric | value | DoD gate |
|---|---|---|
| `/opt/x-data-mcp-preview` total | 1.1 GiB | ≤4 GiB ✓ |
| disk free on `/` | 5.9 GiB | ≥2 GiB ✓ |
| `preview.sqlite` | 200 MB | — |
| `preview.sqlite-wal` | 0 B (checkpointed) | — |
| `generations/` (acquire store) | 547 MB | — |
| `acquire.sqlite` | 52 KB | — |

## 250-day soak (local, earlier evidence)

`tests/sqlite_range_update.rs::two_fifty_day_append_growth_stays_bounded` —
one multi-item update task/day through real `prepare_range`/`commit_range`:
early mean 74.8 KB/day, late-20-day mean 76.2 KB/day, ratio ≈1.02 (≤1.20 gate).

## Scope notes

- 100-item daily update is a subset timing, not full-market evidence.
- `production_ready` remains `false` pending issue #225 certification.
- `market.yearly` accepts explicit items only (no default selection).
