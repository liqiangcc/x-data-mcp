# NM-005 Rust baseline measurements — evidence

Baseline `5435d93eca28622bb7f546fe0ad6f78b6a42fce5` (`migration/node-devin`), release binaries `0.2.0-preview.24`.

## Contents

- `results.json` — consolidated measurements with explicit boundaries (environment, SHA, inputs, samples).
- `bench-debug.jsonl` / `bench-release.jsonl` — raw per-rep nanosecond samples for the 5 synthetic workloads (harness `scripts/node_migration_p0/bench.rs`, `rustc --extern` against the debug or release rlib).
- `mcp_data-stdio-latency.csv` — 20 reps of `mcp_data db19.sqlite < probe7.jsonl` (ms,exit).
- `socket-call-latency.csv` — 20 reps of `mcp_connect sock < call1.jsonl` (ms,exit).
- `SHA256SUMS` — over all of the above.

## Reproduction (Box)

```sh
cd <worktree>
TMPDIR=/dev/shm cargo build --locked --bins            # dev profile
cargo build --locked --release --bins                  # release profile
# compile bench against the release rlib:
rustc --edition 2024 -C debuginfo=0 -O -L dependency=target/release/deps \
  --extern x_data_mcp=$(ls target/release/deps/libx_data_mcp-*.rlib|head -1) \
  --extern serde_json=$(ls target/release/deps/libserde_json-*.rlib|head -1) \
  --extern serde=$(ls target/release/deps/libserde-*.rlib|head -1) \
  --extern rust_decimal=$(ls target/release/deps/librust_decimal-*.rlib|head -1) \
  --extern chrono=$(ls target/release/deps/libchrono-*.rlib|head -1) \
  scripts/node_migration_p0/bench.rs -o target/debug/nm005-bench-rel
target/debug/nm005-bench-rel > bench-release.jsonl
# MCP probes (probe7.jsonl / call1.jsonl reproduced in results.json notes):
mkdir -p -m 700 /dev/shm/nm005 && task_sqlite init19 /dev/shm/nm005/db19.sqlite
for i in $(seq 20); do s=$(date +%s%N); mcp_data db19 < probe7.jsonl >/dev/null; echo $((($(date +%s%N)-s)/1000000)); done
```

Boundaries: 8-vCPU Xeon VM, load ~2.7, 16GB RAM, overlayfs worktree, tmpfs runtime dirs. Single-threaded probes, no injected concurrency. Debug vs release gap is ~6–74× on these probes — debug numbers are dev-loop evidence only.
