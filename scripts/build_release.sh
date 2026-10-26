#!/usr/bin/env bash
set -euo pipefail
revision=$(git rev-parse HEAD)
version=$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["version"])')
name="x-data-mcp-${version}-linux-x86_64"
stage="dist/${name}"
if [[ -e "$stage" ]]; then
  echo "release staging directory already exists: $stage" >&2
  exit 1
fi
cargo build --release --locked --bins
mkdir -p "$stage/bin"
for binary in data_service acquire_service mcp_connect mcp_data mcp_server mcp_sqlite_dev task_sqlite inspect_day inspect_snapshot create_demo consistency_check; do
  cp "target/release/$binary" "$stage/bin/"
done
cp -r migrations "$stage/migrations"
mkdir -p "$stage/scripts"
cp scripts/tdx_fetch.py "$stage/scripts/tdx_fetch.py"
# The tunnel drop-in runs <release>/scripts/wait_service_ready.py directly
# (D-17: one readiness definition), so it ships executable.
cp scripts/wait_service_ready.py "$stage/scripts/wait_service_ready.py"
chmod 755 "$stage/scripts/wait_service_ready.py"
cp README.md "$stage/README.md"
cp fixtures/demo/README.md "$stage/DEMO_DATA.md"
cp scripts/smoke_release.py "$stage/check.py"
cp scripts/test_runtime_peer.py "$stage/check_peer.py"
python3 - "$stage" "$version" "$revision" <<'PY'
import json,pathlib,subprocess,sys
stage,version,revision=sys.argv[1:]
manifest=dict(version=version,git_sha=revision,target="x86_64-unknown-linux-gnu",build_platform="ubuntu-24.04",minimum_glibc="2.39",rustc=subprocess.check_output(["rustc","--version"],text=True).strip(),production="NOT_PRODUCTION_READY",services=["data_service: market data MCP over owner-only unix socket","acquire_service: official-source generation lifecycle MCP over owner-only unix socket"],source_gates=["#54","#6/#46","#71","official file snapshot consistency"],default_backend="explicit SQLite stdio; no implicit initialization",fixture_tool="create_demo: explicit new synthetic database only")
pathlib.Path(stage,"manifest.json").write_text(json.dumps(manifest,indent=2)+"\n")
PY
"$stage/bin/mcp_data" --version | python3 -c 'import sys; assert sys.argv[1] in sys.stdin.read(), "binary build SHA does not match package"' "$revision"
(cd "$stage" && sha256sum bin/* migrations/tasks/*.sql scripts/*.py README.md DEMO_DATA.md check.py check_peer.py manifest.json > SHA256SUMS)
tar -czf "dist/${name}.tar.gz" -C dist "$name"
(cd dist && sha256sum "${name}.tar.gz" > "${name}.tar.gz.sha256")
printf '%s\n' "$stage"
