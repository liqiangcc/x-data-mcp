#!/usr/bin/env bash
# Rollback: point `current` back at a previous release dir, restore the newest
# matching database backup, restart the service, and record a rollback report.
# Usage: rollback_preview.sh <previous-release-sha-or-dir> [backup-file]
set -euo pipefail
DEPLOY_ROOT=/opt/x-data-mcp-preview
DB="$DEPLOY_ROOT/state/preview.sqlite"
SERVICE=x-data-mcp-data.service
REPORTS="$DEPLOY_ROOT/reports"
[[ $# -ge 1 ]] || { echo "usage: rollback_preview.sh <previous-release-sha> [backup-file]" >&2; exit 2; }
TARGET="$DEPLOY_ROOT/releases/$1"
[[ -d "$TARGET/bin" ]] || { echo "release dir not found: $TARGET" >&2; exit 1; }
BACKUP="${2:-}"
if [[ -z "$BACKUP" ]]; then
    BACKUP=$(ls -t "$DEPLOY_ROOT"/state/backup-*.sqlite 2>/dev/null | head -1) \
        || { echo "no backup found" >&2; exit 1; }
fi
[[ -f "$BACKUP" ]] || { echo "backup not found: $BACKUP" >&2; exit 1; }
TS=$(date -u +%Y%m%dT%H%M%SZ)
systemctl stop "$SERVICE"
cp "$DB" "$DEPLOY_ROOT/state/backup-rollback-${TS}.sqlite" || true
cp "$BACKUP" "$DB"
ln -sfn "$TARGET" "$DEPLOY_ROOT/current"
systemctl start "$SERVICE"
systemctl is-active --quiet "$SERVICE" || { echo "$SERVICE failed to start" >&2; exit 1; }
python3 - "$1" "$BACKUP" "$TS" "$REPORTS" <<'PY'
import json, pathlib, sys
target, backup, ts, reports = sys.argv[1:]
rep = {"status": "ROLLED_BACK", "target_release": target, "restored_backup": backup,
       "at_utc": ts, "production": "NOT_PRODUCTION_READY"}
pathlib.Path(f"{reports}/rollback-{ts}.json").write_text(json.dumps(rep, indent=2))
print(json.dumps(rep, indent=2))
PY
