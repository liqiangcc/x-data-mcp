#!/usr/bin/env bash
# Roll back a preview deployment to a specific prior state. Fail-closed:
# the backup must be named explicitly (--backup <path>) or taken from the
# deployment report produced by this deployment's deploy_preview.sh run
# (--from-report <deployment.json>); it never guesses "latest file in dir".
set -euo pipefail
DEPLOY_ROOT=/opt/x-data-mcp-preview
DB="$DEPLOY_ROOT/state/preview.sqlite"
SERVICE=x-data-mcp-data.service
TUNNEL=x-data-mcp-tunnel.service
REPORTS="$DEPLOY_ROOT/reports"
BACKUP=""
FROM_REPORT=""
for arg in "$@"; do
    case "$arg" in
        --backup=*) BACKUP="${arg#--backup=}" ;;
        --from-report=*) FROM_REPORT="${arg#--from-report=}" ;;
        *) echo "usage: rollback_preview.sh --backup=<sqlite> | --from-report=<deployment.json>" >&2; exit 2 ;;
    esac
done
if [[ -n "$FROM_REPORT" ]]; then
    BACKUP=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["backup"] or "")' "$FROM_REPORT")
    [[ -n "$BACKUP" ]] || { echo "report $FROM_REPORT has no backup path" >&2; exit 1; }
fi
[[ -n "$BACKUP" && -f "$BACKUP" ]] || { echo "no explicit backup; refusing to guess" >&2; exit 1; }

systemctl stop "$TUNNEL" 2>/dev/null || true
systemctl stop "$SERVICE"
cp "$BACKUP" "$DB"
rm -f "$DB-shm" "$DB-wal"  # restored snapshot pairs with no WAL journal
# Point current at the release that owns this schema state if recorded.
if [[ -n "$FROM_REPORT" ]]; then
    prev=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("previous_release") or "")' "$FROM_REPORT")
    [[ -n "$prev" && -d "$prev" ]] && ln -sfn "$prev" "$DEPLOY_ROOT/current"
fi
systemctl start "$SERVICE"
systemctl try-restart "$TUNNEL" 2>/dev/null || true

python3 - "$BACKUP" "$REPORTS" "$DEPLOY_ROOT" "$SERVICE" "$TUNNEL" <<'PY'
import json, sqlite3, sys, subprocess, pathlib
backup, reports, root, svc, tun = sys.argv[1:]
db = sqlite3.connect(f"file:{root}/state/preview.sqlite?mode=ro", uri=True)
schema = db.execute("PRAGMA user_version").fetchone()[0]
integrity = db.execute("PRAGMA integrity_check").fetchone()[0]
current = str(pathlib.Path(f"{root}/current").resolve())
def active(n):
    return subprocess.run(["systemctl","is-active",n],capture_output=True,text=True).stdout.strip()
checks = {"integrity": integrity == "ok",
          "service_active": active(svc) == "active",
          "tunnel_active": active(tun) == "active"}
rep = {"status": "PASS" if all(checks.values()) else "FAIL",
       "restored_backup": backup, "schema": schema, "current": current,
       "checks": checks, "production": "NOT_PRODUCTION_READY"}
pathlib.Path(f"{reports}/rollback-{pathlib.Path(backup).name}.json".replace(".sqlite","")) \
    .write_text(json.dumps(rep, indent=2))
print(json.dumps(rep))
sys.exit(0 if rep["status"] == "PASS" else 1)
PY
