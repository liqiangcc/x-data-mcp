#!/usr/bin/env bash
# Deploy a tagged release to the Phase-G two-MCP topology and produce an
# evidence report. Companion to deploy_preview.sh (legacy single-service
# topology); this script targets the #262/#259 production shape:
#
#   x-data-mcp-data.service     data_service <db> <mcp.sock>
#   x-data-mcp-acquire.service  acquire_service <generation-root> <acquire.sock>
#
# Fresh schema-20 install semantics (#259 FRESH_REBUILD_REQUIRED): the
# existing market database is NEVER upgraded in place and NEVER deleted —
# it is renamed aside and `task_sqlite init21` creates a fresh compact
# database at the path the service unit already uses.
#
# Fail-closed:
#   * any unmet precondition exits non-zero before touching /opt;
#   * once the mutating phase starts, any later failure restores the
#     previous current symlink, renames the archived DB back, and restarts
#     the previous services — recording the rollback outcome;
#   * every failure writes reports/<tag>-phaseg-deployment.json with
#     status FAILED_AT_<step>.
#
# Socket/db/generation paths are parsed from the installed units'
# ExecStart lines — the script does not invent a layout. Missing units
# fail with the required unit bodies printed for one-time ops install.
#
# Usage: deploy_phase_g.sh <tag> [--dry-run] [--force-git-transport]
set -euo pipefail

DEPLOY_ROOT=${XDM_DEPLOY_ROOT:-/opt/x-data-mcp-preview}
SCRIPTS_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
DATA_SERVICE=${XDM_DATA_SERVICE:-x-data-mcp-data.service}
ACQ_SERVICE=${XDM_ACQ_SERVICE:-x-data-mcp-acquire.service}
REPORTS="$DEPLOY_ROOT/reports"
REPO=${REPO:-/root/x-data-mcp}
TAG=""
DRY_RUN=0
FORCE_GIT=0
for arg in "$@"; do
    case "$arg" in
        --dry-run) DRY_RUN=1 ;;
        --force-git-transport) FORCE_GIT=1 ;;
        -*) echo "unknown flag: $arg" >&2; exit 2 ;;
        *) [[ -z "$TAG" ]] && TAG="$arg" || { echo "unexpected arg: $arg" >&2; exit 2; } ;;
    esac
done
[[ "$TAG" =~ ^v[0-9] ]] || { echo "usage: deploy_phase_g.sh <tag> [--dry-run] [--force-git-transport]" >&2; exit 2; }
TAG_SHORT=$(sed -E 's/^v[0-9.]+-//; s/\.//g' <<<"$TAG")

step=init
SHA=""
RELEASE_DIR=""
DB=""
DB_ARCHIVED=""
SOCK_DATA=""
SOCK_ACQ=""
ACQ_ROOT=""
schema_before=""
current_before=""
MUTATING=0
INSTALLED_NEW=0
WORK=$(mktemp -d "/tmp/deploy-pg-${TAG}-XXXXXX")
trap 'rm -rf "$WORK"' EXIT

report() {  # report <status> [rollback_outcome]
    python3 - "$TAG" "$SHA" "$1" "$step" "$current_before" \
        "$schema_before" "$DB" "$DB_ARCHIVED" "${2:-}" "$REPORTS" "$TAG_SHORT" <<'PY' || true
import json, pathlib, sys
(tag, sha, status, step, prev, sb, db, archived, rollback, reports, short) = sys.argv[1:]
dep = {"status": status, "code_sha": sha or None, "release": tag,
       "topology": "phase-g two-MCP (data + acquire)",
       "failed_at": step if status.startswith("FAILED_AT_") else None,
       "previous_release": prev or None,
       "market_db": db or None,
       "archived_db": archived or None,
       "before": {"schema": int(sb)} if sb else {},
       "rollback_outcome": rollback or None,
       "production": "NOT_PRODUCTION_READY"}
pathlib.Path(f"{reports}/{short}-phaseg-deployment.json").write_text(
    json.dumps(dep, indent=2))
PY
}

rollback() {
    local outcome="not_needed"
    if [[ "$MUTATING" == "1" ]]; then
        outcome="failed"
        systemctl stop "$ACQ_SERVICE" 2>/dev/null || true
        systemctl stop "$DATA_SERVICE" 2>/dev/null || true
        if [[ -n "$current_before" ]]; then
            ln -sfn "$current_before" "$DEPLOY_ROOT/current" || true
        fi
        # Restore the pre-deploy market DB: drop the fresh file and put
        # the archived database back at the unit's path.
        if [[ -n "$DB" && -n "$DB_ARCHIVED" && -f "$DB_ARCHIVED" ]]; then
            rm -f "$DB" "$DB-shm" "$DB-wal" || true
            mv "$DB_ARCHIVED" "$DB" || true
        fi
        if systemctl start "$DATA_SERVICE" 2>/dev/null && systemctl is-active --quiet "$DATA_SERVICE" \
           && systemctl start "$ACQ_SERVICE" 2>/dev/null && systemctl is-active --quiet "$ACQ_SERVICE"; then
            outcome="restored_previous_release_and_archived_db"
        else
            outcome="failed_services_not_restored"
        fi
    fi
    echo "$outcome"
}

fail() {
    local outcome
    outcome=$(rollback)
    if [[ "$DRY_RUN" != "1" && "$INSTALLED_NEW" == "1" && -n "$RELEASE_DIR" \
          && "$(readlink "$DEPLOY_ROOT/current" 2>/dev/null || true)" != "$RELEASE_DIR" ]]; then
        rm -rf "$RELEASE_DIR" || true
    fi
    [[ "$DRY_RUN" == "1" ]] || report "FAILED_AT_$step" "$outcome"
    echo "FAILED_AT_$step: $1 (rollback: $outcome)" >&2
    exit 1
}

unit_exec_start() { systemctl cat "$1" 2>/dev/null | sed -nE 's/^ExecStart=//p' | head -1; }

required_units() {
    cat <<EOF
Required units (install once, then re-run):

/etc/systemd/system/$DATA_SERVICE
  [Service]
  ExecStart=$DEPLOY_ROOT/current/bin/data_service $DEPLOY_ROOT/state/preview.sqlite $DEPLOY_ROOT/run/mcp.sock
  Restart=on-failure

/etc/systemd/system/$ACQ_SERVICE
  [Service]
  ExecStart=$DEPLOY_ROOT/current/bin/acquire_service $DEPLOY_ROOT/state/generations $DEPLOY_ROOT/run/acquire.sock
  Restart=on-failure
EOF
}

# --- 1. preconditions: units installed, paths parsed from ExecStart ----------
step=preconditions
data_exec=$(unit_exec_start "$DATA_SERVICE")
acq_exec=$(unit_exec_start "$ACQ_SERVICE")
if [[ -z "$data_exec" || -z "$acq_exec" ]]; then
    echo "missing systemd unit(s)" >&2
    required_units >&2
    exit 2
fi
DB=$(awk '{print $2}' <<<"$data_exec")
SOCK_DATA=$(awk '{print $3}' <<<"$data_exec")
ACQ_ROOT=$(awk '{print $2}' <<<"$acq_exec")
SOCK_ACQ=$(awk '{print $3}' <<<"$acq_exec")
[[ -n "$DB" && -n "$SOCK_DATA" && -n "$ACQ_ROOT" && -n "$SOCK_ACQ" ]] \
    || fail "could not parse db/socket paths from unit ExecStart lines"
[[ "$data_exec" == *"/current/bin/data_service"* ]] \
    || fail "$DATA_SERVICE ExecStart does not use the current symlink"
[[ "$acq_exec" == *"/current/bin/acquire_service"* ]] \
    || fail "$ACQ_SERVICE ExecStart does not use the current symlink"

# --- 2. resolve release + verify package in $WORK before touching /opt -------
step=resolve_release
TARBALL="x-data-mcp-${TAG#v}-linux-x86_64.tar.gz"
if [[ "$FORCE_GIT" == "1" ]]; then
    # Git-transport mode: no GitHub release object is required. refs/dist/<tag>
    # carries dist-manifest.json binding tag + tag_sha + tarball sha256.
    remote=$(git -C "$REPO" remote get-url origin) || fail "origin url"
    git init --bare -q "$WORK/repo"
    git -C "$WORK/repo" fetch --depth 1 "$remote" "refs/dist/$TAG" \
        || fail "refs/dist/$TAG not available"
    dist_manifest=$(git -C "$WORK/repo" show "FETCH_HEAD:dist-manifest.json") \
        || fail "refs/dist/$TAG lacks dist-manifest.json"
    SHA=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["tag_sha"])' <<<"$dist_manifest")
    asset_digest=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["tarball_sha256"])' <<<"$dist_manifest")
    [[ "$SHA" =~ ^[0-9a-f]{40}$ && "$asset_digest" =~ ^[0-9a-f]{64}$ ]] \
        || fail "dist-manifest binding malformed"
else
    release_json=$(gh release view "$TAG" --json targetCommitish,assets) || fail "release $TAG missing"
    SHA=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["targetCommitish"])' <<<"$release_json")
    asset_digest=$(TARBALL="$TARBALL" python3 -c '
import json, os, sys
a = {x["name"]: x["digest"].removeprefix("sha256:") for x in json.load(sys.stdin)["assets"]}
need = [os.environ["TARBALL"], os.environ["TARBALL"] + ".sha256", "smoke-report.json"]
missing = [n for n in need if n not in a]
if missing:
    sys.exit("missing release assets: %s" % missing)
print(a[os.environ["TARBALL"]])
' <<<"$release_json") || fail "release assets incomplete"
fi
RELEASE_DIR="$DEPLOY_ROOT/releases/$SHA"

step=before_state
current_before=$(readlink "$DEPLOY_ROOT/current" || true)
if [[ -f "$DB" ]]; then
    schema_before=$(python3 -c "import sqlite3; print(sqlite3.connect('file:$DB?mode=ro', uri=True).execute('PRAGMA user_version').fetchone()[0])")
else
    schema_before=0
fi
svc_before=$(systemctl show "$DATA_SERVICE" "$ACQ_SERVICE" -p Id,ActiveState 2>/dev/null || true)

if [[ "$DRY_RUN" == "1" ]]; then
    cat <<EOF
DRY-RUN phase-g deploy $TAG
  release target:  $SHA
  tarball asset:   $TARBALL sha256:$asset_digest
  release dir:     $RELEASE_DIR
  current ->       $current_before
  data service:    $data_exec
  acquire service: $acq_exec
  market db:       $DB (user_version=$schema_before)
  acquire root:    $ACQ_ROOT
  sockets:         $SOCK_DATA  $SOCK_ACQ
  plan: fetch+verify tarball -> stage release dir -> archive existing db
        aside (never migrated/deleted) -> task_sqlite init21 fresh db ->
        stop both services -> switch current -> start both -> socket +
        schema=20 + tools smoke -> report; failures after mutation restore
        previous release + archived db + old services
EOF
    exit 0
fi

step=fetch_package
fetch_git() {
    if [[ ! -d "$WORK/repo" ]]; then
        remote=$(git -C "$REPO" remote get-url origin) || fail "origin url"
        git init --bare -q "$WORK/repo"
        git -C "$WORK/repo" fetch --depth 1 "$remote" "refs/dist/$TAG" \
            || fail "refs/dist/$TAG not available"
    fi
    git -C "$WORK/repo" archive FETCH_HEAD | tar -x -C "$WORK" \
        || fail "refs/dist/$TAG unpack"
}
fetch_gh() {
    (cd "$WORK" && timeout 60 gh release download "$TAG" -p "$TARBALL" -p "$TARBALL.sha256" -p smoke-report.json) || return 1
}
if [[ "$FORCE_GIT" == "1" ]]; then
    fetch_git
else
    fetch_gh || { echo "gh download slow/failed; falling back to refs/dist/$TAG" >&2; fetch_git; }
fi
[[ -f "$WORK/$TARBALL" && -f "$WORK/$TARBALL.sha256" ]] || fail "tarball or .sha256 missing"
(cd "$WORK" && sha256sum -c "$TARBALL.sha256" >/dev/null) || fail "tarball sha256 mismatch"
[[ "$(sha256sum "$WORK/$TARBALL" | cut -d' ' -f1)" == "$asset_digest" ]] || fail "tarball sha256 differs from release asset digest"
python3 - "$WORK/smoke-report.json" "$SHA" "$TAG" <<'PY' || fail "smoke-report binding"
import json, sys
r = json.load(open(sys.argv[1]))
assert r["status"] == "PASS", r["status"]
assert sys.argv[2] in r["version"], r["version"]
assert sys.argv[3].lstrip("v") in r["version"], r["version"]
PY
mkdir -p "$WORK/stage"
tar -xzf "$WORK/$TARBALL" -C "$WORK/stage" --strip-components=1 || fail "unpack"
(cd "$WORK/stage" && sha256sum --check SHA256SUMS >/dev/null) || fail "package SHA256SUMS"
"$WORK/stage/bin/mcp_data" --version | grep -q "$SHA" || fail "binary version does not embed $SHA"
[[ -x "$WORK/stage/bin/acquire_service" && -x "$WORK/stage/bin/task_sqlite" ]] \
    || fail "release lacks acquire_service/task_sqlite"
step=install_release
if [[ ! -d "$RELEASE_DIR" ]]; then
    mv "$WORK/stage" "$RELEASE_DIR" || fail "install release dir"
    INSTALLED_NEW=1
fi

# --- 3. mutation: archive old db, fresh init21, switch, start -----------------
step=schema21_fresh
MUTATING=1
systemctl stop "$ACQ_SERVICE" || fail "stop $ACQ_SERVICE"
systemctl stop "$DATA_SERVICE" || fail "stop $DATA_SERVICE"
if [[ -f "$DB" ]]; then
    DB_ARCHIVED="${DB}.v${schema_before}-archived-$(date -u +%Y%m%dT%H%M%SZ)"
    mv "$DB" "$DB_ARCHIVED" || fail "archive existing market db"
    rm -f "$DB-shm" "$DB-wal" || true
fi
# The data socket's owner marker binds the previous database inode; the
# fresh init21 file is a new identity, so rebind deliberately.
rm -f "$SOCK_DATA.owner.json" || fail "clear stale endpoint owner"
init_out=$("$RELEASE_DIR/bin/task_sqlite" init21 "$DB") || fail "task_sqlite init21"
[[ "$init_out" == *"task schema 21 ready"* ]] || fail "init21 did not report schema 21"
chmod 600 "$DB" || fail "chmod private market db"
chown "$(id -u):$(id -g)" "$DB" || fail "chown market db"
fresh_schema=$(python3 -c "import sqlite3; print(sqlite3.connect('file:$DB?mode=ro', uri=True).execute('PRAGMA user_version').fetchone()[0])")
[[ "$fresh_schema" == "21" ]] || fail "fresh db user_version=$fresh_schema != 21"

step=switch
ln -sfn "$RELEASE_DIR" "$DEPLOY_ROOT/current" || fail "switch current"
systemctl start "$DATA_SERVICE" || fail "start $DATA_SERVICE"
systemctl start "$ACQ_SERVICE" || fail "start $ACQ_SERVICE"
# Readiness is the MCP handshake on each socket (scripts/wait_service_ready.py),
# never a fixed sleep or a bare socket-file test: a cold data service binds the
# endpoint only after registry_sweep + series_startup_check (~31s on 2026-10-23).
READY_TIMEOUT=${XDM_READY_TIMEOUT:-120}
python3 "$SCRIPTS_DIR/wait_service_ready.py" "$SOCK_DATA" --timeout "$READY_TIMEOUT" \
    --expect-tool plan_market_update || fail "data socket did not become ready"
python3 "$SCRIPTS_DIR/wait_service_ready.py" "$SOCK_ACQ" --timeout "$READY_TIMEOUT" \
    --expect-tool acquire_generation || fail "acquire socket did not become ready"
systemctl is-active --quiet "$DATA_SERVICE" || fail "$DATA_SERVICE not active"
systemctl is-active --quiet "$ACQ_SERVICE" || fail "$ACQ_SERVICE not active"

# --- 4. smoke: both sockets answer MCP tools/list -----------------------------
step=smoke
python3 - "$SOCK_DATA" "$SOCK_ACQ" "$RELEASE_DIR" <<'PY' || fail "dual-socket MCP smoke"
import json, subprocess, select, sys

def rpc(client, method, params, i):
    client.stdin.write(json.dumps(dict(jsonrpc="2.0", id=i, method=method, params=params)) + "\n")
    client.stdin.flush()
    assert select.select([client.stdout], [], [], 10)[0], "rpc timeout"
    r = json.loads(client.stdout.readline())
    assert r["id"] == i and "error" not in r, r
    return r["result"]

def tools(sock, rel):
    client = subprocess.Popen([rel + "/bin/mcp_connect", sock],
                            stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
    try:
        rpc(client, "initialize", {}, 1)
        return {t["name"] for t in rpc(client, "tools/list", {}, 2)["tools"]}
    finally:
        if client.poll() is None:
            client.kill()

data_tools = tools(sys.argv[1], sys.argv[3])
acq_tools = tools(sys.argv[2], sys.argv[3])
need_data = {"update_data", "start_collection", "get_data_availability", "query_data"}
need_acq = {"check_official_updates", "acquire_generation", "activate_generation"}
missing = (need_data - data_tools) | (need_acq - acq_tools)
assert not missing, f"missing tools: {missing}"
print("data tools ok, acquire tools ok")
PY

step=report
report "DEPLOYED"
echo "DEPLOYED $TAG ($SHA) phase-g topology; market db user_version=20 (fresh)"
[[ -z "$DB_ARCHIVED" ]] || echo "previous market db archived at $DB_ARCHIVED"
echo "report: $REPORTS/${TAG_SHORT}-phaseg-deployment.json"
