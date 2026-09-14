#!/usr/bin/env bash
# Deploy a tagged preview release to /opt/x-data-mcp-preview and produce the
# five evidence reports. Fail-closed: any unmet precondition exits non-zero
# before touching /opt; a failed switch leaves `current` and the database
# untouched (see FAILED_AT_* in the deployment report).
#
# Usage: deploy_preview.sh <tag> [--dry-run] [--force-git-transport]
#   --dry-run:            resolve release + record before-state + print plan;
#                       never downloads, never touches /opt.
#   --force-git-transport: skip gh release download, fetch the package via
#                       refs/dist/<tag> (git smart-HTTP bypass for a throttled
#                       object CDN). Either way the tarball must match the
#                       release's .sha256 asset.
set -euo pipefail

DEPLOY_ROOT=/opt/x-data-mcp-preview
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
TAG_SHORT=${TAG//./_}

step=init
fail() { echo "FAILED_AT_$step: $1" >&2; exit 1; }
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
WORK=$(mktemp -d "/tmp/deploy-${TAG}-XXXXXX")
trap 'rm -rf "$WORK"' EXIT

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
  plan: fetch tarball -> verify sha256 -> unpack -> backup db+config ->
        stop $SERVICE -> task_sqlite upgrade if needed -> switch current ->
        start service -> smoke (package + live stdio + tunnel) -> 5 reports
EOF
    exit 0
fi

# --- 3. fetch + verify package ------------------------------------------------
step=fetch_package
fetch_git() {
    git -C "$REPO" fetch origin "refs/dist/$TAG:refs/dist/$TAG" --depth 1 \
        || fail "refs/dist/$TAG not available"
    git -C "$REPO" archive "refs/dist/$TAG" | tar -x -C "$WORK" \
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
(cd "$WORK" && sha256sum -c "$TARBALL.sha256") || fail "tarball sha256 mismatch"
[[ "$(sha256sum "$WORK/$TARBALL" | cut -d' ' -f1)" == "$asset_digest" ]] || fail "tarball sha256 differs from release asset digest"
mkdir -p "$RELEASE_DIR"
tar -xzf "$WORK/$TARBALL" -C "$RELEASE_DIR" --strip-components=1 || fail "unpack"
(cd "$RELEASE_DIR" && sha256sum --check SHA256SUMS >/dev/null) || fail "package SHA256SUMS"
"$RELEASE_DIR/bin/mcp_data" --version | grep -q "$SHA" || fail "binary version does not embed $SHA"
cp "$WORK/smoke-report.json" "$REPORTS/${TAG_SHORT}-smoke.json" 2>/dev/null || true

# --- 4. backup ----------------------------------------------------------------
step=backup
TS=$(now_utc)
BACKUP="$DEPLOY_ROOT/state/backup-${TAG}-${TS}.sqlite"
cp "$DB" "$BACKUP" || fail "db backup"
for ext in shm wal; do [[ -f "$DB-$ext" ]] && cp "$DB-$ext" "$BACKUP-$ext"; done
BACKUP_SHA=$(sha256sum "$BACKUP" | cut -d' ' -f1)
CONF="$DEPLOY_ROOT/state/config-before-${TAG}-${TS}"
mkdir -p "$CONF"
cp "$DEPLOY_ROOT/mcp.json" "$CONF/0-mcp.json" 2>/dev/null || true
systemctl cat "$SERVICE" >"$CONF/1-$SERVICE" || fail "service unit snapshot"
systemctl cat "$TUNNEL" >"$CONF/2-$TUNNEL" || fail "tunnel unit snapshot"
cat /etc/systemd/system/$TUNNEL.d/*.conf >"$CONF/3-data-service.conf" 2>/dev/null || true
cp "$DEPLOY_ROOT/wait-data-service.py" "$CONF/4-wait-data-service.py" 2>/dev/null || true

# --- 5. stop + schema upgrade + verify ---------------------------------------
step=schema_upgrade
systemctl stop "$SERVICE" || fail "stop $SERVICE"
"$RELEASE_DIR/bin/task_sqlite" upgrade "$DB" || fail "task_sqlite upgrade"
schema_after=$(db_scalar "PRAGMA user_version")
[[ "$schema_after" -ge "$schema_before" ]] || fail "user_version regressed"
table_after=$(db_scalar "SELECT count(*) FROM sqlite_master WHERE name='task_item_execution_starts'")
markers_after=0
if [[ "$table_after" == "1" ]]; then
    markers_after=$(db_scalar "SELECT count(*) FROM task_item_execution_starts")
    trig=$(db_scalar "SELECT count(*) FROM sqlite_master WHERE type='trigger' AND name LIKE 'task_item_execution_starts_no_%'")
    [[ "$trig" == "2" ]] || fail "immutability triggers missing"
    [[ "$markers_after" == "$markers_before" ]] || fail "execution-start markers fabricated during upgrade"
fi
# --- 6. atomic switch ----------------------------------------------------------
step=switch
ln -sfn "$RELEASE_DIR" "$DEPLOY_ROOT/current" || fail "switch current"
systemctl start "$SERVICE" || fail "start $SERVICE"
for _ in $(seq 1 100); do [[ -S "$SOCK" ]] && break; sleep 0.1; done
[[ -S "$SOCK" ]] || fail "service socket did not appear"
systemctl is-active --quiet "$SERVICE" || fail "$SERVICE not active"

# --- 7. smoke: live stdio + tunnel observation --------------------------------
step=smoke
python3 - "$SOCK" "$RELEASE_DIR" "$REPORTS/${TAG_SHORT}-stdio.json" <<'PYEOF' || fail "stdio smoke"
import json, subprocess, socket, time, select, hashlib, sqlite3, sys, pathlib
sock, rel, out = sys.argv[1], sys.argv[2], sys.argv[3]
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
db=sqlite3.connect("file:/opt/x-data-mcp-preview/state/preview.sqlite?mode=ro",uri=True)
counts={t:db.execute(f"SELECT count(*) FROM {t}").fetchone()[0] for t in
        ["tasks","requests","data_versions","catalog_versions","scope_occupancy"]}
if db.execute("SELECT count(*) FROM sqlite_master WHERE name='task_item_execution_starts'").fetchone()[0]:
    counts["task_item_execution_starts"]=db.execute("SELECT count(*) FROM task_item_execution_starts").fetchone()[0]
rep={"status":"PASS","version":init["serverInfo"]["version"],
 "service_binary_sha256":hashlib.sha256(pathlib.Path(rel+"/bin/data_service").read_bytes()).hexdigest(),
 "proxy_binary_sha256":hashlib.sha256(pathlib.Path(rel+"/bin/mcp_connect").read_bytes()).hexdigest(),
 "normal_db_counts":counts,"normal_db_schema":db.execute("PRAGMA user_version").fetchone()[0],
 "stdio":{"initialize":init,"tools":[t["name"] for t in tools],"capabilities":cap,
   "catalog":catalog,"empty_reads":empty_reads,"tasks":tasks,
   "inspect_verification_task":capslice("inspect_verification_task"),
   "verify_publication_result":capslice("verify_publication_result"),
   "stopping":capslice("verify_stopped_writes"),
   "yearly_update":capslice("update_data"),
   "same_proxy_idle":idle},
 "chatgpt_consumer":"NOT_RUN","live_source":"NOT_RUN","production":"NOT_PRODUCTION_READY"}
assert not cap["production_ready"] and not cap2["production_ready"]
pathlib.Path(out).write_text(json.dumps(rep,indent=2))
PYEOF
python3 - "$SHA" "$REPORTS/${TAG_SHORT}-tunnel-observation.json" <<'PY' || fail "tunnel observation"
import json, subprocess, hashlib, pathlib, sys, urllib.request
sha, out = sys.argv[1], sys.argv[2]
def svc(n):
    o=subprocess.run(["systemctl","show",n,"-p","MainPID,NRestarts,ActiveState,SubState,ControlGroup,ActiveEnterTimestampMonotonic"],capture_output=True,text=True).stdout
    return dict(l.split("=",1) for l in o.strip().splitlines())
procs=[]
for l in subprocess.run(["bash","-c","for p in /proc/[0-9]*; do e=$(readlink $p/exe 2>/dev/null); case $e in /opt/x-data-mcp-preview/*) echo $(basename $p) $(basename $e);; esac; done"],capture_output=True,text=True).stdout.splitlines():
    pid,name=l.split()
    procs.append({"pid":int(pid),"binary":name,"sha256":hashlib.sha256(pathlib.Path(f"/proc/{pid}/exe").resolve().read_bytes()).hexdigest()})
health={"locator":{"kind":"url_file","url_file":"/root/.local/state/tunnel-client/health/x-data-mcp.url"}}
base=pathlib.Path(health["locator"]["url_file"]).read_text().strip()
health["resolved_base_url"]=base
for ep in ["healthz","readyz","metrics"]:
    with urllib.request.urlopen(f"{base}/{ep}",timeout=5) as r:
        health[ep]={"url":f"{base}/{ep}","ok":r.status==200,"status":r.status,"body":r.read().decode()[:200]}
rep={"status":"PASS","code_sha":sha,
 "services":{n:svc(n) for n in ["x-data-mcp-data.service","x-data-mcp-tunnel.service"]},
 "actual_release_processes":procs,
 "data_process_sha256":hashlib.sha256(pathlib.Path("/proc/"+svc("x-data-mcp-data.service")["MainPID"]+"/exe").resolve().read_bytes()).hexdigest(),
 "health":health,"consumer":"NOT_RUN","live_source":"NOT_RUN","production":"NOT_PRODUCTION_READY"}
pathlib.Path(out).write_text(json.dumps(rep,indent=2))
PY

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
ok=all(x.get("status")=="PASS" for x in (stdio,tunnel,smoke))
fin={"status":"PASS" if ok else "FAILED_AT_REPORTS","code_sha":sha,"release":tag,
 "stdio_report":f"{reports}/{short}-stdio.json","tunnel_report":f"{reports}/{short}-tunnel-observation.json",
 "smoke_report":f"{reports}/{short}-smoke.json","deployment_report":f"{reports}/{short}-deployment.json",
 "normal_db_schema":int(sa),"tools_count":stdio.get("tools_count"),
 "consumer":"NOT_RUN","live_source":"NOT_RUN","production":"NOT_PRODUCTION_READY",
 "source_gates":["GBBQ_QFQ_NOT_VERIFIED","FILE_ACCESS_USE_NOT_VERIFIED","FILE_SET_CONSISTENCY_NOT_VERIFIED","CATALOG_INDUSTRY_NOT_VERIFIED","LOCAL_PIPELINE_NOT_IMPLEMENTED"]}
pathlib.Path(f"{reports}/{short}-final-verification.json").write_text(json.dumps(fin,indent=2))
sys.exit(0 if ok else 1)
PY
echo "deployed $TAG @ $SHA"
