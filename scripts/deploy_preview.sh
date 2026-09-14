#!/usr/bin/env bash
# Deploy a tagged preview release to /opt/x-data-mcp-preview and produce the
# five evidence reports. Fail-closed:
#   * any unmet precondition exits non-zero before touching /opt;
#   * once the mutating phase (stop service / upgrade / switch) starts, any
#     later failure auto-restores the previous current symlink, the consistent
#     DB snapshot and the old service — and records the rollback outcome;
#   * every failure writes reports/<tag>-deployment.json with status
#     FAILED_AT_<step> instead of exiting silently.
#
# Usage: deploy_preview.sh <tag> [--dry-run] [--force-git-transport]
#   --dry-run:             resolve release + record before-state + print plan;
#                        never downloads, never touches /opt.
#   --force-git-transport: skip gh release download, fetch the package via
#                        refs/dist/<tag> (git smart-HTTP bypass for a throttled
#                        object CDN). Either way the tarball must match the
#                        release's .sha256 asset digest.
set -euo pipefail

DEPLOY_ROOT=${XDM_DEPLOY_ROOT:-/opt/x-data-mcp-preview}
DB="$DEPLOY_ROOT/state/preview.sqlite"
SOCK="$DEPLOY_ROOT/run/mcp.sock"
SERVICE=x-data-mcp-data.service
TUNNEL=x-data-mcp-tunnel.service
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
[[ "$TAG" =~ ^v[0-9] ]] || { echo "usage: deploy_preview.sh <tag> [--dry-run] [--force-git-transport]" >&2; exit 2; }
# Historical continuity: v0.2.0-preview.10 -> preview10-*.json
TAG_SHORT=$(sed -E 's/^v[0-9.]+-//; s/\.//g' <<<"$TAG")

step=init
SHA=""
RELEASE_DIR=""
BACKUP=""
BACKUP_SHA=""
CONF=""
schema_before=""
schema_after=""
markers_before=0
markers_after=0
counts_before="{}"
current_before=""
MUTATING=0
INSTALLED_NEW=0
WORK=$(mktemp -d "/tmp/deploy-${TAG}-XXXXXX")
trap 'rm -rf "$WORK"' EXIT

report() {  # report <status> [rollback_outcome]
    python3 - "$TAG" "$SHA" "$1" "$step" "$current_before" "$BACKUP" "$BACKUP_SHA" \
        "$schema_before" "$schema_after" "$markers_before" "$markers_after" \
        "$counts_before" "${2:-}" "$REPORTS" "$TAG_SHORT" <<'PY' || true
import json, pathlib, sys
(tag, sha, status, step, prev, backup, backup_sha, sb, sa, mb, ma,
 cb, rollback, reports, short) = sys.argv[1:]
before = {"schema": int(sb)} if sb else {}
try:
    before.update(json.loads(cb))
except Exception:
    pass
dep = {"status": status, "code_sha": sha or None, "release": tag,
       "failed_at": step if status.startswith("FAILED_AT_") else None,
       "previous_release": prev or None, "backup": backup or None,
       "backup_sha256": backup_sha or None,
       "before": before,
       "after": {"schema": int(sa), "task_item_execution_starts": int(ma)} if sa else {},
       "migration_evidence": {"marker_rows_before": int(mb),
                              "marker_rows_after": int(ma)},
       "rollback_outcome": rollback or None,
       "consumer": "NOT_RUN", "live_source": "NOT_RUN",
       "production": "NOT_PRODUCTION_READY"}
pathlib.Path(f"{reports}/{short}-deployment.json").write_text(
    json.dumps(dep, indent=2))
PY
}

rollback() {  # best-effort restore of pre-switch state; returns outcome string
    local outcome="not_needed"
    if [[ "$MUTATING" == "1" ]]; then
        outcome="failed"
        systemctl stop "$TUNNEL" 2>/dev/null || true
        systemctl stop "$SERVICE" 2>/dev/null || true
        if [[ -n "$current_before" ]]; then
            ln -sfn "$current_before" "$DEPLOY_ROOT/current" || true
        fi
        if [[ -n "$BACKUP" && -f "$BACKUP" ]]; then
            cp "$BACKUP" "$DB" || true
            rm -f "$DB-shm" "$DB-wal"  # snapshot file pairs with no WAL journal
        fi
        if systemctl start "$SERVICE" 2>/dev/null && systemctl is-active --quiet "$SERVICE"; then
            if systemctl try-restart "$TUNNEL" 2>/dev/null && systemctl is-active --quiet "$TUNNEL"; then
                outcome="restored_previous_current_and_db"
            else
                outcome="failed_tunnel_inactive_after_restore"
            fi
        else
            outcome="failed_service_not_restored"
        fi
    fi
    echo "$outcome"
}

fail() {
    local outcome
    outcome=$(rollback)
    # Pre-mutation install cleanup: remove a release dir this run created iff
    # current does not point at it. A pre-existing release dir is never removed.
    if [[ "$DRY_RUN" != "1" && "$INSTALLED_NEW" == "1" && -n "$RELEASE_DIR" \
          && "$(readlink "$DEPLOY_ROOT/current" 2>/dev/null || true)" != "$RELEASE_DIR" ]]; then
        rm -rf "$RELEASE_DIR" || true
    fi
    # --dry-run is strictly read-only: no report files, no state changes.
    [[ "$DRY_RUN" == "1" ]] || report "FAILED_AT_$step" "$outcome"
    echo "FAILED_AT_$step: $1 (rollback: $outcome)" >&2
    exit 1
}
now_utc() { date -u +%Y%m%dT%H%M%SZ; }
db_scalar() { python3 - "$DB" "$1" <<'PY'
import sqlite3, sys
c = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True)
row = c.execute(sys.argv[2]).fetchone()
print(0 if row is None else row[0])
PY
}
db_counts_json() { python3 - "$DB" <<'PY'
import sqlite3, sys, json
c = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True)
tables = ["tasks", "requests", "data_versions", "catalog_versions", "scope_occupancy"]
out = {t: c.execute(f"SELECT count(*) FROM {t}").fetchone()[0] for t in tables}
if c.execute("SELECT count(*) FROM sqlite_master WHERE name='task_item_execution_starts'").fetchone()[0]:
    out["task_item_execution_starts"] = c.execute("SELECT count(*) FROM task_item_execution_starts").fetchone()[0]
print(json.dumps(out))
PY
}

# --- 1. resolve release: tag -> target commit + asset digests ---------------
step=resolve_release
release_json=$(gh release view "$TAG" --json targetCommitish,assets) || fail "release $TAG missing"
SHA=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["targetCommitish"])' <<<"$release_json")
TARBALL="x-data-mcp-${TAG#v}-linux-x86_64.tar.gz"
asset_digest=$(TARBALL="$TARBALL" python3 -c '
import json, os, sys
a = {x["name"]: x["digest"].removeprefix("sha256:") for x in json.load(sys.stdin)["assets"]}
need = [os.environ["TARBALL"], os.environ["TARBALL"] + ".sha256", "smoke-report.json"]
missing = [n for n in need if n not in a]
if missing:
    sys.exit("missing release assets: %s" % missing)
print(a[os.environ["TARBALL"]])
' <<<"$release_json") || fail "release assets incomplete"
RELEASE_DIR="$DEPLOY_ROOT/releases/$SHA"

# --- 2. before state (read-only) ---------------------------------------------
step=before_state
current_before=$(readlink "$DEPLOY_ROOT/current" || true)
schema_before=$(db_scalar "PRAGMA user_version")
table_exists=$(db_scalar "SELECT count(*) FROM sqlite_master WHERE name='task_item_execution_starts'")
markers_before=$([[ "$table_exists" == "1" ]] && db_scalar "SELECT count(*) FROM task_item_execution_starts" || echo 0)
counts_before=$(db_counts_json)
svc_before=$(systemctl show "$SERVICE" "$TUNNEL" -p Id,ActiveState,NRestarts 2>/dev/null || true)

if [[ "$DRY_RUN" == "1" ]]; then
    cat <<EOF
DRY-RUN deploy $TAG
  release target:  $SHA
  tarball asset:   $TARBALL sha256:$asset_digest
  release dir:     $RELEASE_DIR
  current ->       $current_before
  db user_version: $schema_before  markers: $markers_before
  db counts:       $counts_before
  services:
$(echo "$svc_before" | sed 's/^/    /')
  plan: fetch tarball -> verify sha256 -> stage+rename release dir -> backup
        (consistent sqlite3 snapshot + config) -> stop $SERVICE -> explicit
        task_sqlite upgrade + schema/marker checks -> switch current -> start
        $SERVICE + restart $TUNNEL -> smoke (package + live stdio + tunnel)
        -> 5 reports; any failure after mutation auto-restores prior state
EOF
    exit 0
fi

# --- 3. fetch + fully verify package in $WORK before touching /opt -----------
step=fetch_package
fetch_git() {
    # Zero-disturbance bypass: temp bare repo inside $WORK, fetch only the
    # dist ref; the shared checkout's refs are never modified.
    remote=$(git -C "$REPO" remote get-url origin) || fail "origin url"
    git init --bare -q "$WORK/repo"
    git -C "$WORK/repo" fetch --depth 1 "$remote" "refs/dist/$TAG" \
        || fail "refs/dist/$TAG not available"
    git -C "$WORK/repo" archive FETCH_HEAD | tar -x -C "$WORK" \
        || fail "refs/dist/$TAG unpack"
}
fetch_gh() {
    (cd "$WORK" && timeout 60 gh release download "$TAG" -p "$TARBALL" -p "$TARBALL.sha256" -p smoke-report.json) || return 1
}
if [[ "$FORCE_GIT" == "1" ]]; then
    fetch_git
else
    fetch_gh || { echo "gh release download slow/failed; falling back to refs/dist/$TAG" >&2; fetch_git; }
fi
[[ -f "$WORK/$TARBALL" && -f "$WORK/$TARBALL.sha256" ]] || fail "tarball or .sha256 missing after fetch"
(cd "$WORK" && sha256sum -c "$TARBALL.sha256" >/dev/null) || fail "tarball sha256 mismatch"
[[ "$(sha256sum "$WORK/$TARBALL" | cut -d' ' -f1)" == "$asset_digest" ]] || fail "tarball sha256 differs from release asset digest"
# Release smoke report must be PASS and bound to this exact SHA/version.
python3 - "$WORK/smoke-report.json" "$SHA" "$TAG" <<'PY' || fail "smoke-report binding"
import json, sys
r = json.load(open(sys.argv[1]))
assert r["status"] == "PASS", r["status"]
assert sys.argv[2] in r["version"], r["version"]
assert sys.argv[3].lstrip("v") in r["version"], r["version"]
PY
# Fully verify inside $WORK: unpack to a staging dir first, rename atomically.
mkdir -p "$WORK/stage"
tar -xzf "$WORK/$TARBALL" -C "$WORK/stage" --strip-components=1 || fail "unpack"
(cd "$WORK/stage" && sha256sum --check SHA256SUMS >/dev/null) || fail "package SHA256SUMS"
"$WORK/stage/bin/mcp_data" --version | grep -q "$SHA" || fail "binary version does not embed $SHA"
step=install_release
if [[ ! -d "$RELEASE_DIR" ]]; then
    mv "$WORK/stage" "$RELEASE_DIR" || fail "install release dir"
    INSTALLED_NEW=1
fi

# --- 4. consistent backup (sqlite3 backup API, not cp of db+wal/shm) ---------
step=backup
TS=$(now_utc)
BACKUP="$DEPLOY_ROOT/state/backup-${TAG}-${TS}.sqlite"
python3 - "$DB" "$BACKUP" <<'PY' || fail "consistent backup"
import sqlite3, sys
src = sqlite3.connect(sys.argv[1])
dst = sqlite3.connect(sys.argv[2])
src.backup(dst)
dst.close(); src.close()
PY
BACKUP_SHA=$(sha256sum "$BACKUP" | cut -d' ' -f1)
CONF="$DEPLOY_ROOT/state/config-before-${TAG}-${TS}"
mkdir -p "$CONF"
cp "$DEPLOY_ROOT/mcp.json" "$CONF/0-mcp.json" 2>/dev/null || true
systemctl cat "$SERVICE" >"$CONF/1-$SERVICE" || fail "service unit snapshot"
systemctl cat "$TUNNEL" >"$CONF/2-$TUNNEL" || fail "tunnel unit snapshot"
cat /etc/systemd/system/$TUNNEL.d/*.conf >"$CONF/3-data-service.conf" 2>/dev/null || true
cp "$DEPLOY_ROOT/wait-data-service.py" "$CONF/4-wait-data-service.py" 2>/dev/null || true
cp "$WORK/smoke-report.json" "$REPORTS/${TAG_SHORT}-smoke.json" || fail "smoke report"

# --- 5. stop + explicit schema upgrade + full verification -------------------
step=schema_upgrade
MUTATING=1
systemctl stop "$SERVICE" || fail "stop $SERVICE"
upgrade_out=$("$RELEASE_DIR/bin/task_sqlite" upgrade "$DB") || fail "task_sqlite upgrade"
# The binary prints "task schema N ready" only after check_schema validates
# migration identity; N is this release's SCHEMA_VERSION. Anything else fails.
expected_schema=$(sed -nE 's/.*task schema ([0-9]+) ready.*/\1/p' <<<"$upgrade_out")
[[ -n "$expected_schema" ]] || fail "upgrade did not report target schema"
schema_after=$(db_scalar "PRAGMA user_version")
[[ "$schema_after" == "$expected_schema" ]] || fail "user_version $schema_after != release schema $expected_schema"
[[ "$schema_after" -ge "$schema_before" ]] || fail "user_version regressed"
table_after=$(db_scalar "SELECT count(*) FROM sqlite_master WHERE name='task_item_execution_starts'")
if [[ "$expected_schema" -ge 10 ]]; then
    [[ "$table_after" == "1" ]] || fail "task_item_execution_starts missing after upgrade"
    markers_after=$(db_scalar "SELECT count(*) FROM task_item_execution_starts")
    trig=$(db_scalar "SELECT count(*) FROM sqlite_master WHERE type='trigger' AND name LIKE 'task_item_execution_starts_no_%'")
    [[ "$trig" == "2" ]] || fail "immutability triggers missing"
    [[ "$markers_after" == "$markers_before" ]] || fail "execution-start markers fabricated during upgrade"
fi

# --- 6. atomic switch + tunnel refresh ---------------------------------------
step=switch
ln -sfn "$RELEASE_DIR" "$DEPLOY_ROOT/current" || fail "switch current"
systemctl start "$SERVICE" || fail "start $SERVICE"
for _ in $(seq 1 100); do [[ -S "$SOCK" ]] && break; sleep 0.1; done
[[ -S "$SOCK" ]] || fail "service socket did not appear"
systemctl is-active --quiet "$SERVICE" || fail "$SERVICE not active"
# Refresh the tunnel so its spawned mcp_connect comes from the new release.
# MUTATING stays 1: a smoke/report failure after the switch still restores
# the previous current symlink + DB snapshot + old service.
systemctl try-restart "$TUNNEL" || true

# --- 7. smoke: live stdio + tunnel observation (asserted, not assumed) -------
step=smoke
EXPECTED_TOOLS='["start_collection","get_task","run_worker_once","cancel_task","retry_task","get_request","inspect_verification_task","verify_publication_result","verify_stopped_writes","update_data","backfill_data","compute_metrics","query_data","list_data_versions","get_data_availability","list_tasks","get_task_progress","get_data_capabilities"]'
python3 - "$SOCK" "$RELEASE_DIR" "$REPORTS/${TAG_SHORT}-stdio.json" "$EXPECTED_TOOLS" <<'PYEOF' || fail "stdio smoke"
import json, subprocess, socket, time, select, hashlib, sqlite3, sys, pathlib
sock, rel, out, expected_json = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
with socket.socket(socket.AF_UNIX) as s: s.connect(sock)
client = subprocess.Popen([rel+"/bin/mcp_connect", sock], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
def rpc(m,p,i=1):
    client.stdin.write(json.dumps(dict(jsonrpc="2.0",id=i,method=m,params=p))+"\n"); client.stdin.flush()
    assert select.select([client.stdout],[],[],10)[0]
    r=json.loads(client.stdout.readline()); assert r["id"]==i and "error" not in r, r; return r["result"]
def call(n,a,i):
    r=rpc("tools/call",dict(name=n,arguments=a),i); assert not r["isError"],r; return r["structuredContent"]
try:
    init=rpc("initialize",{})
    tools=rpc("tools/list",{})["tools"]
    cap=call("get_data_capabilities",{},2)
    batch={"kind":"batch","entities":[{"exchange":"sh","code":"600006"}]}
    single={"kind":"single","entity":{"exchange":"sh","code":"600006"}}
    catsel={"kind":"catalog","scope":"current_sh_sz_a"}
    latest={"kind":"latest_per_entity"}
    i=10
    empty_reads={}
    for ds,f in [("market.daily",{"kind":"daily","start_date":"2024-01-01","end_date":"2024-12-31"}),
                 ("market.yearly",{"kind":"yearly","start_year":2020,"end_year":2024}),
                 ("securities.catalog",{"kind":"catalog"})]:
        sel = catsel if ds=="securities.catalog" else batch
        empty_reads[f"{ds}/list_data_versions"]=call("list_data_versions",{"dataset":ds,"contract_version":"v1","selection":sel},i); i+=1
        sel2 = catsel if ds=="securities.catalog" else single
        empty_reads[f"{ds}/get_data_availability"]=call("get_data_availability",{"dataset":ds,"contract_version":"v1","selection":sel2,"filter":f,"version":latest},i); i+=1
    catalog=call("query_data",{"dataset":"securities.catalog","contract_version":"v1","selection":catsel,"filter":{"kind":"catalog"},"version":latest},i); i+=1
    tasks=call("list_tasks",{},i); i+=1
    def capslice(name):
        return {"capability_revision":cap["capability_revision"],
                "items":[x for x in cap["items"] if x["tool_name"]==name]}
    t0=time.monotonic()
    time.sleep(33)  # >30s idle regression (#112): same proxy stdin must stay usable
    assert client.poll() is None
    cap2=call("get_data_capabilities",{},i)
    idle={"status":"PASS","elapsed_seconds":time.monotonic()-t0,"stdin_remained_open":True,"actual_tool":"get_data_capabilities"}
    client.stdin.close(); assert client.wait(timeout=10)==0
finally:
    if client.poll() is None: client.kill()
names=[t["name"] for t in tools]
expected=json.loads(expected_json)
assert sorted(names)==sorted(expected), f"public tool set drifted: {sorted(set(names)^set(expected))}"
assert not any("default" in n and "select" in n for n in names), "default-selector leaked to wire"
assert not cap["production_ready"] and not cap2["production_ready"]
db=sqlite3.connect("file:/opt/x-data-mcp-preview/state/preview.sqlite?mode=ro",uri=True)
counts={t:db.execute(f"SELECT count(*) FROM {t}").fetchone()[0] for t in
        ["tasks","requests","data_versions","catalog_versions","scope_occupancy"]}
if db.execute("SELECT count(*) FROM sqlite_master WHERE name='task_item_execution_starts'").fetchone()[0]:
    counts["task_item_execution_starts"]=db.execute("SELECT count(*) FROM task_item_execution_starts").fetchone()[0]
rep={"status":"PASS","version":init["serverInfo"]["version"],
 "service_binary_sha256":hashlib.sha256(pathlib.Path(rel+"/bin/data_service").read_bytes()).hexdigest(),
 "proxy_binary_sha256":hashlib.sha256(pathlib.Path(rel+"/bin/mcp_connect").read_bytes()).hexdigest(),
 "normal_db_counts":counts,"normal_db_schema":db.execute("PRAGMA user_version").fetchone()[0],
 "stdio":{"initialize":init,"tools":names,"capabilities":cap,
   "catalog":catalog,"empty_reads":empty_reads,"tasks":tasks,
   "inspect_verification_task":capslice("inspect_verification_task"),
   "verify_publication_result":capslice("verify_publication_result"),
   "stopping":capslice("verify_stopped_writes"),
   "yearly_update":capslice("update_data"),
   "same_proxy_idle":idle},
 "chatgpt_consumer":"NOT_RUN","live_source":"NOT_RUN","production":"NOT_PRODUCTION_READY"}
pathlib.Path(out).write_text(json.dumps(rep,indent=2))
PYEOF
python3 - "$SHA" "$RELEASE_DIR" "$REPORTS/${TAG_SHORT}-tunnel-observation.json" <<'PYEOF' || fail "tunnel observation"
import json, subprocess, hashlib, pathlib, sys, urllib.request
sha, rel, out = sys.argv[1], sys.argv[2], sys.argv[3]
def svc(n):
    o=subprocess.run(["systemctl","show",n,"-p","MainPID,NRestarts,ActiveState,SubState,ControlGroup,ActiveEnterTimestampMonotonic"],capture_output=True,text=True).stdout
    return dict(l.split("=",1) for l in o.strip().splitlines())
def exe_sha(pid):
    return hashlib.sha256(pathlib.Path(f"/proc/{pid}/exe").resolve().read_bytes()).hexdigest()
expected_data=hashlib.sha256(pathlib.Path(rel+"/bin/data_service").read_bytes()).hexdigest()
expected_proxy=hashlib.sha256(pathlib.Path(rel+"/bin/mcp_connect").read_bytes()).hexdigest()
services={n:svc(n) for n in ["x-data-mcp-data.service","x-data-mcp-tunnel.service"]}
# Cgroup-specific proxy binding: enumerate only the tunnel service's cgroup.
# Manual/client mcp_connect processes elsewhere must not influence the check.
tunnel_cg=services["x-data-mcp-tunnel.service"].get("ControlGroup","")
cg_procs_path=f"/sys/fs/cgroup{tunnel_cg}/cgroup.procs" if tunnel_cg else ""
procs=[]
cg_error=None
try:
    for pid in pathlib.Path(cg_procs_path).read_text().split():
        exe=pathlib.Path(f"/proc/{pid}/exe").resolve()
        if exe.name=="mcp_connect":
            procs.append({"pid":int(pid),"binary":exe.name,"exe":str(exe),"sha256":exe_sha(pid)})
except Exception as e:
    cg_error=str(e)
data_sha=exe_sha(services["x-data-mcp-data.service"]["MainPID"])
health={"locator":{"kind":"url_file","url_file":"/root/.local/state/tunnel-client/health/x-data-mcp.url"}}
checks={}
try:
    base=pathlib.Path(health["locator"]["url_file"]).read_text().strip()
    health["resolved_base_url"]=base
    for ep in ["healthz","readyz","metrics"]:
        try:
            with urllib.request.urlopen(f"{base}/{ep}",timeout=5) as r:
                health[ep]={"url":f"{base}/{ep}","ok":r.status==200,"status":r.status,"body":r.read().decode()[:200]}
        except Exception as e:
            health[ep]={"url":f"{base}/{ep}","ok":False,"error":str(e)}
except Exception as e:
    health["error"]=str(e)
checks["services_active"]=all(s["ActiveState"]=="active" for s in services.values())
checks["data_process_matches_release"]=data_sha==expected_data
# >=1 mcp_connect inside the tunnel cgroup, all matching the new release.
checks["tunnel_cgroup_readable"]=cg_error is None
checks["tunnel_proxy_matches_release"]=bool(procs) and all(p["sha256"]==expected_proxy for p in procs)
checks["healthz"]=health.get("healthz",{}).get("ok") is True
checks["readyz"]=health.get("readyz",{}).get("ok") is True
checks["control_plane_poll"]=health.get("metrics",{}).get("ok") is True
status="PASS" if all(checks.values()) else "FAIL"
rep={"status":status,"code_sha":sha,"services":services,
 "tunnel_cgroup":tunnel_cg,"cgroup_procs_path":cg_procs_path,
 "cgroup_error":cg_error,"tunnel_cgroup_mcp_connect":procs,
 "data_process_sha256":data_sha,
 "expected_data_sha256":expected_data,"expected_proxy_sha256":expected_proxy,
 "assertions":checks,"health":health,
 "consumer":"NOT_RUN","live_source":"NOT_RUN","production":"NOT_PRODUCTION_READY"}
pathlib.Path(out).write_text(json.dumps(rep,indent=2))
sys.exit(0 if status=="PASS" else 1)
PYEOF

# --- 8. reports -----------------------------------------------------------------
step=reports
counts_after=$(db_counts_json)
python3 - "$TAG" "$SHA" "$current_before" "$BACKUP" "$BACKUP_SHA" "$CONF" "$schema_before" "$schema_after" "$markers_before" "$markers_after" "$counts_before" "$counts_after" "$REPORTS" "$TAG_SHORT" <<'PY' || fail "reports"
import json, pathlib, sys
tag,sha,prev,backup,backup_sha,conf,sb,sa,mb,ma,cb,ca,reports,short = sys.argv[1:]
dep={"status":"DEPLOYED","code_sha":sha,"release":tag,
 "previous_release":prev,"backup":backup,"backup_sha256":backup_sha,"config_backup":conf,
 "schema_migration":f"EXPLICIT_UPGRADE_{sb}_TO_{sa}_VIA task_sqlite upgrade" if sb!=sa else "NOT_NEEDED_SCHEMA_UNCHANGED",
 "before":dict({"schema":int(sb)},**json.loads(cb)),
 "after":dict({"schema":int(sa)},**json.loads(ca)),
 "migration_evidence":{"marker_rows_before":int(mb),"marker_rows_after":int(ma)},
 "configuration":"UNCHANGED","demo":"UNCHANGED",
 "consumer":"NOT_RUN","live_source":"NOT_RUN","production":"NOT_PRODUCTION_READY"}
pathlib.Path(f"{reports}/{short}-deployment.json").write_text(json.dumps(dep,indent=2))
def load(n):
    p=pathlib.Path(f"{reports}/{short}-{n}.json")
    return json.loads(p.read_text()) if p.exists() else {"status":"MISSING"}
stdio,tunnel,smoke=load("stdio"),load("tunnel-observation"),load("smoke")
assert dep["status"]=="DEPLOYED"
checks={
 "stdio_status":stdio.get("status")=="PASS",
 "stdio_tools_count":len(stdio.get("stdio",{}).get("tools",[]))==18,
 "stdio_production_ready_false":stdio.get("stdio",{}).get("capabilities",{}).get("production_ready") is False,
 "stdio_idle_pass":stdio.get("stdio",{}).get("same_proxy_idle",{}).get("status")=="PASS",
 "tunnel_status":tunnel.get("status")=="PASS",
 "tunnel_assertions":all(tunnel.get("assertions",{}).values()) if tunnel.get("assertions") else False,
 "smoke_status":smoke.get("status")=="PASS",
 "smoke_bound":sha in smoke.get("version",""),
}
ok=all(checks.values())
fin={"status":"PASS" if ok else "FAILED_AT_REPORTS","code_sha":sha,"release":tag,
 "checks":checks,
 "stdio_report":f"{reports}/{short}-stdio.json","tunnel_report":f"{reports}/{short}-tunnel-observation.json",
 "smoke_report":f"{reports}/{short}-smoke.json","deployment_report":f"{reports}/{short}-deployment.json",
 "normal_db_schema":int(sa),"tools_count":len(stdio["stdio"]["tools"]) if stdio.get("stdio") else None,
 "consumer":"NOT_RUN","live_source":"NOT_RUN","production":"NOT_PRODUCTION_READY",
 "source_gates":["GBBQ_QFQ_NOT_VERIFIED","FILE_ACCESS_USE_NOT_VERIFIED","FILE_SET_CONSISTENCY_NOT_VERIFIED","CATALOG_INDUSTRY_NOT_VERIFIED","LOCAL_PIPELINE_NOT_IMPLEMENTED"]}
pathlib.Path(f"{reports}/{short}-final-verification.json").write_text(json.dumps(fin,indent=2))
sys.exit(0 if ok else 1)
PY
echo "deployed $TAG @ $SHA"
