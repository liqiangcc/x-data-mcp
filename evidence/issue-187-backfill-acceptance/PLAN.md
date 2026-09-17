# Issue #187 backfill acceptance — frozen plan (BLOCKED ON TEST-SUBJECT CONTRACT)

Status: `BLOCKED_ON_TEST_SUBJECT_CONTRACT`. The implementation components
(TdxGapVerifier, TdxBackfillProvider, worker dispatch) are delivered and covered
by in-repo tests on branch `issue-187-official-gap-backfill`. The standalone
evidence-script acceptance subject cannot be constructed honestly under the
current storage contract; see "Block" below. This plan freezes everything that
does not depend on that construction.

## Frozen inputs

Retained official files under `/home/box/retained-inputs/` (do not re-download):

| File | sha256 |
|---|---|
| `official-base.zip` | `dcdf6b49898926675698263535021aed7ad92b34be82a1a9e8a2a8982bd68bc6` |
| `official-hsjday.zip` | `c82f852a7bc2e52c7e89347a1e0d1878b01ce997946c2745f7af323752270892` |
| `official-gbbq.zip` | `daad6290c4f1cd478d7436b1e7077260a3e3e52cefd7eae58e4f24878fa2d0e9` |

Manifest: constructed per `tests/official_market.rs::retained_snapshot`
(schema `x-data-mcp.official-file-input.v1`, files in order base/day/gbbq with
the URLs frozen in that helper). Manifest sha256 as constructed:

`b31d2bb795299d3014b3c99b0b267ea9ef0556f5564aaf048125339a0213c7e7`

Extracted member bindings (produced by `inspect()` on the above):

| role | member | member sha256 |
|---|---|---|
| day | `sz\lday\sz000002.day` | `6f168cf053ca561fedde1aa97c3b38d71876854ade3dae5f1a2c9ba9f6b63826` |
| gbbq | `gbbq` | `4a3e9b862247a8a3fec406cc25b9a5a6ee3303f740780822519c99decaa09d77` |

## Frozen symbol and target

- Symbol: `sz.000002` (平安银行). Materialized QFQ history: 8479 bars,
  `2000-01-04` through `2026-09-14`.
- Target (gap) day: `2000-01-10` — a real trading day present in the source
  member.
- Official QFQ OHLC for `2000-01-10` (category-1 GBBQ events only; volume and
  amount intentionally absent — official units not evidence-anchored):

| field | value |
|---|---|
| open | `-8.14` |
| high | `-8.06` |
| low | `-8.15` |
| close | `-8.06` |

## Intended subject (what Phase A requires)

A published `market.daily` `EffectiveVersion` derived from real source bars
with exactly the `2000-01-10` bar removed, honestly labelled:

- `batch.coverage.status = Incomplete` with a reason naming the withheld date
  and marking the record an acceptance local-gap subject — it must never claim
  a complete source history.
- Valid `snapshot_inputs` bindings identical to the installed source's
  extraction (same manifest/day/gbbq member digests), valid integrity digests,
  published through the governed stage → prepare → commit path bound to a real
  accepted task. No raw SQLite manipulation.

## Block

No honest construction of that subject exists under the current storage
contract:

- `market_storage::compat::check` (src/market_storage/compat.rs) hard-rejects
  any record whose `batch.coverage.status` is not `Complete`
  (`coverage must be Complete`).
- `check` is enforced unconditionally on every governed path the subject would
  need: `SqliteMarket::stage` (write), `candidate`, `fixed` (read),
  `read_record` (read), and therefore `freeze_subject_on` during
  `verify_data_gap` acceptance. An `Incomplete` record cannot be staged,
  published, read back, or frozen as a verification subject.
- The alternative — `status = Complete` with a disclosing reason — remains a
  false machine-readable coverage claim, which the acceptance spec explicitly
  forbids.
- Relaxing `check` to admit honest `Incomplete` coverage would weaken the
  integrity gate for all records — a Final-Gate-level contract decision, not
  something this slice may change.

In-repo tests on the branch exercise the verifier/provider mechanics with a
scratch subject whose coverage `reason` discloses the withheld bar while
`status` stays `Complete` (the only construction the contract admits); whether
that form satisfies Phase A as-is or the contract needs a governed relaxation
is a Coordinator/Final-Gate call.

## Intended sequence (when unblocked)

1. `publish-local-gap-subject <db> sz.000002 2000-01-10` — governed scratch
   subject construction (real accepted task, materialized real bars minus
   exactly the target, honest Incomplete coverage, staged/prepared/committed).
2. `data_supply start_collection {op:verify_data_gap, scope:market.daily,
   symbol:sz.000002, expected_head:<subject>, frozen:{gap_target,subject}}` →
   `run-worker-once` with `X_DATA_MCP_OFFICIAL_MANIFEST` +
   `X_DATA_MCP_OFFICIAL_DIR` set to the retained snapshot.
3. `data_supply start_collection {op:backfill_data, scope:market.daily,
   symbol:sz.000002, contract_version:default, expected_head:<subject>}` →
   `run-worker-once`.
4. `query_data market.daily sz.000002` read-back.

## Predeclared assertions

1. `verify_data_gap` succeeds: one `gap_verification_decisions` row
   (`verifier_id=tdx-official-gap-verifier-v1`, conclusion
   `confirmed_missing_range`, basis bound to the `.day` member digest above)
   and one `proven_gaps` row covering `2000-01-10`.
2. `backfill_data` succeeds: a new published `market.daily` version restoring
   the `2000-01-10` bar with exactly the frozen QFQ values above; all bars
   outside the target identical to the subject's; original subject version
   retained; a `gap_coverings` row written by the default runner.
3. Negative legs: source-limited target → `source_limitation` decision, no
   gap; locally-present target → `no_local_gap`, no gap; mismatched
   subject/digest/overlap conflict → fail-closed error, no gap, no publish.
4. Env absent → `SOURCE_NOT_VERIFIED`, unchanged. Explicit-range
   `backfill_data` → still `SOURCE_NOT_VERIFIED` (OfficialRangeProvider).

## Honest limitations (frozen)

- QFQ values are negative by design (cumulative unverified GBBQ chain; Issue
  #166 follow-up). The acceptance asserts equality of restored values, not
  plausibility of their sign.
- `provenance.input_url`/`retrieved_at` are pinned to the inspected manifest
  file; they record snapshot acquisition, not live fetch.
- The exact `gap_id`/decision id binding inside `Evidence.revision_ref` is not
  visible to the provider layer; the ref binds the contract and authorizing
  inputs (base version/digest + symbol + frozen range). The runner supplies
  the frozen-gap enforcement and writes `gap_coverings`.
- The project remains `NOT_PRODUCTION_READY`; this acceptance does not change
  any gate.
