# NM-006 P1 platform validation — evidence

Baseline `8a02e30218d0fc48f3f5f208ec57eab6ea91687b` (`migration/node-devin`).

## Contents

- `probe-results.jsonl` — raw JSONL, one line per case (`{case,status,detail,evidence}`): 14 lines, 12 PASS + runtime INFO + node:sqlite NOT_RUN.
- `results.json` — consolidated decision record: environment, candidates, probe command/exit, synthetic DB digests, per-case statuses, selection rationale, gaps.
- `SHA256SUMS` — over both files.

## Reproduction

```sh
cd <worktree>                       # package.json/package-lock.json committed
npm ci                              # installs better-sqlite3@11.10.0 (prebuilt; needs no make)
mkdir -p -m 700 /dev/shm/nm006
target/release/task_sqlite init   /dev/shm/nm006/db18.sqlite
target/release/task_sqlite init19 /dev/shm/nm006/db19.sqlite
: > /dev/shm/nm006/empty.db && head -c 100 /dev/urandom > /dev/shm/nm006/garbage.bin
chmod 600 /dev/shm/nm006/*
node scripts/node_migration_p1/sqlite_probe.mjs /dev/shm/nm006
```

Boundaries: Node v20.19.2 / npm 9.2.0 on Box; `make` absent so node-gyp cannot build — better-sqlite3@13 (engines >=22) already demonstrated that failure. `node:sqlite` requires Node >=22.5 (NOT_RUN here). All DBs synthetic on /dev/shm; no production/live DBs touched.
