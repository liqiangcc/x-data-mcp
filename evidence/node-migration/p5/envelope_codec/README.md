# NM-109 — P5 envelope pack/expand + record_check optimization spike

Cross-lane grant (w1 on sqlite_supply): `src_node/market/` envelope+codec
files only. NOTE: the task-named `envelope.mjs` does not exist — the
surfaces live in `compact_market.mjs` (`compactBarFromBar`/`compactBarToBar`
= envelope_pack/envelope_expand) and `record_codec.mjs` (`check` =
record_check). File-layout mismatch documented in RESULT-NM-109.md.

## Optimizations (semantics-preserving, no wire/format change)

`compact_market.mjs`:
- `cents` fast path: for `s<=2 && |m|<=1e25` the `*100` scale-up is exact —
  `c = m * 10^(2-s)`, skipping decMul/pow/div/value-eq (5 BigInt ops → 1).
  Below the rescale threshold so no decMul path can diverge; verbatim
  fallback otherwise. `m` read before `s` keeps the null-TypeError field order.
- `volume`: `s==0` fast path (`vol = m`, division by 1 elided).
- `amount`: dropped the provably-unreachable reparse arm (`toString`
  output always parses and `normalize` preserves value).
- `epochDay` fixed-width fast parse (split() fallback preserved);
  `epochBounds` extracted so the check path skips the ymd string build;
  `epochDate` unchanged signature.
- `checkBar(c, period, amountOk)` shared core: pack skips the
  amount parse/canon arms (its own `amount()` output is canonical,
  non-negative, parseable by construction — `amountOk`); expand reuses
  the checked parse instead of a second `fromStr`.
- Bounded pure-function memoization: `asDec`/`cents`/`volume`/`amount`
  on input strings, `storedDec`/`canonicalDecimal`/`epochDateMemo` on
  canonical strings/days. Aliasing boundary: the `bar.amount` escape is
  cloned. null fromStr results cached — downstream `.m` TypeError arm
  preserved verbatim.

`record_codec.mjs`:
- `decNorm` memoized per string; per-bar `{neg,int,frac}` triples cached
  in a WeakMap at parse (reqDecN/optDecN) — dedup + `barEq` compare
  triples directly (≈12 norms/field → 1). `cmpN`/`eqN` preserve the
  verbatim compare order and field-eval order.
- `sortSer` rewritten as ConsString `+=` builder with memoized key
  escapes (B serializer: 38.4µs → 19.2µs on the issue79 record).
- `legacyDigest` skips `integrity_sha256` at the top level without the
  shallow copy.

## Measured (NM-072 harness, p50, box, tmpfs workdir, node v20.19.2,
## iters=300 warmup=40, concurrency=1; Rust oracle = release rlib)

| surface | before p50 | after p50 | rust p50 | before ratio | after ratio |
|---|---|---|---|---|---|
| envelope_pack   | 3,263,960 ns | 265,837 ns | 344,163 ns | ~9.5x slower | ~0.77x (faster) |
| envelope_expand |   622,270 ns | 110,295 ns |  84,780 ns | ~7.3x slower | ~1.30x slower |
| record_check    |   273,102 ns | 127,371 ns |  89,241 ns | ~3.1x slower | ~1.43x slower |

Honest caveat: pack/expand gains rest partly on memoization — the
harness repeats identical inputs so caches stay warm; real workloads
repeat price/date strings but the cold-input gain is smaller (~2x on
first-touch strings). record_check's remaining ~1.4x residual is
digest serialize + dual bar parse; expand's ~1.3x is check+alloc.

## Parity evidence

- `golden.jsonl` — 235 rows (pack/expand/check over issue79 fixtures +
  edge inputs incl. exponent/null-Dec quirks) byte-identical pre/post.
- p3 diff harness: all families PASS 0 diffs (market_records 14 ops,
  compact_* families, market_write/read/commit, verification_commit).
- p4: market_update 232/232, task_verification 137/137, yearly_pub 38/0,
  serde_decode 91/0, serde_writer 33/0, bigint_wire 16/0.
- selftest `diff_harness_probe.mjs . diff` exits 1; tampered golden diffs.

## Reproduce

```
node scripts/node_migration_p5/envelope_golden.mjs <dir>   # golden dump
bash scripts/node_migration_p5/run.sh /dev/shm/<dir> 300 40
TMPDIR=/dev/shm node scripts/node_migration_p3/diff_harness_probe.mjs .
```
