# NM-004 recovery/fail-closed vectors — evidence

Baseline `9fc6a11036fbe8806b4213f98506e33ff82c7703` (`migration/node-devin`).

## Contents

- `matrix.json` — machine-readable coverage matrix: 11 rows over the AC1 categories with status (`observed_test` / `observed_test+live_probe` / `source_mapped` / `not_run`), mechanism, source paths, test names, expected fail-closed outcome, and observed result. Includes the executed test-run record (command, exit 0, 118 passed / 0 failed) and the P3/P4 gap list.
- `live-runs.jsonl` — 13 synthetic live-probe records: `{run, cmd, exit, output}` for the path/lease/socket/schema guards and the SIGKILL-restart recovery.
- `SHA256SUMS` — sha256 over both files.

## Reproduction notes

- Test run: `TMPDIR=/dev/shm cargo test --locked --test sqlite_tasks --test retry --test cancel --test occupancy --test stopped_verification --test sqlite_market --test local_runtime --test acquire_service --test publication_verification --test data_supply_generation` — 118 passed, 0 failed, exit 0.
- Live probes used `target/debug` binaries compiled at baseline worktree; `src/`+`tests/` are byte-identical between the build SHA (`ea14ede`) and baseline (`9fc6a11`) — verified by `git diff --stat`.
- Probe workspace: `/dev/shm/nm004` (tmpfs, 0700, UID-owned — required for the guards to pass at all; a 0755 parent is itself a reject case).
- The overlayfs DB probe used a copy inside the worktree (`dev-nm004.sqlite`, deleted after).
- No opt-out env vars were used; no official inputs, no ECS, no live databases.
