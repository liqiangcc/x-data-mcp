# NM-009 P1 native Linux binding — evidence

Baseline `67c647992cf1d898f2f4f50f773886766e35efd7` (`migration/node-devin`). Node v20.19.2, euid 1000.

## Contents

- `probe-results.jsonl` — 10 cases: 8 PASS / 1 NOT_RUN / 1 INFO / 0 FAIL.
- `results.json` — environment, build recipe, artifact hashes, validated semantics, limitations.
- `SHA256SUMS` — over the two files (excludes itself).

## Reproduction

```sh
# build (no make/node-gyp; libnode-dev headers + gcc only)
gcc -shared -fPIC -O2 -o scripts/node_migration_p1/native/linux_guards.node \
    scripts/node_migration_p1/native/linux_guards.c -I/usr/include/node
mkdir -p -m 700 /dev/shm/nm009
node scripts/node_migration_p1/native_probe.mjs /dev/shm/nm009
```

## What was proven (real kernel syscalls, not simulation)

- `getpeereuid(fd)` — real `getsockopt(SO_PEERCRED)` on an accepted unix-socket fd → kernel `ucred {pid:2271907, uid:1000, gid:1000}`; same-UID authorize PASS; non-socket fd rejected. Cross-UID reject path NOT_RUN (single-UID box — kernel ucred cannot be forged locally).
- `flockfd(fd, op)` — real `flock()` on a process-owned fd: acquire/unlock/reacquire 0; second open-file-description contention → EWOULDBLOCK(11); **util-linux `flock -n` observes the N-API lease (exit 1) — kernel-enforced**; release on close; cross-process child → EWOULDBLOCK; EBADF(9) on invalid fd.
- No subprocess lease, no PID files, no skipped UID check.
