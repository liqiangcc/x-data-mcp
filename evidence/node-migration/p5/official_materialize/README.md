# NM-110 — P5 bench: official materialize + zip-inspect path

Baseline for the `src_node/official/` lane. All numbers are
sequential single-thread, fixed retained gen-3 bytes, warmup + N iters,
p50/p95/max + VmHWM. Same harness discipline as NM-072
(`scripts/node_migration_p5/`); see `results.jsonl`.

## Inputs / environment

- Fixtures: `/home/box/retained-inputs/gen3/` — `hsjday.zip` 550,106,783 B
  (12,423 members), `gbbq.zip` 6,337,960 B (member `gbbq`,
  5,974,990 B → 206,034 events), `base.zip`.
- Manifest: `evidence/issue-4-update-gen3-acceptance/snapshot-manifest-gen3.json`.
- Env line is the first row of `results.jsonl`: Box (overlayfs,
  inputs on `/home` — both impls read the same bytes), node v20.19.2,
  rustc 1.98.1, code `997bdad` (pre-change), concurrency 1.
- Rust oracle: `target/release/nm110_official_bench` — release-profile
  rlib + `rustc -O` (`scripts/node_migration_p5/official_bench.rs`).
  Node: `scripts/node_migration_p5/official_bench.mjs`.

## Results (p50)

| surface | iters | Rust p50 | Node p50 | Node/Rust |
|---|---|---|---|---|
| `zip_index` | 200 | 32.1 ms | 51.9 ms | 1.6× |
| `zip_member_read` | 200 | 35.0 ms | 54.3 ms | 1.6× |
| `gbbq_decode` | 30 | 75.6 ms | 1,878.7 ms | **24.8×** |
| `official_inspect` | 30 | 484.0 ms | 507.3 ms | 1.05× |
| `materialize_warm` | 100 | 4.9 ms | 95.8 ms | **19.6×** |
| `materialize_cold` | 30 | 153.4 ms | 1,931.5 ms | 12.6× |

## Surface definitions

- `zip_index` — EOCD scan + central-directory parse of `hsjday.zip`
  (550 MB file; only tail+CD read). Rust: `zip::ZipArchive::new(File)`
  (the call private `ArchiveIndex::open` wraps). Node:
  `ArchiveIndex.open(path)`.
- `zip_member_read` — index open + `by_name` member lookup + full
  deflate read + CRC + sha256 of `sh\lday\sh600000.day` (204,576 B
  output). Rust: `ZipArchive::by_name` + `read_to_end` + `hash::sha256`
  — the composition private `member_bytes` makes. Node: `ArchiveIndex`
  + `memberOpen`/`memberRead` + `sha256hex`.
- `gbbq_decode` — `decoded_gbbq` body minus the OnceCell: gbbq member
  read + `gbbq::parse` + `decode_events` + by-security fold + sha256.
  Rust: the same public calls verbatim (`x_data_mcp::gbbq`,
  `x_data_mcp::hash`). Node: fresh `MaterializeSession(source)` +
  `decodedGbbq()` per iter.
- `official_inspect` — `TdxMarketSource::inspect` end-to-end: manifest
  parse + per-file size/sha256 verification over all three archives
  (~572 MB hashed per iter — hence ~0.5 s).
- `materialize_warm` — `session.materialize_daily("sh.600000")` on a
  session with gbbq already decoded: day-member read + `.day` parse +
  QFQ chain + `assemble_daily` (6,393 bars; `input_bytes` column carries
  the bar count on this surface).
- `materialize_cold` — `source.materialize_daily("sh.600000")`: a fresh
  session per iter, so each pays index + gbbq decode + member read +
  parse + assemble.

## Findings (numbers only — no optimization done)

1. **I/O-bound surfaces are near parity.** `official_inspect` is
   1.05× — dominated by hashing 572 MB (Node's sha256 is
   native-backed); `zip_index`/`zip_member_read` sit at ~1.6×.
2. **The GBBQ decode loop is the structural gap — ~25×.** Node
   `decodeEvents` alone measured ~1.31 s of the ~1.88 s total
   (206,034 records × decrypt + `event_from_clear` + Decimal
   construction; plus ~94 ms `parseGbbq`, ~105 ms member read, remainder
   fold+digest). This is `src_node/parsers/gbbq_decode.mjs` — outside
   this lane's file boundary, no change made.
3. **Per-symbol materialize is ~20× on warm path.** Node warm
   `materializeDaily` = 95.8 ms vs 4.9 ms Rust (day parse + per-bar
   Decimal QFQ + assemble on 6,393 bars — `parsers/day_file.mjs` +
   decimal math are likewise other lanes' code).
4. Cold materialize (~1.93 s Node) ≈ warm + gbbq decode + index — the
   decode dominates; the archive index itself is not the bottleneck.
5. RSS: Node peaks higher (330–825 MB vs 10–47 MB) on decode/materialize
   — object-per-bar/event heap shape vs packed structs.

## Reproduce

```
export PATH="$HOME/.cargo/bin:$PATH"
bash scripts/node_migration_p5/official_run.sh 200 20 > results.jsonl
```

`--selftest`: n/a (bench, not a correctness probe) — the differential
correctness gate for this path is NM-075/NM-101 evidence.
