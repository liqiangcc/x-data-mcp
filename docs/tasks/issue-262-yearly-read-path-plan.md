# Issue #262 — yearly/read-path performance plan

> 历史文档：描述 schema 20 版本链模型，被 [docs/latest-only-storage.md](../latest-only-storage.md) 取代（待合并）。

## Baseline (measured, preview.18 @ ECS 2vCPU/2GiB, schema-19, 5,221 entities)

| Phase | Cost | Evidence |
|---|---|---|
| yearly task accept (`YearlyContract::resolve`) | ~60–80 s per 100-item chunk | `fixed_many(&all_ids)` materializes every base daily version: delta-chain replay + full segment decode + legacy JSON envelope per item |
| yearly item execute (`derive_and_commit` + `derive_yearly`) | ~0.8 s/item (task sum 4,115 s / 5,221) | `fixed(base_id)` full materialize ×2 (runner + publisher) + `fixed(prior)` + `aggregate` over ~6,400 bars ×2 |
| full-market yearly end-to-end | ~2 h 57 m (52 serial chunks) | driver log / `phase-g-full-market-5221.json` |

Daily paths are already acceptable: first collection 147 ms/item, no-change
update ~63 ms/item, typed append fast path (S5G) is O(ops) — out of scope.

## Root cause

The yearly path authenticates a published daily base by *materializing* it
(`fixed()`/`fixed_many`), when it only needs:

- accept: symbol/period/version_id + `integrity_sha256` + the set of years
  present in the selected window;
- execute/verify: header facts + stored integrity digest + bars inside the
  selected years only.

For `{2025, 2026}` that is ~400 of ~6,400 bars per entity — ~16× wasted
decode, applied 3–4× per item (accept, runner, publisher, prior head).

## Design

Two read-surface additions on `MarketRepository`, dispatched by
`MarketBackend` (compact = typed row/clip reads; legacy = falls back to
`fixed()`, semantics preserved, perf unchanged):

```rust
/// O(1) published-version identity/integrity facts; no bar materialization.
fn fixed_facts(&self, id: &str) -> Result<FixedFacts, Error>;

/// Published read; `version().records` contains at least every stored bar in
/// [first_day, last_day] (compact restricts to the window; legacy may return
/// the full version). `raw()["integrity_sha256"]` is the STORED digest of the
/// complete version — a windowed raw is a projection, never canonical input
/// for staging/digesting.
fn fixed_window(&self, id: &str, first_day: NaiveDate, last_day: NaiveDate)
    -> Result<CheckedRecord, Error>;
```

Compact `fixed_window` = `leaf_bars_in_range` (existing verified windowed
leaf read: chain-metadata walk + per-clip digest check on overlapping
segments) + header/evidence/binding reads + `synthesize`, with
`raw.integrity_sha256` overwritten by the stored authoritative
`integrity_digest` (recomputing over a partial projection would be wrong).

### Call-site changes

| Site | From | To |
|---|---|---|
| `yearly_publication/tasks.rs` resolve | `fixed_many(bases+comparisons)` | `fixed_window` per base (window = min..max selected year), `fixed_facts` per comparison |
| `market_update/tasks.rs` resolve (explicit ranges) | `fixed_many` | `fixed_facts` per item |
| `data_supply/collect.rs` yearly resolve | `fixed` | `fixed_facts` |
| `yearly_publication/runner.rs` derive_and_commit | `fixed(base)`, `fixed(prior)` | `fixed_window(base, years window)`, `fixed_facts(prior)` |
| `yearly_publication.rs` derive_yearly | `fixed(base)` | `fixed_window(base, request.years window)` |
| `yearly_publication.rs` prepare_derived_yearly | `fixed(base)` | `fixed_facts` (digest compare only) |

Expected-yearly-head reads (`prepare_derived_yearly`, runner) stay on
`fixed()` — yearly versions are ~tens of bars, not a bottleneck.

### Semantics / fail-closed invariants

- Authentication is unchanged: accept and execution compare against the
  stored `integrity_digest` (committed at publish time), not a recomputed
  projection digest.
- `yearly::aggregate` receives only selected-window bars: `MissingYear`,
  ordering/OHLC and `as_of` validation are preserved for the inputs the
  result actually depends on; out-of-window bar validation was
  defense-in-depth, not authentication — the stored digest binds the whole
  version. Documented on the trait method.
- Year-presence at accept stays exact (decoded bars in window), not inferred
  from clip ranges (suspension holes would be missed).
- `validation.record_count` on a windowed projection reports the windowed
  count; the stored totals remain in `data_versions.record_count`.

## Targets

| Metric | Baseline | Target |
|---|---|---|
| yearly accept per 100-item chunk | ~60–80 s | <5 s |
| yearly item execute | ~0.8 s | <150 ms |
| full-market yearly (52 chunks, serial driver) | ~2 h 57 m | <15 min |
| regression | — | `derive`/`aggregate`/`verify` outputs byte-identical on fixed inputs |

## Verification

1. Unit: compact `fixed_window` returns exactly in-window bars + stored
   digest; `fixed_facts` matches `fixed()` fields; yearly roundtrip tests
   unchanged.
2. Equivalence: derive {2025,2026} via windowed vs full read on the same
   committed base → identical candidate raw.
3. Full suite `TMPDIR=/dev/shm`.
4. Measure: timing probe on the 11,783-symbol capacity DB (`fixed` vs
   `fixed_window` on real 30-year histories); optionally one ECS yearly
   chunk on the next preview deploy.
