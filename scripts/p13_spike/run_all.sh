#!/usr/bin/env bash
# NM-061 S13 kernel-parity spike runner.
# Builds src_node/native/rename_noreplace.node (flockfd, statfsmagic,
# getpeereuid, rename_noreplace, openat_*, unlinkat_file), creates a
# private 0700 scratch dir on tmpfs (/dev/shm — whitelisted magic), runs
# every proof, and exits nonzero if any primitive reports a FAIL.
set -u
ROOT=$(git rev-parse --show-toplevel)
cd "$ROOT" || exit 1

gcc -shared -fPIC -O2 -o src_node/native/rename_noreplace.node \
    src_node/native/rename_noreplace.c -I/usr/include/node || exit 1

DIR=$(mktemp -d /dev/shm/s13-spike-XXXXXX) || exit 1
chmod 700 "$DIR"
trap 'rm -rf "$DIR"' EXIT

FAIL=0
for p in flock peercred statfs openflags rename_noreplace signals frame eventloop; do
  echo "=== s13_${p} ===" >&2
  node "scripts/p13_spike/s13_${p}.mjs" "$DIR" || FAIL=1
done
exit $FAIL
