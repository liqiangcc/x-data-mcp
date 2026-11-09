#!/usr/bin/env python3
"""Bounded fetch helper for x-data-acquire. Stdlib only.

The acquire service owns all state and decisions; this helper only moves
bytes. Commands print one JSON object to stdout:

  head  <url>        -> {"status", "etag", "last_modified", "content_length"}
  fetch <url> <dest> -> head fields + {"bytes", "sha256"} after a complete,
                        fsynced write to <dest> (temp file + rename)

While `fetch` runs it may print one `{"bytes": N}` line per landed chunk; the
last line is always the result object. `X_DATA_MCP_TDX_FETCH_NATIVE`, when set,
names a `tdx-fetch-progress` binary that does the same fetch and reports the
same progress lines; this helper then only validates its result. That override
is how the service reaches the streaming implementation without repackaging
the helper (#625).

Errors exit non-zero with the message on stderr; the service maps that to a
task failure, never to a retry of its own.
"""
import hashlib
import json
import os
import subprocess
import sys
import tempfile
import urllib.request

HEAD_TIMEOUT = 30
FETCH_TIMEOUT = 900
CHUNK = 1 << 20
NATIVE_ENV = "X_DATA_MCP_TDX_FETCH_NATIVE"


def _head(url):
    req = urllib.request.Request(url, method="HEAD")
    with urllib.request.urlopen(req, timeout=HEAD_TIMEOUT) as r:
        length = r.headers.get("Content-Length")
        return {
            "status": r.status,
            "etag": r.headers.get("ETag"),
            "last_modified": r.headers.get("Last-Modified"),
            "content_length": int(length) if length is not None else None,
        }


def _native_fetch(url, dest):
    """Delegate the fetch to the Rust helper, validating its result line.

    The service set the variable on this call only; the child inherits a
    scrubbed environment so a nested helper cannot delegate again. The result
    line is re-emitted verbatim (the caller prints it) after the checked
    fields are re-verified here: bytes on disk, digest and size.
    """
    native = os.environ[NATIVE_ENV]
    child_env = {k: v for k, v in os.environ.items() if k != NATIVE_ENV}
    proc = subprocess.run(
        [native, url, dest], env=child_env, capture_output=True, text=True
    )
    if proc.returncode != 0:
        raise SystemExit(proc.stderr.strip() or f"{native} exited {proc.returncode}")
    result = None
    for line in proc.stdout.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            payload = json.loads(line)
        except ValueError:
            print(line, flush=True)
            continue
        if "sha256" in payload:
            result = payload
        else:
            print(json.dumps(payload), flush=True)
    if result is None:
        raise SystemExit(f"{native} printed no result line")
    digest = hashlib.sha256()
    size = 0
    with open(dest, "rb") as f:
        while True:
            chunk = f.read(CHUNK)
            if not chunk:
                break
            digest.update(chunk)
            size += len(chunk)
    if size != result.get("bytes") or digest.hexdigest() != result.get("sha256"):
        raise SystemExit(
            f"native result mismatch: {size} bytes {digest.hexdigest()[:12]} "
            f"!= {result.get('bytes')} bytes {str(result.get('sha256'))[:12]}"
        )
    return result


def _fetch(url, dest):
    head = _head(url)
    digest = hashlib.sha256()
    size = 0
    fd, tmp = tempfile.mkstemp(dir=os.path.dirname(os.path.abspath(dest)))
    try:
        with os.fdopen(fd, "wb") as out, urllib.request.urlopen(
            url, timeout=FETCH_TIMEOUT
        ) as r:
            while True:
                chunk = r.read(CHUNK)
                if not chunk:
                    break
                out.write(chunk)
                digest.update(chunk)
                size += len(chunk)
            out.flush()
            os.fsync(out.fileno())
        if head.get("content_length") is not None and int(
            head["content_length"]
        ) != size:
            raise SystemExit(
                f"size mismatch: {size} != Content-Length "
                f"{head['content_length']}"
            )
        os.replace(tmp, dest)
    except BaseException:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise
    return {**head, "bytes": size, "sha256": digest.hexdigest()}


def main():
    if len(sys.argv) < 3:
        sys.exit("usage: tdx_fetch.py head <url> | fetch <url> <dest>")
    command, url = sys.argv[1], sys.argv[2]
    if command == "head":
        result = _head(url)
    elif command == "fetch":
        if len(sys.argv) != 4:
            sys.exit("usage: tdx_fetch.py fetch <url> <dest>")
        native = os.environ.get(NATIVE_ENV)
        result = _native_fetch(url, sys.argv[3]) if native else _fetch(url, sys.argv[3])
    else:
        sys.exit(f"unknown command {command}")
    print(json.dumps(result))


if __name__ == "__main__":
    main()
