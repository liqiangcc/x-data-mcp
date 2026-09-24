# NM-091 — serde_json decode path optimization (P5 spike 3)

Optimized the `parseJsonBytes` hot path in `src_node/mcp/serde_json.mjs`
— byte-identical results AND error messages/positions.

## Changes

- Whitespace scan: `Set.has` -> `Uint8Array(256)` table.
- Success path unboxed: internal methods return values directly;
  failures throw `ParseError` (verbatim message + eofKind for the
  container key-EOF remap). Public `run()` contract unchanged.
- `string()`: prescan with a local cursor — a clean string decodes in
  one `toString` with no parts[] machinery; `\\`/C0/EOF falls into
  `stringSlow` with byte-exact error positions preserved.
  (Rejected alternative: `buf.indexOf(0x22)` + `subarray().indexOf`
  fast path — two native passes + a view alloc lost to the byte loop
  on short-string-heavy docs; measured ~20% SLOWER than baseline.)
- `number()`: integer digits accumulated as Number while scanning;
  BigInt only for >15-digit lexemes (<=15 digits always < 1e15 < 2^53
  -> exact). Float path verbatim (`Number(lexeme)` + range check).
- `container()`: identical decide flow, unboxed.

## Parity

- `serde_decode_probe.mjs`: **91/91** byte-identical vs the
  git-baseline parser — all error classes with exact columns
  (EOF variants, post-comma contexts, trailing comma, control char,
  bad/lone-surrogate escapes, leading zero, out-of-range, recursion
  limit, duplicate keys) + value corpus incl. F64/BigInt edges.
- Regressions unchanged: mcp_transport 175/0 fail, data_supply
  275/0 fail (207+68 seam), early_bins 122/1 doc divergence,
  gap_verification 18/0, serde_writer 33/0.
- `--selftest` exits 1.

## Perf (NM-072 harness slice, verbatim timed/stats, 136057B doc,
BENCH_ITERS=400, /dev/shm)

| impl | p50_ns | p95_ns | mean_ns |
|---|---|---|---|
| old  | 1,980,091 | 2,798,713 | 2,121,983 |
| new  | 1,221,747 | 2,099,897 | 1,341,761 |

~1.6x on the decode surface (repeat runs: 1212/1219/1222us p50 vs
2047/2060/2107us). Rust reference 562us — residual ~2.2x.

## Incidental fix

`gap_fixture.mjs` no longer trusts the shared `/dev/shm/nm006/
db18.sqlite` skeleton — another lane had polluted it with committed
versions (incl. `nm052-result`), breaking every consumer probe. The
fixture now seeds a private skeleton via `target/debug/task_sqlite
init` per probe run (mirrors sqlite_market::tests::setup()'s fresh
schema-18 db). Not a serde change — a shared-scratch isolation fix.
