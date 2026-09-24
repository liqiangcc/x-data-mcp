#!/usr/bin/env bash
# NM-072 P5 bench orchestrator — builds the rlib-linked Rust oracle
# (when the crate target cache exists) then runs the Node bench.
# Usage: run.sh <workdir-on-local-fs> [iters] [warmup]
set -euo pipefail
cd "$(dirname "$0")/../.."
DIR="${1:?workdir required (use /dev/shm/...)}"
ITERS="${2:-200}"; WARMUP="${3:-20}"
export PATH="$HOME/.cargo/bin:$PATH"

# Prefer the release rlib — a dev-profile oracle would understate Rust
# and pollute the baseline. Fall back to debug only if no release lib.
PROFILE=release
RLIB=$(ls -t target/release/deps/libx_data_mcp-*.rlib 2>/dev/null | head -1 || true)
[ -z "$RLIB" ] && PROFILE=debug && \
  RLIB=$(ls -t target/debug/deps/libx_data_mcp-*.rlib 2>/dev/null | head -1 || true)
# rustc scratch objects overflow /dev/shm's 64MB — build under target/
# (gitignored) with a spacious TMPDIR; the bench workdir stays tmpfs.
BENCH="target/node_p5_bench"
if [ -n "$RLIB" ]; then
  dep() { ls -t target/"$PROFILE"/deps/lib"$1"-*.rlib 2>/dev/null | head -1; }
  mkdir -p target/.p5-rust-tmp
  TMPDIR="$PWD/target/.p5-rust-tmp" rustc --edition 2024 -O \
    scripts/node_migration_p5/rust_bench.rs \
    --extern "x_data_mcp=$RLIB" \
    --extern "serde_json=$(dep serde_json)" \
    --extern "serde=$(dep serde)" \
    --extern "chrono=$(dep chrono)" \
    --extern "rust_decimal=$(dep rust_decimal)" \
    --extern "rusqlite=$(dep rusqlite)" \
    -L target/"$PROFILE"/deps -o "$BENCH" 2>&1 | tail -5
  rm -rf target/.p5-rust-tmp
  echo "oracle: $PROFILE rlib $RLIB" >&2
fi
BENCH_ITERS="$ITERS" BENCH_WARMUP="$WARMUP" \
  node scripts/node_migration_p5/bench.mjs "$DIR" \
  "$([ -x "$BENCH" ] && echo "$BENCH" || echo "")"
