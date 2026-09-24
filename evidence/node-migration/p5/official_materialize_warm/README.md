# NM-114 — P5 slice: materialize warm-path (decimal QFQ arm)

Bounded optimization of the warm `materializeDaily` per-bar path in
`src_node/official/official_market.mjs` — byte-identical output verified.

## Profile (post-NM-081 trunk merge, pre-change)

`sh.600000` (6,393 bars, 27 GBBQ events) warm path, per-iter p50:

| phase | ms | share |
|---|---|---|
| `memberBytes` (day member read+CRC+sha256) | ~2.5 | 3% |
| `parseDay` (NM-081-improved) | ~1.7 | 2% |
| `qfqBars` → `forwardAdjust` + map | ~85.7 | 89% |
| `assembleDaily` | ~5.7 | 6% |

Inside `forwardAdjust` the per-price chain was
`decDiv(cents,100)` + `decMul` + `decAdd` + `roundAway` + `decToString`.
`decDiv` measured ~23.7 ms/25k ops — ~0.95 µs each, ~30× the cost of
`decMul` — the single most expensive op in the traversal.

## Change (one function — `MaterializeSession.qfqBars`)

Fused the `forwardAdjust` traversal with the final Bar map inside this
module, using the same exported `qfq_affine` primitives (`factors`,
`composeChain`, `decAdd`, `decMul`, `_dec.roundAway`, `decToString`) so
every decimal result is produced by the same code:

1. **`rawPrice` inlined** — `decDiv({m:c,s:0},{m:100,s:0})` ≡
   `c%100==0 ? {m:c/100,s:0} : unscale({m:c·10^7,s:9})`. Verified
   rep-identical ({m,s} pair equality) against `_dec.div` over 200,022
   cases (all c≤200k + 2^32-boundary edges like 167,772,160). The rep
   reaching `decMul` is bit-identical → every downstream op is
   byte-identical by construction.
2. **One regex per bar** — `ymdInt`'s ISO check and the Bar `date`
   object share the same match (previously two regexes per bar).
3. **No intermediate QfqBar objects** — the output Bar is assembled in
   the same pass.
4. Traversal semantics verbatim: u32 `date_raw`/`ohlc_raw` guards with
   identical RangeError texts, stable `date_raw` sort, partition-point
   walk with out-of-order fallback, strict-later chain boundary,
   `chain_event_count` semantics preserved via `dated.length - k`
   (internal only — not in the Bar contract).

`forwardAdjust`/`qfq_affine.mjs` untouched (other lane) — they remain
the reference implementation this function mirrors.

## Results (p50; `before.jsonl`/`after.jsonl`)

| surface | before (NM-110) | after | Rust |
|---|---|---|---|
| `materialize_warm` Node | 95.8 ms | **53.2 ms** | 4.9 ms |
| `materialize_cold` Node | 1,931 ms | 2,123 ms* | 153 ms |

\* cold is dominated by `gbbq_decode` (~1.88 s, other-lane code) — the
~40 ms improvement is inside its noise; not significant.

Same-run phase check: warm `materializeDaily` 91.4 ms → 53.2 ms
(post-merge baseline → post-change, this change alone ≈ −42%).

Warm-path ratio vs Rust: 19.6× → **10.9×**.

### Residual (structural, honest)

~48 ms/iter remains inside the fused traversal: per-price `decMul +
decAdd + roundAway + decToString` on `{m:BigInt,s}` decimal objects —
object + BigInt allocation churn (25.6k prices × ~4 intermediate reps)
plus V8 BigInt cost. The `{m,s}` protocol and the per-op rescaleFit
guards are the `algorithms/` lane's contract; deeper cuts need a
different decimal representation — out of scope/lane here.

## Parity gates (all actual exit codes)

- `node scripts/node_migration_p4/official_materialize_probe.mjs`
  → **47/47 PASS** (byte-exact vs Rust oracle).
- `node scripts/node_migration_p4/official_runners_probe.mjs`
  → **68/68 PASS** (acquire paths consume `qfqBars`).
- Golden: pre-change module (HEAD `eee0a96`) vs post-change —
  `materializeDaily` canonical JSON **byte-identical** on sh.600000 /
  sh.600004 / sz.000002 / sh.600519 (26,518 bars).
- `--selftest` both probes → exit 1 (negative contract).
- `node --check` clean. `cargo build --release --lib` unaffected
  (no Rust-side changes).

## Reproduce

```
export PATH="$HOME/.cargo/bin:$PATH"
MATERIALIZE_WARM_ITERS=200 MATERIALIZE_COLD_ITERS=30 \
  bash scripts/node_migration_p5/official_run.sh 200 20
```
