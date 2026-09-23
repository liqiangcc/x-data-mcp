# NM-008 P1 Linux primitive adapter — evidence

Baseline `2364c7de8513891395cb476cac2d356ade77f3cd` (`migration/node-devin`). Node v20.19.2 on Box (euid 1000).

## Contents

- `probe-results.jsonl` — one JSON line per case `{primitive,id,status,detail,evidence}`: 18 PASS / 2 NOT_RUN / 1 INFO / 0 FAIL.
- `results.json` — consolidated decision record per primitive with Node path, caveats, and rejected alternatives.
- `SHA256SUMS` — over the two files (manifest excludes itself).

## Reproduction

```sh
mkdir -p -m 700 /dev/shm/nm008
node scripts/node_migration_p1/linux_probe.mjs /dev/shm/nm008
```

No npm dependency needed for the covered primitives (pure `node:fs`/`node:net` + `/usr/bin/flock` from util-linux 2.41.5, already installed).

## Status summary

| Primitive | Node path | Status |
|---|---|---|
| fstatfs whitelist | `fs.statfsSync().type` vs Rust magic set | PASS (tmpfs accept, overlayfs reject, table match) |
| private_metadata | `fs.lstatSync` uid/mode/non-symlink | PASS; cross-UID NOT_RUN (single UID on Box) |
| O_NOFOLLOW | `fs.openSync(O_RDONLY\|O_NOFOLLOW)` → ELOOP | PASS |
| flock lease | `/usr/bin/flock -n` helper process | PASS (contention exit 1); fs-ext rejected (needs make) |
| inode identity | `statSync` dev+ino | PASS (replacement detected) |
| socket owner/no-clobber | `node:net` + `node:fs` mirror of `Endpoint::bind` | PASS (7 sub-cases incl. SIGKILL-stale rebind) |
| SO_PEERCRED | none — N-API/neon addon path selected | NOT_RUN (toolchain pending; 0700-dir invariant documented) |

Caveats recorded in results.json: path-vs-fd fstatfs delta, flock-helper lifetime model, SO_PEERCRED parity gap.
