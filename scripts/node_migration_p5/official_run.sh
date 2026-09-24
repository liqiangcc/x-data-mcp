#!/usr/bin/env bash
# NM-110 P5 orchestrator — official materialize lane (w4).
# Builds the release rlib oracle if needed, then runs every surface on
# both impls with warmup+N iters, one JSONL stats line per (impl,surface).
#
# Usage: official_run.sh [iters] [warmup]
#   Env: GEN3=/home/box/retained-inputs/gen3
#        MANIFEST=evidence/issue-4-update-gen3-acceptance/snapshot-manifest-gen3.json
#        PER_SURFACE ITERS via <SURFACE>_ITERS (e.g. GBBQ_DECODE_ITERS=30)
set -euo pipefail
cd "$(dirname "$0")/../.."
export PATH="$HOME/.cargo/bin:$PATH"
ITERS="${1:-200}"; WARMUP="${2:-20}"
GEN3="${GEN3:-/home/box/retained-inputs/gen3}"
MANIFEST="${MANIFEST:-evidence/issue-4-update-gen3-acceptance/snapshot-manifest-gen3.json}"
SYM="sh.600000"
DAY_MEMBER='sh\lday\sh600000.day'

# ---- release oracle (dev profile would understate Rust) ----
BENCH="target/release/nm110_official_bench"
RLIB=$(ls -t target/release/deps/libx_data_mcp-*.rlib 2>/dev/null | head -1 || true)
if [ -z "$RLIB" ]; then
  cargo build --release --lib
  RLIB=$(ls -t target/release/deps/libx_data_mcp-*.rlib | head -1)
fi
if [ ! -x "$BENCH" ] || [ scripts/node_migration_p5/official_bench.rs -nt "$BENCH" ]; then
  dep() { ls -t target/release/deps/lib"$1"-*.rlib 2>/dev/null | head -1; }
  mkdir -p target/.p5-rust-tmp
  TMPDIR="$PWD/target/.p5-rust-tmp" rustc --edition 2024 -O \
    scripts/node_migration_p5/official_bench.rs \
    --extern "x_data_mcp=$RLIB" \
    --extern "zip=$(dep zip)" \
    --extern "chrono=$(dep chrono)" \
    -L dependency=target/release/deps -o "$BENCH"
fi

NB="node scripts/node_migration_p5/official_bench.mjs"
env_line() { BENCH_ITERS="$1" BENCH_WARMUP="$WARMUP" $NB env; }

# surface | rust args | node args | iters
run() {
  local surface="$1" rargs="$2" nargs="$3" n="${4:-$ITERS}"
  BENCH_ITERS="$n" BENCH_WARMUP="$WARMUP" "$BENCH" "$surface" $rargs
  BENCH_ITERS="$n" BENCH_WARMUP="$WARMUP" $NB "$surface" $nargs
}

env_line "$ITERS"
run zip_index        "$GEN3/hsjday.zip"           "$GEN3/hsjday.zip"                  "${ZIP_INDEX_ITERS:-$ITERS}"
run zip_member_read  "$GEN3/hsjday.zip $DAY_MEMBER" "$GEN3/hsjday.zip $DAY_MEMBER"    "${ZIP_MEMBER_READ_ITERS:-$ITERS}"
run gbbq_decode      "$GEN3/gbbq.zip"             "$GEN3 $MANIFEST"                   "${GBBQ_DECODE_ITERS:-30}"
run official_inspect "$GEN3 $MANIFEST"            "$GEN3 $MANIFEST"                   "${OFFICIAL_INSPECT_ITERS:-30}"
run materialize_warm "$GEN3 $MANIFEST $SYM"       "$GEN3 $MANIFEST $SYM"              "${MATERIALIZE_WARM_ITERS:-100}"
run materialize_cold "$GEN3 $MANIFEST $SYM"       "$GEN3 $MANIFEST $SYM"              "${MATERIALIZE_COLD_ITERS:-30}"
