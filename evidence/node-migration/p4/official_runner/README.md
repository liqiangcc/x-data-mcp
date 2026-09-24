# NM-101 — S11b official-runner decision layer (differential)

Port under test: `src_node/official/runner.mjs` — the decision layer of
`src/official_market.rs` (official runner) over the NM-075 source
surface: `TdxRangeProvider` (probe/probe_facts/probe_bound internally,
observable through `acquire`/`acquire_typed`), `TdxBackfillProvider`,
`TdxRepublishProvider`, `acquire_merged_segment`, `combined_version_record`,
`candidate_digest`, `unit_drift`, `TdxGapVerifier::verify`, and
`TdxDailyRunner::run_item`/`run_items` + `collection_candidate`.

Storage stays behind the typed `RunnerRepositorySeam` (unwired arms fail
closed) — the differential exercises the seam through a scripted
repository harness implemented identically on both sides.

## Result

**PASS — 68/68 byte-exact** against the Rust oracle
(`target/debug/nm101_official_runners_oracle`, compiled against the
workspace lib). Every compared value is the canonical serde-Value form
(sorted keys, NaiveDate/DateTime canonical strings, Decimals as strings).

## Commands

```
cd /home/box/x-data-mcp-node-w4
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --lib                                                  # exit 0
rustc --edition 2021 scripts/node_migration_p4/official_runners_oracle.rs \
  --extern x_data_mcp=target/debug/libx_data_mcp.rlib \
  --extern chrono=$(ls target/debug/deps/libchrono-*.rlib | head -1) \
  --extern serde_json=$(ls target/debug/deps/libserde_json-*.rlib | head -1) \
  --extern serde=$(ls target/debug/deps/libserde-*.rlib | head -1) \
  -L dependency=target/debug/deps \
  -o target/debug/nm101_official_runners_oracle                    # exit 0

TMPDIR=/dev/shm node scripts/node_migration_p4/official_runners_probe.mjs \
  --out evidence/node-migration/p4/official_runner                 # exit 0
node scripts/node_migration_p4/official_runners_probe.mjs --selftest  # exit 1 (negative test)
node --check src_node/official/runner.mjs                          # exit 0
```

## Coverage matrix

`acquire` (envelope `RangeProvider::acquire`, `TdxRangeProvider`):

| id | vector | continuity / outcome |
|----|--------|----------------------|
| 0 | base digests = full member | unchanged arm; `evidence.continuity.mode="unchanged"`; combined=null; batch=segment |
| 1 | base day sha = sha256(prefix K·32) | append arm; combined candidate present (merged history ≠ base) |
| 2 | append base, range = one base day | merged == base → combined=null, coverage pinned to base |
| 3 | bogus base day sha | unverified → full materialize; combined=null |
| 4 | events digest mismatch | unverified → full materialize |
| 5 | base unit_currency conflicts (confirmed EUR vs CNY) | unchanged arm + metadata drift → combined candidate |
| 6 | range past member end | `invalid source response: snapshot carries no bars inside the accepted range` |
| 7 | `snapshot_inputs: null` | unverified → full materialize |
| 8 | missing `integrity_sha256` | `MARKET_INTEGRITY: missing/mismatched legacy digest` (codec boundary) |
| 9 | `bj.600000` symbol | `unsupported symbol "bj.600000"; expected sh|sz + six digits` |
| 10 | prefix sha ok, base records later than prefix end | tail[0] ≤ last base date → unverified (fail closed) |

`acquire_typed` (`RangeProvider::acquire_typed`, `TdxRangeProvider`):

| id | vector | outcome |
|----|--------|---------|
| 11 | unchanged continuity | `TypedDecision::Unchanged` + continuity evidence |
| 12 | append continuity | `Append{upserts}` — exactly the 3 strictly-newer bars |
| 13 | append, narrow range covering base only | `Unchanged` (upserts empty) |
| 14 | unverified continuity | `null` → envelope fallback |
| 15 | unchanged + empty base_window | `invalid source response: snapshot carries no bars inside the accepted range` |
| 16 | facts unit drift (enrichment) | `null` → envelope fallback |
| 17 | facts coverage `Incomplete` | `null` → envelope fallback |
| 18 | append + coverage drift | `null` → envelope fallback |
| 19 | `bj.600000` facts | `unsupported symbol` |

`acquire_merged_segment` via `TdxBackfillProvider`/`TdxRepublishProvider`:

| id | vector | outcome |
|----|--------|---------|
| 20/21 | base + in-range slice | merged record; `revision_ref` set for backfill, null for republish |
| 22 | empty in-range | invalid-response error |
| 23 | `snapshot_inputs` null | `SOURCE_NOT_VERIFIED: base was not published from this snapshot` |
| 24 | overlapping bar conflicts base | `SOURCE_NOT_VERIFIED: known value conflict on <date>` |

`TdxGapVerifier::verify`:

| id | vector | outcome |
|----|--------|---------|
| 25 | subject lacks a day the source has | `confirmed_missing_range` basis (digest bound to member/manifest) |
| 26 | subject already has the day | `no_local_gap` |
| 27 | source member lacks the day | `source_limitation` |
| 28 | multi-day target | `MARKET_BASIS_NOT_VERIFIED: bounded gap verifier supports single-day targets only` |
| 29 | subject digest ≠ frozen binding | basis not verified |
| 30 | subject `snapshot_inputs` ≠ installed source | basis not verified |
| 31 | overlapping value conflict | `subject/source value conflict on <date>` |

`TdxDailyRunner::run_item` (scripted repo — outcome + op trace compared):

| id | vector | outcome |
|----|--------|---------|
| 32 | receipt exists | `Published(receipt)` — replay, no transitions |
| 33 | wrong contract | `INVALID_MARKET_COMMAND: unsupported collection contract` |
| 34 | foreign run id | `ACCEPTANCE_NEEDS_VERIFICATION: old run or unresolved execution` |
| 35 | item stage running | `PUBLISH_OUTCOME_UNKNOWN: running item has no verified result; …` |
| 36 | terminal + success outcome, no receipt | `MARKET_INTEGRITY: successful collection item has no receipt` |
| 37 | terminal + failed outcome | `NotPublished` (no transitions) |
| 38 | cancel_requested | finishExecution → `NotPublished` |
| 39 | pending→claim→startItem→materialize→stage→prepare→commit | `Published` — full happy path incl. `stage:`/`candidate:`/`mark_validated:`/`commit:` trace order |
| 40 | effective_head exists | Invalid → finishItem(failed) → finishExecution → `NotPublished` |
| 41 | commit Storage error | → collection_receipt recheck → rebind → RequireVerification → rethrow storage error |
| 42 | commit Storage error, receipt appears on recheck | `Published` (replay wins; never overwrite success) |
| 43 | task_head shows cancel_requested mid-run | Conflict → finishItem("cancelled before publication: …") → finishCancellation → `NotPublished` |
| 44 | binding absent | `task_head` probe → `INVALID_MARKET_COMMAND: collection item absent from accepted task` |

`TdxDailyRunner::run_items` (batch path; `cand:` lines hash each
`CollectionCandidate`'s full serde-Value form — content-bound compare):

| id | vector | outcome |
|----|--------|---------|
| 45 | two pending items, batch commit ok | `some` — candidates produced, single batch commit, per-item receipts |
| 46 | `commit_collection_batch` → None | `null` — every item falls back to `run_item` |
| 47 | per-item `err` result | that item falls back to `run_item`, other committed |
| 48 | batch commit throws | all items fall back to `run_item` (isolation) |
| 49 | terminal head → candidate null | falls back to `run_item` → `NotPublished` |

## Notes / deliberate deltas

- `probe`/`probe_facts`/`probe_bound` return a private `Continuity` enum —
  not externally nameable, so dispatch is compared through
  `acquire`/`acquire_typed` evidence (`continuity.mode`, `day_prefix_bytes`)
  and outcomes, which proved byte-exact.
- `Publisher::prepare` is a market_storage arm: the shipped code calls the
  injected `repository.publisher_prepare` seam; the probe's scripted repo
  mirrors prepare's call order + `check_task_binding` guards so the op
  trace (stage→candidate→task_item_binding→mark_validated→commit) is
  compared. `Publisher::prepare` fidelity itself is w1's lane — NOT_RUN
  here beyond the boundary trace.
- `GapBasis.observed_at` field is serde `Z`-form; the content digest feeds
  chrono `to_rfc3339()` (`+00:00` form) — the port digests the rfc3339
  view, keeping both parities.
- `decide_head` is `pub(crate)` — unreachable from the oracle; the
  scripted repo declares transition effects and the compared artifact is
  the runner's call order/args/outcome (task_control semantics are the
  `storage/task_repository.mjs` lane, already ported).
- `FabricatedAppendProvider` (measurement harness, test-only) is deferred
  — not a production path.
- No storage writes exist: `RunnerRepositorySeam` throws typed
  `MARKET_STORAGE: official runner repository seam not wired …` on every
  storage arm.

## Selftest

`--selftest` exits 1 unconditionally (negative contract check). Fixture
isolation: per-run `mkdtempSync` scratch under `$TMPDIR`, removed on exit
(0 leftovers); gen-3 inputs read-only.
