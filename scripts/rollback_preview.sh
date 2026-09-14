#!/usr/bin/env bash
# Roll back a preview deployment to a specific prior state. Fail-closed:
# the restored pair (previous release dir + DB backup) must be bound
# together, never guessed. Two interfaces:
#   --from-report=<deployment.json>   take backup + previous_release from the
#                                     deployment report written by the run
#                                     being rolled back;
#   --backup=<sqlite> --previous-release=<sha-or-dir>   manual, both required.
# Every mutation step failure writes reports/rollback-*.json status=FAIL
# with failed_at and exits non-zero; only full verification writes PASS.
set -euo pipefail
DEPLOY_ROOT=${XDM_DEPLOY_ROOT:-/opt/x-data-mcp-preview}
DB="$DEPLOY_ROOT/state/preview.sqlite"
SERVICE=x-data-mcp-data.service
TUNNEL=x-data-mcp-tunnel.service
REPORTS="$DEPLOY_ROOT/reports"
BACKUP=""
PREV=""
FROM_REPORT=""
for arg in "$@"; do
    case "$arg" in
        --backup=*) BACKUP="${arg#--backup=}" ;;
        --previous-release=*) PREV="${arg#--previous-release=}" ;;
        --from-report=*) FROM_REPORT="${arg#--from-report=}" ;;
        *) echo "usage: rollback_preview.sh --from-report=<deployment.json> | --backup=<sqlite> --previous-release=<sha-or-dir>" >&2; exit 2 ;;
    esac
done
if [[ -n "$FROM_REPORT" ]]; then
    read -r BACKUP PREV < <(python3 -c 'import json,sys; r=json.load(open(sys.argv[1])); print(r.get("backup") or "", r.get("previous_release") or "")' "$FROM_REPORT")
fi
# --- input validation: all errors here exit before any mutation --------------
[[ -n "$BACKUP" && -f "$BACKUP" ]] || { echo "no explicit backup; refusing to guess" >&2; exit 1; }
[[ -n "$PREV" ]] || { echo "no previous release bound to this backup; refusing" >&2; exit 1; }
# Normalize previous release to a directory inside releases/.
[[ "$PREV" != /* ]] && PREV="$DEPLOY_ROOT/releases/$PREV"
[[ -d "$PREV" ]] || { echo "previous release dir missing: $PREV" >&2; exit 1; }
[[ -x "$PREV/bin/task_sqlite" && -x "$PREV/bin/mcp_connect" ]] \
    || { echo "previous release $PREV lacks executable bin/task_sqlite or bin/mcp_connect" >&2; exit 1; }

# --- mutation phase: failures write a FAIL report, never bare-exit -----------
rollback_step=init
rfail() {
    python3 - "$BACKUP" "$PREV" "$1" "$2" "$REPORTS" <<'PY' || true
import json, pathlib, sys
backup, prev, step, msg, reports = sys.argv[1:]
rep = {"status": "FAIL", "failed_at": step, "error": msg,
       "restored_backup": backup, "previous_release": prev,
       "production": "NOT_PRODUCTION_READY"}
pathlib.Path(f"{reports}/rollback-{pathlib.Path(backup).stem}.json").write_text(
    json.dumps(rep, indent=2))
PY
    echo "rollback FAILED_AT_$1: $2" >&2
    exit 1
}
rollback_step=stop_tunnel;   systemctl stop "$TUNNEL"  || rfail "$rollback_step" "systemctl stop $TUNNEL"
rollback_step=stop_service;  systemctl stop "$SERVICE" || rfail "$rollback_step" "systemctl stop $SERVICE"
rollback_step=restore_db
cp "$BACKUP" "$DB"                            || rfail "$rollback_step" "cp backup -> $DB"
rm -f "$DB-shm" "$DB-wal"  # restored snapshot pairs with no WAL journal
rollback_step=switch_current
ln -sfn "$PREV" "$DEPLOY_ROOT/current"        || rfail "$rollback_step" "current -> $PREV"
rollback_step=start_service; systemctl start "$SERVICE" || rfail "$rollback_step" "systemctl start $SERVICE"
rollback_step=restart_tunnel; systemctl restart "$TUNNEL" || rfail "$rollback_step" "systemctl restart $TUNNEL"

rollback_step=revalidate
python3 - "$BACKUP" "$PREV" "$REPORTS" "$DEPLOY_ROOT" "$SERVICE" "$TUNNEL" <<'PY' \
    || rfail "$rollback_step" "post-rollback verification"
import hashlib, json, pathlib, shutil, sqlite3, subprocess, sys, tempfile
backup, prev, reports, root, svc, tun = sys.argv[1:]
db_path = f"{root}/state/preview.sqlite"
db = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
schema = db.execute("PRAGMA user_version").fetchone()[0]
integrity = db.execute("PRAGMA integrity_check").fetchone()[0]
current = str(pathlib.Path(f"{root}/current").resolve())
def active(n):
    return subprocess.run(["systemctl","is-active",n],capture_output=True,text=True).stdout.strip()
# Schema acceptance: run the restored release's own current-schema check on a
# COPY (task_sqlite upgrade reports "task schema N ready" after identity+FK
# checks; on a copy it cannot mutate the live DB).
with tempfile.TemporaryDirectory() as td:
    cp = f"{td}/check.sqlite"; shutil.copy(db_path, cp)
    for ext in ("-shm","-wal"):
        p = db_path + ext
        if pathlib.Path(p).exists(): shutil.copy(p, cp + ext)
    r = subprocess.run([f"{prev}/bin/task_sqlite","upgrade",cp],
                       capture_output=True, text=True)
    ok_schema = f"task schema {schema} ready" in r.stdout
# Tunnel cgroup proxy must be the restored release's mcp_connect.
expected_proxy = hashlib.sha256(pathlib.Path(f"{prev}/bin/mcp_connect").read_bytes()).hexdigest()
cg = subprocess.run(["systemctl","show",tun,"-p","ControlGroup"],capture_output=True,text=True).stdout.strip().split("=",1)[-1]
procs, cg_error = [], None
try:
    for pid in pathlib.Path(f"/sys/fs/cgroup{cg}/cgroup.procs").read_text().split():
        exe = pathlib.Path(f"/proc/{pid}/exe").resolve()
        if exe.name == "mcp_connect":
            procs.append({"pid": int(pid), "exe": str(exe),
                          "sha256": hashlib.sha256(exe.read_bytes()).hexdigest()})
except Exception as e:
    cg_error = str(e)
checks = {"integrity": integrity == "ok",
          "current_points_at_previous_release": current == str(pathlib.Path(prev).resolve()),
          "schema_accepted_by_restored_release": ok_schema,
          "service_active": active(svc) == "active",
          "tunnel_active": active(tun) == "active",
          "tunnel_cgroup_readable": cg_error is None,
          "tunnel_proxy_matches_release": bool(procs) and all(p["sha256"] == expected_proxy for p in procs)}
rep = {"status": "PASS" if all(checks.values()) else "FAIL",
       "restored_backup": backup, "previous_release": prev,
       "schema": schema, "current": current,
       "tunnel_cgroup": cg, "tunnel_cgroup_mcp_connect": procs,
       "schema_check_stdout": r.stdout.strip(), "checks": checks,
       "production": "NOT_PRODUCTION_READY"}
out = f"{reports}/rollback-{pathlib.Path(backup).stem}.json"
pathlib.Path(out).write_text(json.dumps(rep, indent=2))
print(json.dumps({"status": rep["status"], "report": out}))
sys.exit(0 if rep["status"] == "PASS" else 1)
PY
