#!/usr/bin/env python3
"""Bounded fetch helper for the M3 exchange-source entry. Stdlib only.

The Rust side (`src/exchange_source/transport.rs`) never holds a socket; this
helper only moves bytes and headers, exactly like `tdx_fetch.py` does for the
acquire service. Retries, validation and freezing are the Rust entry's job, so
a non-2xx status is reported as a normal result and a transport failure exits
non-zero.

Usage:

    exchange_fetch.py get <dest>

The request is one JSON object on stdin:

    {"url": "https://...", "headers": {"Referer": "...", "User-Agent": "..."}}

and one JSON object is printed on stdout:

    {"status": 200, "etag": "...", "last_modified": "...", "bytes": 1234,
     "sha256": "..."}

`<dest>` is written through a temporary file in the same directory, fsynced and
renamed, so a partial body is never visible under the final name.
"""
import hashlib
import json
import os
import sys
import tempfile
import urllib.error
import urllib.request

TIMEOUT = 120
CHUNK = 1 << 20


def _request(payload):
    url = payload.get("url")
    if not isinstance(url, str) or not url:
        raise SystemExit("request needs a url")
    headers = payload.get("headers") or {}
    if not isinstance(headers, dict):
        raise SystemExit("headers must be an object")
    return urllib.request.Request(
        url,
        headers={str(k): str(v) for k, v in headers.items()},
        method="GET",
    )


def _fetch(request, dest):
    digest = hashlib.sha256()
    size = 0
    fd, tmp = tempfile.mkstemp(dir=os.path.dirname(os.path.abspath(dest)))
    try:
        with os.fdopen(fd, "wb") as out, urllib.request.urlopen(
            request, timeout=TIMEOUT
        ) as response:
            while True:
                chunk = response.read(CHUNK)
                if not chunk:
                    break
                out.write(chunk)
                digest.update(chunk)
                size += len(chunk)
            out.flush()
            os.fsync(out.fileno())
            status = response.status
            etag = response.headers.get("ETag")
            last_modified = response.headers.get("Last-Modified")
        os.replace(tmp, dest)
    except BaseException:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise
    return {
        "status": status,
        "etag": etag,
        "last_modified": last_modified,
        "bytes": size,
        "sha256": digest.hexdigest(),
    }


def main():
    if len(sys.argv) != 3 or sys.argv[1] != "get":
        sys.exit("usage: exchange_fetch.py get <dest>  (request JSON on stdin)")
    payload = json.load(sys.stdin)
    request = _request(payload)
    try:
        result = _fetch(request, sys.argv[2])
    except urllib.error.HTTPError as error:
        # A non-2xx answer still carries the status and headers the entry
        # classifies as `source_unavailable`; exit zero so the caller can tell
        # "the source said no" apart from "the transport broke".
        result = {
            "status": error.code,
            "etag": error.headers.get("ETag") if error.headers else None,
            "last_modified": error.headers.get("Last-Modified") if error.headers else None,
            "bytes": 0,
            "sha256": hashlib.sha256(b"").hexdigest(),
        }
        with open(sys.argv[2], "wb"):
            pass
    print(json.dumps(result))


if __name__ == "__main__":
    main()
