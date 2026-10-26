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
# Evidence report (#296 P1-3): every --execute run writes exactly one
# machine report — on success and on failure/rollback — to `--report <path>`
# or by default to
#     <ROOT>/reports/<short-tag>-switch-<UTC>.json
# e.g. <ROOT>/reports/preview36-switch-20261026T041500Z.json.
# Naming and the top-level field names follow deploy_phase_g.sh's
# `<short-tag>-phaseg-deployment.json` convention: `release` carries the tag,
# `code_sha` the commit, plus `status`, `started_at`/`finished_at`,
# `failed_at`, `previous_release`, `current_release`, `rollback_outcome`,
# `production`. `steps` holds one {step, status, elapsed_ms, evidence} record
# per S1–S7 and `readiness_probe` holds the JSON the probe itself printed.
# A dry-run writes nothing into ROOT: it prints the report path it would use.
#
# usage: switch_binary.sh <v0.2.0-preview.N> <S:40-hex> [--execute] [--report <path>]
# env:   XDM_DEPLOY_ROOT (default /opt/x-data-mcp-preview)
set -Eeuo pipefail

ROOT=${XDM_DEPLOY_ROOT:-/opt/x-data-mcp-preview}
SCRIPTS_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

EXECUTE=0
REPORT=""
REPORT_GIVEN=0
POSITIONAL=()
while (($#)); do
    case "$1" in
        --execute) EXECUTE=1; shift ;;
        --report) REPORT=${2:?usage: --report needs a path}; REPORT_GIVEN=1; shift 2 ;;
        --report=*) REPORT=${1#--report=}; REPORT_GIVEN=1; shift ;;
        -*) echo "unknown flag: $1 (usage: switch_binary.sh <tag> <S(40-hex)> [--execute] [--report <path>])" >&2; exit 2 ;;
        *) POSITIONAL+=("$1"); shift ;;
    esac
done
[[ ${#POSITIONAL[@]} -eq 2 ]] \
    || { echo "usage: switch_binary.sh <tag> <S(40-hex)> [--execute] [--report <path>]" >&2; exit 2; }
TAG=${POSITIONAL[0]}
S=${POSITIONAL[1]}
[[ "$TAG" =~ ^v0\.2\.0-preview\.[0-9]+$ && "$S" =~ ^[0-9a-f]{40}$ ]] || {
    echo "bad args: tag must be v0.2.0-preview.N, S a 40-hex commit" >&2
    exit 2
}
VER=${TAG#v}
TARBALL="x-data-mcp-${VER}-linux-x86_64.tar.gz"
# Same short-tag rule as deploy_phase_g.sh: v0.2.0-preview.36 -> preview36.
TAG_SHORT=$(sed -E 's/^v[0-9.]+-//; s/\.//g' <<<"$TAG")
utc_now() { date -u +%Y-%m-%dT%H:%M:%SZ; }
# Milliseconds without `date +%s%3N`: some coreutils builds (uutils) print full
# nanoseconds for %3N, which would corrupt every elapsed_ms in the report.
now_ms() {
    if [[ -n ${EPOCHREALTIME:-} ]]; then
        local us=${EPOCHREALTIME//[!0-9]/}
        echo $(( us / 1000 ))
    else
        echo "$(( $(date +%s) * 1000 ))"
    fi
}
STARTED_AT=$(utc_now)
if [[ $REPORT_GIVEN == 1 ]]; then
    :
else
    REPORT="$ROOT/reports/${TAG_SHORT}-switch-$(date -u +%Y%m%dT%H%M%SZ).json"
fi
WORK=$(mktemp -d /tmp/switch-bin-XXXXXX)
STEPS="$WORK/steps.jsonl"
trap 'rm -rf "$WORK"' EXIT

PREV=""
NEW=""
MUTATING=0
CUR_STEP=""
CUR_NAME=""
CUR_START=0
CUR_DONE=1
FAILED_STEP=""

run() {
    if [[ $EXECUTE == 1 ]]; then
        echo "+ $*"
        "$@"
    else
        echo "DRY: $*"
    fi
}
say() { echo "== $*"; }

record() {  # record <step> <name> <status> <elapsed_ms> <evidence>
    python3 - "$STEPS" "$1" "$2" "$3" "$4" "$5" <<'PY'
import json, sys
path, step, name, status, ms, evidence = sys.argv[1:]
with open(path, "a", encoding="utf-8") as f:
    f.write(json.dumps({"step": step, "name": name, "status": status,
                        "elapsed_ms": int(ms), "evidence": evidence},
                       ensure_ascii=False) + "\n")
PY
}
begin_step() {  # begin_step <id> <name>
    CUR_STEP=$1
    CUR_NAME=$2
    CUR_START=$(now_ms)
    CUR_DONE=0
    say "$CUR_STEP $CUR_NAME"
}
end_step() {  # end_step <evidence>
    record "$CUR_STEP" "$CUR_NAME" PASS "$(( $(now_ms) - CUR_START ))" "$1"
    CUR_DONE=1
}

# Report fields and file naming follow deploy_phase_g.sh's phase-g report.
write_report() {  # write_report <status> <rollback_outcome>
    local status=$1 outcome=$2 finished
    finished=$(utc_now)
    if [[ $EXECUTE != 1 ]]; then
        echo "DRY: report would be written to $REPORT (dry-run writes nothing into $ROOT)"
        return 0
    fi
    mkdir -p "$(dirname "$REPORT")"
    python3 - "$REPORT" "$TAG" "$S" "$status" "$STARTED_AT" "$finished" \
        "$FAILED_STEP" "$PREV" "$(readlink "$ROOT/current" 2>/dev/null || true)" \
        "$outcome" "$STEPS" "$WORK/readiness.data.json" "$WORK/readiness.acquire.json" <<'PY'
import json, pathlib, sys
(path, tag, sha, status, started, finished, failed_at, previous, current,
 outcome, steps_file, probe_data, probe_acquire) = sys.argv[1:]

def probe(path):
    p = pathlib.Path(path)
    if not p.is_file():
        return None
    try:
        return json.loads(p.read_text())
    except json.JSONDecodeError:
        return None

steps_path = pathlib.Path(steps_file)
steps = [json.loads(line) for line in steps_path.read_text().splitlines() if line.strip()] \
    if steps_path.is_file() else []
report = {"status": status,
          "kind": "binary-only switch (no DB migration, no rebuild)",
          "release": tag,
          "code_sha": sha,
          "started_at": started,
          "finished_at": finished,
          "failed_at": failed_at or None,
          "previous_release": previous or None,
          "current_release": current or None,
          "rollback_outcome": outcome or None,
          "steps": steps,
          "readiness_probe": {"data": probe(probe_data),
                              "acquire": probe(probe_acquire)},
          "production": "NOT_PRODUCTION_READY"}
pathlib.Path(path).write_text(json.dumps(report, indent=2) + "\n")
PY
    echo "report: $REPORT"
}

# Sets ROLLBACK_OUTCOME (not stdout: child commands must not corrupt it).
ROLLBACK_OUTCOME="not_needed"
rollback() {
    ROLLBACK_OUTCOME="not_needed"
    if [[ $EXECUTE == 1 && $MUTATING == 1 ]]; then
        echo "ROLLBACK -> $PREV" >&2
        ROLLBACK_OUTCOME="failed"
        systemctl stop x-data-mcp-acquire 2>/dev/null || true
        systemctl stop x-data-mcp-data 2>/dev/null || true
        ln -sfn "$PREV" "$ROOT/current.rb" && mv -T "$ROOT/current.rb" "$ROOT/current" || true
        if systemctl start x-data-mcp-data && systemctl is-active --quiet x-data-mcp-data \
           && systemctl start x-data-mcp-acquire && systemctl is-active --quiet x-data-mcp-acquire; then
            ROLLBACK_OUTCOME="restored_previous_release"
        else
            ROLLBACK_OUTCOME="failed_services_not_restored"
        fi
    fi
}

fail() {  # fail <message>: record the step, roll back, write the FAIL report, exit 1
    local msg=$1 outcome
    trap - ERR
    set +e
    if [[ $CUR_DONE == 0 ]]; then
        record "$CUR_STEP" "$CUR_NAME" FAIL "$(( $(now_ms) - CUR_START ))" "$msg"
        CUR_DONE=1
    fi
    FAILED_STEP=${FAILED_STEP:-${CUR_STEP:-preflight}}
    rollback
    outcome=$ROLLBACK_OUTCOME
    echo "FAILED_AT_$FAILED_STEP: $msg (rollback: $outcome)" >&2
    write_report FAIL "$outcome"
    exit 1
}
on_error() { fail "command failed: $BASH_COMMAND"; }
trap on_error ERR

# --- P. preconditions (read-only) --------------------------------------------
p_preconditions() {
    begin_step P "preconditions (read-only)"
    df -h / | tail -1
    free -m | sed -n 2p
    PREV=$(readlink "$ROOT/current")
    NEW="$ROOT/releases/$S"
    if [[ ! -d "$ROOT/releases/${PREV##*/}" ]]; then
        fail "previous release dir missing: $PREV"
    fi
    echo "current -> $PREV"
    echo "new     -> $NEW"
    echo "report  -> $REPORT"
    echo "OPERATOR MUST HAVE CONFIRMED via MCP (PLAN-H2 P1-P4): no inflight acquisition,"
    echo "no non-terminal data task, baseline catalog/daily/yearly head counts recorded,"
    echo "state/preview.sqlite size and -wal/-shm presence noted for post-switch compare."
    end_step "current -> $PREV; new -> $NEW; report -> $REPORT"
}

# --- S1. fetch + verify ------------------------------------------------------
s1_fetch_verify() {
    begin_step S1 "fetch + verify release artifacts into \$WORK (plan only in dry-run)"
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
        end_step "tarball sha256 + smoke-report PASS+$S+$TAG + package SHA256SUMS + mcp_data --version"
    else
        echo "DRY: sha256sum -c $TARBALL.sha256; smoke-report PASS+$S+$TAG assert; unpack; SHA256SUMS; mcp_data --version"
        end_step "dry-run: nothing downloaded or verified"
    fi
}

# --- S2. private smoke on the staged tree ------------------------------------
s2_private_smoke() {
    begin_step S2 "private smoke on the unpacked tree (tempdir db + unix sockets, not prod sockets)"
    if [[ $EXECUTE == 1 ]]; then
        python3 "$WORK/stage/check.py" --bin-dir "$WORK/stage/bin" --report "$WORK/smoke.json"
        end_step "staged check.py exited 0 (report \$WORK/smoke.json)"
    else
        echo "DRY: python3 <stage>/check.py --bin-dir <stage>/bin --report <tmp>/smoke.json"
        end_step "dry-run: no staged smoke"
    fi
}

# --- S3. install release dir -------------------------------------------------
s3_install() {
    begin_step S3 "install release dir (rename into releases/; no service impact)"
    if [[ -e "$NEW" ]]; then
        fail "release dir already exists: $NEW"
    fi
    run mv "$WORK/stage" "$NEW"
    if [[ $EXECUTE == 1 ]]; then
        end_step "release dir installed at $NEW"
    else
        echo "DRY: verify (cd $NEW && sha256sum --check SHA256SUMS; bin/mcp_data --version)"
        end_step "dry-run: no release dir installed"
    fi
}

# --- S4. stop services -------------------------------------------------------
s4_stop() {
    begin_step S4 "stop services (acquire first, then data). Downtime starts here."
    echo "   DB, -wal, -shm and socket owner markers are left untouched."
    # Set before the first stop: a partial stop already needs the restore path.
    if [[ $EXECUTE == 1 ]]; then
        MUTATING=1
    fi
    run systemctl stop x-data-mcp-acquire
    run systemctl stop x-data-mcp-data
    end_step "systemctl stop acquire,data issued"
}

# --- S5. atomic symlink switch ----------------------------------------------
s5_switch() {
    begin_step S5 "atomic symlink switch (ln to staging name, then mv -T over current)"
    run ln -sfn "$NEW" "$ROOT/current.new"
    run mv -T "$ROOT/current.new" "$ROOT/current"
    if [[ $EXECUTE == 1 ]]; then
        end_step "current -> $NEW"
    else
        end_step "dry-run: current unchanged"
    fi
}

# --- S6. start + readiness ---------------------------------------------------
s6_start() {
    begin_step S6 "start (data then acquire), wait for readiness, verify is-active and symlink"
    run systemctl start x-data-mcp-data
    run systemctl start x-data-mcp-acquire
    # Readiness is the MCP handshake on the socket (scripts/wait_service_ready.py),
    # never a fixed sleep or a bare socket-file test: a cold data service binds the
    # endpoint only after registry_sweep + series_startup_check (~31s on 2026-10-23).
    READY_TIMEOUT=${XDM_READY_TIMEOUT:-120}
    if [[ $EXECUTE == 1 ]]; then
        python3 "$SCRIPTS_DIR/wait_service_ready.py" "$ROOT/run/mcp.sock" \
            --timeout "$READY_TIMEOUT" --expect-tool plan_market_update \
            >"$WORK/readiness.data.json"
        python3 "$SCRIPTS_DIR/wait_service_ready.py" "$ROOT/run/acquire.sock" \
            --timeout "$READY_TIMEOUT" --expect-tool acquire_generation \
            >"$WORK/readiness.acquire.json"
        systemctl is-active --quiet x-data-mcp-data
        systemctl is-active --quiet x-data-mcp-acquire
        [[ "$(readlink "$ROOT/current")" == "$NEW" ]]
        end_step "data+acquire ready via probe; is-active data,acquire; current -> $NEW"
    else
        echo "DRY: python3 $SCRIPTS_DIR/wait_service_ready.py $ROOT/run/mcp.sock --timeout $READY_TIMEOUT --expect-tool plan_market_update"
        echo "DRY: python3 $SCRIPTS_DIR/wait_service_ready.py $ROOT/run/acquire.sock --timeout $READY_TIMEOUT --expect-tool acquire_generation"
        end_step "dry-run: services not started"
    fi
}

# --- S7. operator next steps -------------------------------------------------
s7_next_steps() {
    begin_step S7 "next steps (operator, read-only via MCP, BEFORE any write task)"
    echo "   capability revisions generic-data-v7/x-data-acquire-v1; tools 23/10;"
    echo "   sync_status idle and active generation unchanged; pending_roles present;"
    echo "   head counts equal baseline; sample query_data reads unchanged."
    say "ROLLBACK plan on any mismatch (S2 failure aborts before touching prod; S6/S7 failure):"
    echo "   systemctl stop both services; mv -T symlink back to $PREV; restart data then acquire;"
    echo "   re-run S7 checks (a)-(d)(f). The database is never modified, so no DB restore is needed."
    end_step "operator checklist printed"
}

p_preconditions
s1_fetch_verify
s2_private_smoke
s3_install
s4_stop
s5_switch
s6_start
s7_next_steps

write_report PASS "not_needed"
if [[ $EXECUTE == 1 ]]; then
    echo "SWITCHED $TAG ($S); current -> $NEW"
else
    echo "DRY-RUN complete; nothing was changed under $ROOT"
fi
