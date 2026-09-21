#!/usr/bin/env python3
"""Bounded fetch helper for x-data-acquire. Stdlib only.

The acquire service owns all state and decisions; this helper only moves
bytes. Commands print one JSON object to stdout:

  head  <url>        -> {"status", "etag", "last_modified", "content_length"}
  fetch <url> <dest> -> head fields + {"bytes", "sha256"} after a complete,
                        fsynced write to <dest> (temp file + rename)

Errors exit non-zero with the message on stderr; the service maps that to a
task failure, never to a retry of its own.
"""
import hashlib
import json
import os
import sys
import tempfile
import urllib.request

HEAD_TIMEOUT = 30
FETCH_TIMEOUT = 900
CHUNK = 1 << 20


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
        result = _fetch(url, sys.argv[3])
    else:
        sys.exit(f"unknown command {command}")
    print(json.dumps(result))


if __name__ == "__main__":
    main()
