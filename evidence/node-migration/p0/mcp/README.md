# NM-002 MCP descriptor freeze — capture evidence

Baseline `ea14ede82af049c9d71485b9d66f4a6c90a8ee16`, binary `0.2.0-preview.24`, build SHA `ea14ede…` (reported by `--version`).

## Contents

- `probes/*.jsonl` — newline-delimited JSON-RPC request fixtures (initialize, notifications/initialized, tools/list, capability call, unknown-tool, bad-args, unknown-method, missing-tool-name probes).
- `captures/*.jsonl` — verbatim stdout wire responses, one response per line.
- `summary.json` — normalized manifest: protocol facts, error envelopes, per-surface tool names/counts, capability revisions, limits, per-file sha256, NOT_RUN list.
- `SHA256SUMS` — sha256 over probes, captures and summary.

## Reproduction (deterministic, Box)

```sh
cd <worktree>
TMPDIR=/dev/shm ~/.cargo/bin/cargo build --locked --bins   # rustup stable 1.98.1
mkdir -p -m 700 /dev/shm/p0 && cd /dev/shm/p0              # parent dir MUST be 0700 UID-owned (guard)
<wt>/target/debug/task_sqlite init db18.sqlite             # schema 18
<wt>/target/debug/task_sqlite init19 db19.sqlite           # schema 19
chmod 600 db*.sqlite

# stdio surfaces: pipe a probe file on stdin, capture stdout
<wt>/target/debug/mcp_data db18.sqlite        < probes/probe-data.jsonl   > mcp_data-schema18.jsonl
<wt>/target/debug/mcp_data db19.sqlite        < probes/probe-data.jsonl   > mcp_data-schema19.jsonl
X_DATA_MCP_STORE=/dev/shm/p0/jsonstore \
  <wt>/target/debug/mcp_server                < probes/probe-legacy.jsonl > mcp_server-legacy.jsonl
<wt>/target/debug/mcp_sqlite_dev db18.sqlite  < probes/probe-legacy.jsonl > mcp_sqlite_dev-legacy.jsonl

# socket surfaces: start service, drive via mcp_connect, SIGTERM to stop
<wt>/target/debug/data_service db19.sqlite /dev/shm/p0/mcp.sock &
<wt>/target/debug/mcp_connect /dev/shm/p0/mcp.sock < probes/probe-data.jsonl > data_service-schema19.jsonl
<wt>/target/debug/acquire_service /dev/shm/p0/genroot /dev/shm/p0/acq.sock &   # genroot 0700
<wt>/target/debug/mcp_connect /dev/shm/p0/acq.sock < probes/probe-acquire.jsonl  > acquire_service.jsonl
<wt>/target/debug/mcp_connect /dev/shm/p0/acq.sock < probes/probe-acquire2.jsonl > acquire_service-capabilities.jsonl
```

Notes:

- tmpfs (`/dev/shm`) passes `local_filesystem::supported_file`; no `X_DATA_MCP_UNSAFE_NONLOCAL_FS` was needed or used for these captures.
- Runtime paths must be UID-owned, `& ~0o077`, non-symlink; a 0755 parent is rejected (`runtime path must be owned by this UID, private, and not a symlink`).
- `data_service`/`acquire_service` remove their socket on SIGTERM via `Endpoint::drop`; `*.sock.owner.json` remains.
- Output contains no secrets; `X_DATA_MCP_GENERATION_ROOT` was unset, so `generation_root_unconfigured` blocking is expected.
