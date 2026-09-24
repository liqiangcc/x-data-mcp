# NM-066 — mcp_connect bin

Differential evidence for `src_node/bin/mcp_connect.mjs` vs the Rust
`target/debug/mcp_connect` binary.

## Lane

- `src_node/bin/mcp_connect.mjs` — the stdio↔socket proxy.
- `src_node/bin/_shared.mjs` — IO_TEXT extended with socket errnos
  (EPIPE/ECONNRESET/ECONNREFUSED/ECONNABORTED/ETIMEDOUT).
- Consumes w3's `src_node/local_runtime/paths.mjs connect()` read-only
  (no edits; branch rebased `--onto 2b4888b` to carry S13).
- `src_node/native/rename_noreplace.node` built locally per w3's recipe
  (`gcc -shared -fPIC -O2 -I/usr/include/node`) — gitignored artifact.

## Run

    cargo build --bin mcp_connect
    gcc -shared -fPIC -O2 -I/usr/include/node \
      -o src_node/native/rename_noreplace.node src_node/native/rename_noreplace.c
    TMPDIR=/dev/shm node scripts/node_migration_p4/mcp_connect_probe.mjs
    TMPDIR=/dev/shm SELFTEST=1 node scripts/node_migration_p4/mcp_connect_probe.mjs  # exits 1

## Results

16 cases, **16 pass, 0 fail**, 1 NOT_RUN (foreign-owner peer UID —
single-UID box). Compared: stdout bytes (base64), stderr text, exit code.

- Arg surface: no-arg / two-args → `usage: mcp_connect <private-socket>`, exit 1.
- Path guards via w3 connect(): missing socket → `No such file or
  directory (os error 2)`; regular file → `endpoint is not a socket`;
  symlink / 0755 parent → `runtime path must be owned by this UID,
  private, and not a symlink`; missing parent → ENOENT; bare name →
  `socket requires an explicit private parent directory`; stale socket
  → `Connection refused (os error 111)`.
- Overlayfs parent without opt-out → `socket directory changed or is
  not a supported local filesystem`; with X_DATA_MCP_UNSAFE_NONLOCAL_FS=1
  → connects and echoes (opt-out reaches through w3's supported_file).
- Live copy: echo small / 200KiB / empty (byte-identical stdout);
  `spew` — server writes payload + FIN while client stdin stays open →
  client exits 0 without waiting for stdin (dead-service EOF semantic);
  `early-close` — abortive close propagation matched.

## Artifacts

- `probe-results.jsonl` — per-case stdout/stderr/exit for both impls.
- `negative-selftest.jsonl` — injected exit-code flip + detection proof.
- `results.json` — counts + oracle identity + NOT_RUN.
- `SHA256SUMS`.
