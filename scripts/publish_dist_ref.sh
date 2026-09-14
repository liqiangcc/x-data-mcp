#!/usr/bin/env bash
# Publish a verified release payload to refs/dist/<tag> on `origin` — the
# git-transport fallback consumed by deploy_preview.sh --force-git-transport.
#
# Usage (inside a git worktree whose `origin` can push):
#   publish_dist_ref.sh <tag> <tag_sha> <payload_dir>
# payload_dir must contain x-data-mcp-<version>-linux-x86_64.tar.gz,
# <tarball>.sha256 and smoke-report.json — all already validated by the
# caller (release workflow). This script re-checks the tarball checksum and
# the smoke report's SHA/version binding before publishing.
#
# Fail-closed:
#   * an existing refs/dist/<tag> whose dist-manifest.json does not bind this
#     exact tag + tag_sha + tarball digest aborts the run — never force-overwrite;
#   * an identical binding is idempotent: the ref is left as-is;
#   * retention keeps the newest 3 refs/dist/v*-preview.<N> plus the current
#     tag; older refs are deleted best-effort (warning only).
set -euo pipefail

TAG=${1:?tag}; TAG_SHA=${2:?tag sha}; PAYLOAD=${3:?payload dir}
[[ "$TAG" =~ ^v[0-9] ]] || { echo "tag must be v*" >&2; exit 2; }
[[ "$TAG_SHA" =~ ^[0-9a-f]{40}$ ]] || { echo "tag_sha must be a commit" >&2; exit 2; }
version=${TAG#v}
TARBALL="x-data-mcp-${version}-linux-x86_64.tar.gz"
for f in "$TARBALL" "$TARBALL.sha256" "smoke-report.json"; do
    [[ -f "$PAYLOAD/$f" ]] || { echo "payload missing $f" >&2; exit 1; }
done
(cd "$PAYLOAD" && sha256sum -c "$TARBALL.sha256" >/dev/null) \
    || { echo "tarball fails its own .sha256" >&2; exit 1; }
tar_sha=$(sha256sum "$PAYLOAD/$TARBALL" | cut -d' ' -f1)
python3 - "$PAYLOAD/smoke-report.json" "$TAG_SHA" "$version" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
assert r["status"] == "PASS", r["status"]
assert sys.argv[2] in r["version"], r["version"]
assert sys.argv[3] in r["version"], r["version"]
PY

MANIFEST=$(python3 - "$TAG" "$TAG_SHA" "$tar_sha" "$version" <<'PY'
import json, sys
print(json.dumps({"tag": sys.argv[1], "tag_sha": sys.argv[2],
                  "tarball_sha256": sys.argv[3], "version": sys.argv[4]},
                 indent=2, sort_keys=True))
PY
)

# --- existing ref: idempotent only when the binding is identical -------------
if git fetch --quiet --depth 1 origin "refs/dist/$TAG" 2>/dev/null; then
    old=$(git show "FETCH_HEAD:dist-manifest.json" 2>/dev/null) \
        || { echo "refs/dist/$TAG exists without dist-manifest.json" >&2; exit 1; }
    python3 - "$old" "$TAG" "$TAG_SHA" "$tar_sha" <<'PY' \
        || { echo "refs/dist/$TAG binds different tag/sha/digest; refusing" >&2; exit 1; }
import json, sys
m = json.loads(sys.argv[1])
assert m["tag"] == sys.argv[2] and m["tag_sha"] == sys.argv[3] \
    and m["tarball_sha256"] == sys.argv[4], m
PY
    echo "refs/dist/$TAG already binds tag_sha=$TAG_SHA tarball=$tar_sha; idempotent, no push"
else
    # --- orphan commit via plumbing; index and worktree untouched -------------
    export GIT_AUTHOR_NAME="x-data-mcp release" GIT_AUTHOR_EMAIL="release@localhost" \
           GIT_COMMITTER_NAME="x-data-mcp release" GIT_COMMITTER_EMAIL="release@localhost"
    mtree() { git hash-object -w "$PAYLOAD/$1" | sed "s/^/100644 blob /;s/$/\t$1/"; }
    tree=$( { mtree "$TARBALL"; mtree "$TARBALL.sha256"; mtree "smoke-report.json"
              echo "$MANIFEST" > "$PAYLOAD/.dist-manifest.json"
              mtree ".dist-manifest.json" | sed 's/\.dist-manifest\.json/dist-manifest.json/'
              rm -f "$PAYLOAD/.dist-manifest.json"; } | git mktree )
    commit=$(git commit-tree "$tree" -m "dist $TAG @ $TAG_SHA tarball sha256:$tar_sha")
    git push origin "$commit:refs/dist/$TAG" \
        || { echo "push refs/dist/$TAG failed" >&2; exit 1; }
    echo "pushed refs/dist/$TAG -> $commit"
fi

# --- bounded retention: newest 3 preview dist refs + the current tag ---------
ls_remote=$(git ls-remote origin 'refs/dist/v*-preview.*' 2>/dev/null || true)
if [[ -n "$ls_remote" ]]; then
    for ref in $(awk '{print $2}' <<<"$ls_remote" | python3 -c '
import re, sys
refs = [l.strip() for l in sys.stdin if l.strip()]
keyed = [(int(re.search(r"preview\.(\d+)$", r).group(1)), r) for r in refs
         if re.search(r"preview\.(\d+)$", r)]
keyed.sort(reverse=True)
print("\n".join(r for _, r in keyed[3:]))
'); do
        [[ "$ref" == "refs/dist/$TAG" ]] && continue  # never delete current
        git push origin --delete "$ref" >/dev/null 2>&1 \
            || echo "::warning::could not delete old dist ref $ref" >&2
    done
fi
