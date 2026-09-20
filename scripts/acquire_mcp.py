#!/usr/bin/env python3
"""Acquisition adapter MCP server — the network half of daily sync.

Deliberately separate from x-data-mcp's data_service, which performs no
network I/O by design (trust boundary: the data service only consumes
locally inspected, sha256-pinned inputs). This adapter owns the network
side: HEAD probes of the three official TDX URLs and conditional download
of a new generation, verified by sha256 and written atomically into the
inspected input directory so the next service connection picks it up
(data_service re-inspects the env-pinned manifest per connection).

Tools:
  check_official_updates — HEAD the three URLs; report etag/Last-Modified
                           vs last-seen state. Read-only, zero download.
  acquire_generation     — if anything changed, download the three files,
                           verify zip integrity + sizes, write a pinned
                           manifest, and atomically swap the input dir.
                           Idempotent: unchanged etags short-circuit.
  sync_status            — report the currently installed generation.

State file: <official_dir>/../acquire-state.json (next to the input dir).
Stdlib only. MCP stdio JSON-RPC (initialize / tools/list / tools/call).
"""
import email.utils
import hashlib
import json
import os
import shutil
import sys
import tempfile
import urllib.request
import zipfile
from datetime import datetime, timezone

OFFICIAL_DIR = os.environ.get(
    "XDM_OFFICIAL_DIR", "/opt/x-data-mcp-preview/official-inputs")
STATE_PATH = os.path.join(os.path.dirname(OFFICIAL_DIR.rstrip("/")),
                          "acquire-state.json")

URLS = {
    "base_industry": "https://www.tdx.com.cn/products/data/data/dbf/base.zip",
    "gbbq": "https://www.tdx.com.cn/products/data/data/dbf/gbbq.zip",
    "day": "https://data.tdx.com.cn/vipdoc/hsjday.zip",
}
FILENAMES = {"base_industry": "base.zip", "gbbq": "gbbq.zip",
             "day": "hsjday.zip"}


def now_z():
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def load_state():
    try:
        return json.load(open(STATE_PATH))
    except Exception:
        return {"seen": {}, "generations": []}


def save_state(state):
    os.makedirs(os.path.dirname(STATE_PATH), exist_ok=True)
    tmp = STATE_PATH + ".tmp"
    with open(tmp, "w") as f:
        json.dump(state, f, ensure_ascii=False, indent=1)
    os.replace(tmp, STATE_PATH)


def head(url):
    req = urllib.request.Request(url, method="HEAD")
    with urllib.request.urlopen(req, timeout=30) as r:
        return {"status": r.status,
                "etag": r.headers.get("ETag"),
                "last_modified": r.headers.get("Last-Modified"),
                "content_length": r.headers.get("Content-Length")}


def check_updates(_args):
    state = load_state()
    out, changed = {}, []
    for role, url in URLS.items():
        h = head(url)
        prev = state["seen"].get(role)
        out[role] = {**h, "url": url,
                     "changed": prev is None or prev.get("etag") != h["etag"]}
        if out[role]["changed"]:
            changed.append(role)
    return {"checked_at": now_z(), "files": out, "changed": changed,
            "note": "weekends/holidays serve the last trading day's bytes; "
                    "unchanged etag means no new generation"}


def sha256_of(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def acquire(_args):
    state = load_state()
    check = check_updates({})
    if not check["changed"]:
        return {"action": "noop", "reason": "etags unchanged",
                "checked_at": check["checked_at"]}
    started = now_z()
    staging = tempfile.mkdtemp(prefix="xdm-acquire-")
    try:
        files = []
        for role, url in URLS.items():
            dest = os.path.join(staging, FILENAMES[role])
            t0 = now_z()
            with urllib.request.urlopen(url, timeout=600) as r, \
                    open(dest, "wb") as f:
                shutil.copyfileobj(r, f, 1 << 20)
            meta = check["files"][role]
            size = os.path.getsize(dest)
            if meta.get("content_length") and \
                    int(meta["content_length"]) != size:
                raise SystemExit(
                    f"{role}: size {size} != Content-Length "
                    f"{meta['content_length']}")
            with zipfile.ZipFile(dest) as z:
                if z.testzip() is not None:
                    raise SystemExit(f"{role}: zip CRC failure")
            files.append({
                "role": role,
                "relative_path": FILENAMES[role],
                "source_url": url,
                "acquisition_started_at": t0,
                "acquisition_completed_at": now_z(),
                "last_modified": meta.get("last_modified"),
                "etag": meta.get("etag"),
                "bytes": size,
                "sha256": sha256_of(dest),
            })
        manifest = {"schema": "x-data-mcp.official-file-input.v1",
                    "files": files}
        mpath = os.path.join(staging, "snapshot-manifest.json")
        with open(mpath, "w") as f:
            json.dump(manifest, f, ensure_ascii=False, indent=1)
        # Atomic swap: new dir staged complete, then rename over a backup.
        backup = OFFICIAL_DIR + ".prev"
        if os.path.isdir(backup):
            shutil.rmtree(backup)
        if os.path.isdir(OFFICIAL_DIR):
            os.replace(OFFICIAL_DIR, backup)
        os.replace(staging, OFFICIAL_DIR)
        gen = {"installed_at": now_z(), "acquired_at": started,
               "etags": {r: files[i]["etag"]
                         for i, r in enumerate(
                             ["base_industry", "gbbq", "day"])},
               "sha256": {f["role"]: f["sha256"] for f in files}}
        state["seen"] = {r: {"etag": check["files"][r]["etag"],
                             "last_modified":
                                 check["files"][r]["last_modified"],
                             "seen_at": now_z()}
                         for r in URLS}
        state["generations"].append(gen)
        save_state(state)
        return {"action": "installed", "generation": gen,
                "next": "call update_data on x-data-mcp to apply; "
                        "service picks up the new source on next connection"}
    except Exception:
        shutil.rmtree(staging, ignore_errors=True)
        raise


def status(_args):
    state = load_state()
    installed = os.path.isdir(OFFICIAL_DIR)
    manifest = None
    mp = os.path.join(OFFICIAL_DIR, "snapshot-manifest.json")
    if installed and os.path.exists(mp):
        m = json.load(open(mp))
        manifest = {"schema": m["schema"],
                    "files": {f["role"]: {"sha256": f["sha256"],
                                          "bytes": f["bytes"],
                                          "last_modified":
                                              f.get("last_modified")}
                              for f in m["files"]}}
    return {"official_dir": OFFICIAL_DIR, "installed": installed,
            "manifest": manifest,
            "generations_seen": len(state["generations"]),
            "last_seen": state["seen"]}


TOOLS = [
    {"name": "check_official_updates",
     "description": "HEAD the three official TDX URLs and compare "
                    "ETag/Last-Modified to last-seen state. Read-only.",
     "inputSchema": {"type": "object", "properties": {}},
     "handler": check_updates},
    {"name": "acquire_generation",
     "description": "If any of the three official files changed, download "
                    "the full generation, verify zip integrity + sizes, pin "
                    "sha256 into a fresh snapshot manifest, and atomically "
                    "install it into the inspected input directory.",
     "inputSchema": {"type": "object", "properties": {}},
     "handler": acquire},
    {"name": "sync_status",
     "description": "Report the installed generation: manifest pins, "
                    "last-seen remote state, generation count.",
     "inputSchema": {"type": "object", "properties": {}},
     "handler": status},
]


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except json.JSONDecodeError:
            continue
        rid, method, params = req.get("id"), req.get("method"), \
            req.get("params") or {}
        if method == "initialize":
            out = {"protocolVersion": "2024-11-05",
                   "capabilities": {"tools": {}},
                   "serverInfo": {"name": "x-data-acquire",
                                  "version": "0.1.0"}}
        elif method == "notifications/initialized" or rid is None:
            continue
        elif method == "tools/list":
            out = {"tools": [{k: v for k, v in t.items()
                              if k != "handler"} for t in TOOLS]}
        elif method == "tools/call":
            tool = next((t for t in TOOLS
                         if t["name"] == params.get("name")), None)
            if tool is None:
                out = {"isError": True, "content": [
                    {"type": "text", "text": "unknown tool"}]}
            else:
                try:
                    res = tool["handler"](params.get("arguments") or {})
                    out = {"content": [{"type": "text",
                                        "text": json.dumps(
                                            res, ensure_ascii=False)}],
                           "structuredContent": res}
                except Exception as e:
                    out = {"isError": True, "content": [
                        {"type": "text", "text": f"{type(e).__name__}: {e}"}]}
        else:
            out = {"error": {"code": -32601,
                             "message": f"unknown method {method}"}}
        print(json.dumps({"jsonrpc": "2.0", "id": rid, "result" if "error"
                          not in out else "error": out}),
              flush=True)


if __name__ == "__main__":
    main()
