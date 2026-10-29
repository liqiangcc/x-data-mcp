#!/usr/bin/env bash
# Deploy a tagged release to the Phase-G two-MCP topology and produce an
# evidence report. Companion to deploy_preview.sh (legacy single-service
# topology); this script targets the #262/#259 production shape:
#
#   x-data-mcp-data.service     data_service <db> <sock> [--entry k] [<sock> ...] [tail]
#   x-data-mcp-acquire.service  acquire_service <generation-root> <acquire.sock>
#
# v8 phase 2 (D-56) lets one data unit serve several sockets/entries. The
# installed ExecStart is parsed by scripts/data_unit_layout.py, which accepts
# both the old single-socket shape and the repeated `<socket> --entry <kind>`
# shape, and keeps tail options such as the production `--poll-ms 1000`
# drop-in verbatim. Readiness probes every parsed socket with that entry's
# expected tool (research->query_data, ops->plan_market_update,
# maint->compute_metrics; scripts/wait_service_ready.py owns the mapping).
# This script never installs or rewrites a unit: the three-socket body is
# printed for the owner-approved change.
#
# Fresh compact-schema install semantics (#259 FRESH_REBUILD_REQUIRED): the
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
# The tunnel drop-in that gates tunnel startup on the same readiness probe
# is carried in the repo as a template (`tunnel_dropin_template()`, printed
# with the unit bodies below). This script never installs it: writing
# /etc/systemd/system is a separate owner-approved step (#296 P1-3).
#
# Usage: deploy_phase_g.sh <tag> [--dry-run] [--force-git-transport]
set -euo pipefail

DEPLOY_ROOT=${XDM_DEPLOY_ROOT:-/opt/x-data-mcp-preview}
SCRIPTS_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
DATA_SERVICE=${XDM_DATA_SERVICE:-x-data-mcp-data.service}
ACQ_SERVICE=${XDM_ACQ_SERVICE:-x-data-mcp-acquire.service}
TUNNEL_SERVICE=${XDM_TUNNEL_SERVICE:-x-data-mcp-tunnel.service}
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
SOCK_ACQ=""
# Parsed data-unit layout: "entry=socket" specs, the sockets alone (owner
# markers, target rendering) and the preserved trailing options.
DATA_ENDPOINTS=()
DATA_SOCKETS=()
DATA_TAIL_ARGS=()
DATA_UNIT_BIN=""
DATA_UNIT_DB=""
DATA_UNIT_TAIL=()
DATA_UNIT_ENTRIES=()
DATA_UNIT_SOCKETS=()
ACQ_ROOT=""
schema_before=""
current_before=""
MUTATING=0
INSTALLED_NEW=0
REPLAY_REPORT=""
SOURCE_STORE="$DEPLOY_ROOT/state/source-store"
WORK=$(mktemp -d "/tmp/deploy-pg-${TAG}-XXXXXX")
trap 'rm -rf "$WORK"' EXIT

report() {  # report <status> [rollback_outcome]
    python3 - "$TAG" "$SHA" "$1" "$step" "$current_before" \
        "$schema_before" "$DB" "$DB_ARCHIVED" "${2:-}" "$REPORTS" "$TAG_SHORT" \
        "$REPLAY_REPORT" <<'PY' || true
import json, pathlib, sys
(tag, sha, status, step, prev, sb, db, archived, rollback, reports, short,
 replay_path) = sys.argv[1:]
replay = None
if replay_path:
    try:
        replay = json.loads(pathlib.Path(replay_path).read_text())
    except (OSError, ValueError):
        replay = {"status": "report_unreadable", "path": replay_path}
dep = {"status": status, "code_sha": sha or None, "release": tag,
       "topology": "phase-g two-MCP (data + acquire)",
       "failed_at": step if status.startswith("FAILED_AT_") else None,
       "previous_release": prev or None,
       "market_db": db or None,
       "archived_db": archived or None,
       "before": {"schema": int(sb)} if sb else {},
       "source_store_replay": replay,
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

# `|| true`: a missing unit makes `systemctl cat` exit non-zero, and with
# pipefail that used to abort the script before the "missing systemd unit(s)"
# branch below could print the required unit bodies and the tunnel template.
unit_exec_start() { systemctl cat "$1" 2>/dev/null | sed -nE 's/^ExecStart=//p' | head -1 || true; }

required_units() {
    # Generated multi-socket body (v8 phase 2 §3). The tail keeps the
    # production drop-in's `--poll-ms 1000`; XDM_DATA_UNIT_TAIL overrides it.
    local tail=${XDM_DATA_UNIT_TAIL:---poll-ms 1000} target
    target=$(python3 "$SCRIPTS_DIR/data_unit_layout.py" --render \
        --binary "$DEPLOY_ROOT/current/bin/data_service" \
        --db "$DEPLOY_ROOT/state/preview.sqlite" \
        --endpoint "ops=$DEPLOY_ROOT/run/mcp.sock" \
        --endpoint "research=$DEPLOY_ROOT/run/research.sock" \
        --endpoint "maint=$DEPLOY_ROOT/run/maint.sock" \
        --tail "$tail") \
        || { echo "cannot render the data unit ExecStart" >&2; exit 2; }
    cat <<EOF
Required units (install once, then re-run):

/etc/systemd/system/$DATA_SERVICE
  [Service]
  ExecStart=$target
  ExecStartPre=$DEPLOY_ROOT/current/scripts/wait_service_ready.py $DEPLOY_ROOT/run/mcp.sock --timeout 120 --expect-tool plan_market_update
  ExecStartPre=$DEPLOY_ROOT/current/scripts/wait_service_ready.py $DEPLOY_ROOT/run/research.sock --timeout 120 --expect-tool query_data
  ExecStartPre=$DEPLOY_ROOT/current/scripts/wait_service_ready.py $DEPLOY_ROOT/run/maint.sock --timeout 120 --expect-tool compute_metrics
  Restart=on-failure

/etc/systemd/system/$ACQ_SERVICE
  [Service]
  ExecStart=$DEPLOY_ROOT/current/bin/acquire_service $DEPLOY_ROOT/state/generations $DEPLOY_ROOT/run/acquire.sock
  Restart=on-failure
EOF
    tunnel_dropin_template
}

# #296 P1-3 / D-17: the tunnel must gate on the same readiness definition as
# the deploy scripts — the repo probe from the `current` symlink, 120s budget,
# and the tool assertion that proves the data plane is actually serving.
# TimeoutStartSec must exceed the probe budget: systemd's default is 90s and
# covers ExecStartPre, so a 120s wait would be killed before its own deadline.
# This is a template only; the script writes no unit, drop-in or /etc file, and
# switching the live unit is a separate owner-approved action.
tunnel_dropin_template() {
    cat <<EOF
Tunnel drop-in (template only; this script does not install it):
/etc/systemd/system/$TUNNEL_SERVICE.d/data-service.conf
  [Unit]
  Wants=$DATA_SERVICE
  After=$DATA_SERVICE

  [Service]
  ExecStartPre=$DEPLOY_ROOT/current/scripts/wait_service_ready.py $DEPLOY_ROOT/run/mcp.sock --timeout 120 --expect-tool plan_market_update
  TimeoutStartSec=180
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
layout=$(python3 "$SCRIPTS_DIR/data_unit_layout.py" --exec-start "$data_exec" --format sh) \
    || fail "cannot parse $DATA_SERVICE ExecStart as <db> <socket> [--entry k] ... [tail]"
eval "$layout"
DB=$DATA_UNIT_DB
DATA_TAIL_ARGS=("${DATA_UNIT_TAIL[@]}")
for i in "${!DATA_UNIT_SOCKETS[@]}"; do
    DATA_SOCKETS+=("${DATA_UNIT_SOCKETS[$i]}")
    DATA_ENDPOINTS+=("${DATA_UNIT_ENTRIES[$i]}=${DATA_UNIT_SOCKETS[$i]}")
done
ACQ_ROOT=$(awk '{print $2}' <<<"$acq_exec")
SOCK_ACQ=$(awk '{print $3}' <<<"$acq_exec")
[[ -n "$DB" && ${#DATA_SOCKETS[@]} -gt 0 && -n "$ACQ_ROOT" && -n "$SOCK_ACQ" ]] \
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

# Target three-socket body for the owner-approved unit change: keep the
# sockets the installed unit already has and add the missing standard entries,
# preserving every parsed tail option (e.g. --poll-ms 1000).
target_exec=$(python3 "$SCRIPTS_DIR/data_unit_layout.py" --exec-start "$data_exec" --target) \
    || fail "cannot render the target data unit ExecStart"

if [[ "$DRY_RUN" == "1" ]]; then
    cat <<EOF
DRY-RUN phase-g deploy $TAG
  release target:  $SHA
  tarball asset:   $TARBALL sha256:$asset_digest
  release dir:     $RELEASE_DIR
  current ->       $current_before
  data service:    $data_exec
  data endpoints:  ${DATA_ENDPOINTS[*]}
  data tail args:  ${DATA_TAIL_ARGS[*]-}
  data target:     $target_exec
                   (printed only; the unit change is owner-approved, never here)
  acquire service: $acq_exec
  market db:       $DB (user_version=$schema_before)
  acquire root:    $ACQ_ROOT
  acquire socket:  $SOCK_ACQ
  plan: fetch+verify tarball -> stage release dir -> archive existing db
        aside (never migrated/deleted) -> task_sqlite init21 fresh db ->
        stop both services -> switch current -> start both -> readiness
        probe per data socket + acquire socket -> schema + tools smoke ->
        report; failures after mutation restore previous release + archived
        db + old services
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

# The fresh-init schema is a fact of the release commit, not of this script:
# read COMPACT_SCHEMA_VERSION at the tag's SHA (before anything is mutated) so
# the check moves with the code instead of pinning one schema number here.
step=expected_schema
EXPECTED_SCHEMA=${XDM_EXPECTED_SCHEMA:-$(git -C "$REPO" show "$SHA:src/sqlite_tasks.rs" 2>/dev/null \
    | sed -nE 's/^pub const COMPACT_SCHEMA_VERSION: u32 = ([0-9]+);$/\1/p')}
[[ "$EXPECTED_SCHEMA" =~ ^[0-9]+$ ]] || fail "cannot read COMPACT_SCHEMA_VERSION at $SHA"

# --- 3. mutation: archive old db, fresh init21, switch, start -----------------
step=schema_fresh
MUTATING=1
systemctl stop "$ACQ_SERVICE" || fail "stop $ACQ_SERVICE"
systemctl stop "$DATA_SERVICE" || fail "stop $DATA_SERVICE"
if [[ -f "$DB" ]]; then
    DB_ARCHIVED="${DB}.v${schema_before}-archived-$(date -u +%Y%m%dT%H%M%SZ)"
    mv "$DB" "$DB_ARCHIVED" || fail "archive existing market db"
    rm -f "$DB-shm" "$DB-wal" || true
fi
# Each data socket's owner marker binds the previous database inode; the
# fresh init21 file is a new identity, so rebind every endpoint deliberately.
for socket in "${DATA_SOCKETS[@]}"; do
    rm -f "$socket.owner.json" || fail "clear stale endpoint owner ($socket)"
done
init_out=$("$RELEASE_DIR/bin/task_sqlite" init21 "$DB") || fail "task_sqlite init21"
[[ "$init_out" == *"task schema $EXPECTED_SCHEMA ready"* ]] \
    || fail "init21 did not report schema $EXPECTED_SCHEMA"
chmod 600 "$DB" || fail "chmod private market db"
chown "$(id -u):$(id -g)" "$DB" || fail "chown market db"
fresh_schema=$(python3 -c "import sqlite3; print(sqlite3.connect('file:$DB?mode=ro', uri=True).execute('PRAGMA user_version').fetchone()[0])")
[[ "$fresh_schema" == "$EXPECTED_SCHEMA" ]] \
    || fail "fresh db user_version=$fresh_schema != $EXPECTED_SCHEMA"

# --- 3b. source store replay (M3 PR-1a, writes since PR-6) ------------------
# The fresh database has empty M3 index tables; the source store under the
# deployment state directory is the only durable copy of facts a fresh init
# cannot regenerate. Since M3 PR-6 (`replay --scope industry`) this step
# *writes* the fresh database, so it runs after `init21` and before the
# services start: the replay takes the database service lease, and a live
# writer would make it fail instead of rebuilding. `store_empty` (before the
# first import) is a recorded deployment fact, not a failure; a corrupt store
# or a drift verdict fails the deployment instead of silently deploying empty
# history.
step=replay_source_store
replay_out=$("$RELEASE_DIR/bin/source_store" replay --store "$SOURCE_STORE" --db "$DB" --scope all) \
    || fail "source_store replay failed"
printf '%s\n' "$replay_out" > "$WORK/source-store-replay.json"
REPLAY_REPORT="$WORK/source-store-replay.json"
replay_status=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["status"])' "$REPLAY_REPORT") \
    || fail "source store replay report unreadable"
echo "source store replay: $replay_status (store $SOURCE_STORE)"

step=switch
ln -sfn "$RELEASE_DIR" "$DEPLOY_ROOT/current" || fail "switch current"
systemctl start "$DATA_SERVICE" || fail "start $DATA_SERVICE"
systemctl start "$ACQ_SERVICE" || fail "start $ACQ_SERVICE"
# Readiness is the MCP handshake on every socket (scripts/wait_service_ready.py),
# never a fixed sleep or a bare socket-file test: a cold data service binds the
# endpoint only after registry_sweep + series_startup_check (~31s on 2026-10-23).
# `--endpoint entry=socket` makes the probe also assert that entry's tool
# (research=query_data, ops=plan_market_update, maint=compute_metrics).
READY_TIMEOUT=${XDM_READY_TIMEOUT:-120}
probe_args=(--timeout "$READY_TIMEOUT")
for spec in "${DATA_ENDPOINTS[@]}"; do probe_args+=(--endpoint "$spec"); done
python3 "$SCRIPTS_DIR/wait_service_ready.py" "${probe_args[@]}" \
    || fail "data socket(s) did not become ready"
python3 "$SCRIPTS_DIR/wait_service_ready.py" "$SOCK_ACQ" --timeout "$READY_TIMEOUT" \
    --expect-tool acquire_generation || fail "acquire socket did not become ready"
systemctl is-active --quiet "$DATA_SERVICE" || fail "$DATA_SERVICE not active"
systemctl is-active --quiet "$ACQ_SERVICE" || fail "$ACQ_SERVICE not active"

# --- 4. smoke: every socket answers MCP tools/list ---------------------------
step=smoke
python3 - "${DATA_ENDPOINTS[@]}" -- "$SOCK_ACQ" "$RELEASE_DIR" <<'PY' || fail "MCP socket smoke"
import json, subprocess, select, sys

args = sys.argv[1:]
split = args.index("--")
data_specs = args[:split]
acq_sock, release = args[split + 1:]
ENTRY_TOOLS = {"research": "query_data", "ops": "plan_market_update",
               "maint": "compute_metrics"}

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

need_ops = {"update_data", "start_collection", "get_data_availability", "query_data"}
need_acq = {"check_official_updates", "acquire_generation", "activate_generation"}
for spec in data_specs:
    entry, _, sock = spec.partition("=")
    served = tools(sock, release)
    assert ENTRY_TOOLS[entry] in served, f"{entry} socket lacks {ENTRY_TOOLS[entry]}"
    if entry == "ops":
        assert not need_ops - served, f"ops socket missing tools: {need_ops - served}"
acq_tools = tools(acq_sock, release)
assert not need_acq - acq_tools, f"acquire socket missing tools: {need_acq - acq_tools}"
print(f"data tools ok ({len(data_specs)} socket(s)), acquire tools ok")
PY

step=report
report "DEPLOYED"
echo "DEPLOYED $TAG ($SHA) phase-g topology; market db user_version=$fresh_schema (fresh)"
[[ -z "$DB_ARCHIVED" ]] || echo "previous market db archived at $DB_ARCHIVED"
echo "report: $REPORTS/${TAG_SHORT}-phaseg-deployment.json"
