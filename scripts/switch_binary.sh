#!/usr/bin/env bash
# switch_binary.sh — preview binary-only switch (PLAN-H2 blueprint).
#
# OPERATOR-RUN ONLY. Real execution requires explicit user approval and
# the --execute flag; the default mode is dry-run, which prints the plan
# and the live preflight facts without changing anything. Never run on a
# path you do not control; the deploy root defaults to the production
# preview root and every mutation happens there.
#
# Invariants: the database file, its WAL/SHM, generations/, and socket
# owner markers are never deleted, moved or truncated. The only mutable
# production state this script can touch is the `current` symlink.
#
# usage: switch_binary.sh <v0.2.0-preview.N> <S:40-hex> [--execute]
# env:   XDM_DEPLOY_ROOT (default /opt/x-data-mcp-preview)
set -euo pipefail

ROOT=${XDM_DEPLOY_ROOT:-/opt/x-data-mcp-preview}
TAG=${1:?usage: switch_binary.sh <tag> <S(40-hex)> [--execute]}
S=${2:?usage: switch_binary.sh <tag> <S(40-hex)> [--execute]}
EXECUTE=0
[[ "${3:-}" == "--execute" ]] && EXECUTE=1
[[ "$TAG" =~ ^v0\.2\.0-preview\.[0-9]+$ && "$S" =~ ^[0-9a-f]{40}$ ]] || {
    echo "bad args: tag must be v0.2.0-preview.N, S a 40-hex commit" >&2
    exit 2
}
VER=${TAG#v}
TARBALL="x-data-mcp-${VER}-linux-x86_64.tar.gz"
PREV=$(readlink "$ROOT/current")
NEW="$ROOT/releases/$S"
WORK=$(mktemp -d /tmp/switch-bin-XXXXXX)
trap 'rm -rf "$WORK"' EXIT

run() {
    if [[ $EXECUTE == 1 ]]; then
        echo "+ $*"
        "$@"
    else
        echo "DRY: $*"
    fi
}
say() { echo "== $*"; }

say "P preconditions (read-only)"
df -h / | tail -1
free -m | sed -n 2p
[[ -d "$ROOT/releases/${PREV##*/}" ]] || {
    echo "previous release dir missing: $PREV" >&2
    exit 1
}
[[ -d "$WORK" ]]
echo "current -> $PREV"
echo "new     -> $NEW"
echo "OPERATOR MUST HAVE CONFIRMED via MCP (PLAN-H2 P1-P4): no inflight acquisition,"
echo "no non-terminal data task, baseline catalog/daily/yearly head counts recorded,"
echo "state/preview.sqlite size and -wal/-shm presence noted for post-switch compare."

say "S1 fetch + verify release artifacts into $WORK (plan only in dry-run)"
run gh release download "$TAG" -p "$TARBALL" -p "$TARBALL.sha256" -p smoke-report.json --dir "$WORK"
if [[ $EXECUTE == 1 ]]; then
    (cd "$WORK" && sha256sum -c "$TARBALL.sha256")
    python3 - "$WORK/smoke-report.json" "$S" "$TAG" <<'PY'
import json,sys
r=json.load(open(sys.argv[1]))
assert r["status"]=="PASS" and sys.argv[2] in r["version"] and sys.argv[3].lstrip("v") in r["version"], r["version"]
PY
    mkdir -p "$WORK/stage"
    tar -xzf "$WORK/$TARBALL" -C "$WORK/stage" --strip-components=1
    (cd "$WORK/stage" && sha256sum --check SHA256SUMS >/dev/null)
    "$WORK/stage/bin/mcp_data" --version | grep -q "$S"
    [[ -x "$WORK/stage/bin/acquire_service" && -x "$WORK/stage/bin/data_service" ]]
else
    echo "DRY: sha256sum -c $TARBALL.sha256; smoke-report PASS+$S+$TAG assert; unpack; SHA256SUMS; mcp_data --version"
fi

say "S2 private smoke on the unpacked tree (tempdir db + unix sockets, not prod sockets)"
if [[ $EXECUTE == 1 ]]; then
    python3 "$WORK/stage/check.py" --bin-dir "$WORK/stage/bin" --report "$WORK/smoke.json"
else
    echo "DRY: python3 <stage>/check.py --bin-dir <stage>/bin --report <tmp>/smoke.json"
fi

say "S3 install release dir (rename into releases/; no service impact)"
[[ -e "$NEW" ]] && { echo "release dir already exists: $NEW" >&2; exit 1; }
run mv "$WORK/stage" "$NEW"
if [[ $EXECUTE != 1 ]]; then
    echo "DRY: verify (cd $NEW && sha256sum --check SHA256SUMS; bin/mcp_data --version)"
fi

rollback() {
    echo "ROLLBACK -> $PREV" >&2
    systemctl stop x-data-mcp-acquire x-data-mcp-data 2>/dev/null || true
    ln -sfn "$PREV" "$ROOT/current.rb" && mv -T "$ROOT/current.rb" "$ROOT/current"
    systemctl start x-data-mcp-data && systemctl start x-data-mcp-acquire
}
if [[ $EXECUTE == 1 ]]; then
    trap 'rollback; rm -rf "$WORK"' ERR
fi

say "S4 stop services (acquire first, then data). Downtime starts here."
say "   DB, -wal, -shm and socket owner markers are left untouched."
run systemctl stop x-data-mcp-acquire
run systemctl stop x-data-mcp-data

say "S5 atomic symlink switch (ln to staging name, then mv -T over current)"
run ln -sfn "$NEW" "$ROOT/current.new"
run mv -T "$ROOT/current.new" "$ROOT/current"

say "S6 start (data then acquire), wait for sockets, verify is-active and symlink"
run systemctl start x-data-mcp-data
run systemctl start x-data-mcp-acquire
if [[ $EXECUTE == 1 ]]; then
    for _ in $(seq 1 200); do
        [[ -S "$ROOT/run/mcp.sock" && -S "$ROOT/run/acquire.sock" ]] && break
        sleep 0.1
    done
    [[ -S "$ROOT/run/mcp.sock" && -S "$ROOT/run/acquire.sock" ]]
    systemctl is-active --quiet x-data-mcp-data
    systemctl is-active --quiet x-data-mcp-acquire
    [[ "$(readlink "$ROOT/current")" == "$NEW" ]]
    trap - ERR
fi

say "S7 next steps (operator, read-only via MCP, BEFORE any write task):"
echo "   capability revisions generic-data-v3/x-data-acquire-v1; tools 20/10;"
echo "   sync_status idle and active generation unchanged; pending_roles present;"
echo "   head counts equal baseline; sample query_data reads unchanged."
say "ROLLBACK plan on any mismatch (S2 failure aborts before touching prod; S6/S7 failure):"
echo "   systemctl stop both services; mv -T symlink back to $PREV; restart data then acquire;"
echo "   re-run S7 checks (a)-(d)(f). The database is never modified, so no DB restore is needed."
