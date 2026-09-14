#!/usr/bin/env bash
# Repeatable local test for publish_dist_ref.sh: temp bare remote, real
# plumbing, no GitHub required.
#   1. first push creates refs/dist/<tag>
#   2. identical rerun is idempotent (same commit, no error)
#   3. same tag with a different tarball digest is rejected, ref unchanged
#   4. `git archive` extracts the 3 assets + manifest and the tarball digest
#      matches the manifest binding
#   5. retention keeps the newest 3 preview refs plus the current tag
set -euo pipefail
cd "$(dirname "$0")/.."
T=$(mktemp -d /tmp/dist-ref-test-XXXXXX); trap 'rm -rf "$T"' EXIT
git init --bare -q "$T/remote.git"
git init -q "$T/work"; cd "$T/work"; git remote add origin "$T/remote.git"
PUB="$OLDPWD/scripts/publish_dist_ref.sh"
pass=0; fail=0
check() { if eval "$2"; then echo "  PASS $1"; pass=$((pass+1)); else echo "  FAIL $1"; fail=$((fail+1)); fi; }

mkpayload() {  # mkpayload <dir> <version> <tag_sha> <tar-content>
    local d=$1 v=$2 sha=$3
    mkdir -p "$d"
    printf '%s' "$4" > "$d/x-data-mcp-$v-linux-x86_64.tar.gz"
    (cd "$d" && sha256sum "x-data-mcp-$v-linux-x86_64.tar.gz" > "x-data-mcp-$v-linux-x86_64.tar.gz.sha256")
    printf '{"status":"PASS","version":"x-data-mcp %s %s NOT_PRODUCTION_READY"}\n' "$v" "$sha" > "$d/smoke-report.json"
}

SHA_A=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
SHA_B=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
TAG=v0.2.0-preview.90

echo "== 1+2: first push + idempotent rerun =="
mkpayload "$T/p1" 0.2.0-preview.90 "$SHA_A" "payload-A"
bash "$PUB" "$TAG" "$SHA_A" "$T/p1"
ref1=$(git ls-remote origin "refs/dist/$TAG" | cut -f1)
check "ref created" "[[ -n '$ref1' ]]"
bash "$PUB" "$TAG" "$SHA_A" "$T/p1" >/dev/null
ref2=$(git ls-remote origin "refs/dist/$TAG" | cut -f1)
check "idempotent rerun keeps ref" "[[ '$ref1' == '$ref2' ]]"

echo "== 3: same tag, different digest rejected =="
mkpayload "$T/p2" 0.2.0-preview.90 "$SHA_A" "payload-B-different"
set +e
bash "$PUB" "$TAG" "$SHA_A" "$T/p2" >/dev/null 2>&1; rc=$?
set -e
check "digest mismatch fails" "[[ $rc -ne 0 ]]"
ref3=$(git ls-remote origin "refs/dist/$TAG" | cut -f1)
check "ref unchanged after rejection" "[[ '$ref1' == '$ref3' ]]"

echo "== 4: archive round-trip =="
mkdir -p "$T/extract"
git archive "$ref1" | tar -x -C "$T/extract"
check "three assets + manifest" "[[ -f '$T/extract/x-data-mcp-0.2.0-preview.90-linux-x86_64.tar.gz' && -f '$T/extract/x-data-mcp-0.2.0-preview.90-linux-x86_64.tar.gz.sha256' && -f '$T/extract/smoke-report.json' && -f '$T/extract/dist-manifest.json' ]]"
check "extracted tar sha matches manifest" "python3 -c \"import json,hashlib,pathlib; m=json.load(open('$T/extract/dist-manifest.json')); assert m['tag']=='$TAG' and m['tag_sha']=='$SHA_A'; assert hashlib.sha256(pathlib.Path('$T/extract/x-data-mcp-0.2.0-preview.90-linux-x86_64.tar.gz').read_bytes()).hexdigest()==m['tarball_sha256']\""

echo "== 5: retention keeps newest 3 + current =="
for n in 91 92 93; do
    mkpayload "$T/pn$n" "0.2.0-preview.$n" "$SHA_B" "payload-$n"
    bash "$PUB" "v0.2.0-preview.$n" "$SHA_B" "$T/pn$n" >/dev/null
done
mkpayload "$T/pn94" "0.2.0-preview.94" "$SHA_B" "payload-94"
bash "$PUB" "v0.2.0-preview.94" "$SHA_B" "$T/pn94" >/dev/null
refs=$(git ls-remote origin 'refs/dist/v*-preview.*' | awk '{print $2}' | sort)
check "oldest refs deleted" "! grep -qE 'preview\.(90|91)$' <<<\"\$refs\""
check "newest 3 kept" "grep -q 'preview\.92$' <<<\"\$refs\" && grep -q 'preview\.93$' <<<\"\$refs\" && grep -q 'preview\.94$' <<<\"\$refs\""
check "exactly 3 dist refs remain" "[[ \$(git ls-remote origin 'refs/dist/*' | wc -l) -eq 3 ]]"

echo "== $pass passed, $fail failed =="
[[ $fail -eq 0 ]]
