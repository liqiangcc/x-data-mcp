# NM-070 — serde from_value consolidation + metrics wire types

## Scope

- Consolidated the serde `from_value`-equivalent decoder machinery into
  `src_node/mcp/serde_value.mjs` (single home for Decode, text
  primitives, chrono scanners, and the t* combinators). Consumers:
  `src_node/data_supply/wire.mjs` (type tables), `src_node/metrics/*`,
  and `src_node/bin/_consistency_input.mjs` (shared text primitives +
  `parseDateTime` classifier for the positional `from_slice` decoder).
- `src_node/metrics/wire.mjs` — the full `metrics/wire.rs` request and
  output surface (Compute..Computation, Outcome internally-tagged).
- `src_node/metrics/sqlite_wire.mjs` — `sqlite_metrics.rs` decode-side
  shapes (AnalysisCoverage/Origin/YearCoverage/Coverage +
  CoverageEvidence/EmbeddedEvidence) — decode parity only.
- `src_node/metrics/service.mjs` — `Metrics::compute` pre-repository
  validation ported verbatim; `repository.observations()` is the typed
  seam (StorageError, never silent).

## Decoder generalizations landed this slice (oracle-verified)

- Internally-tagged enums decode from sequences: `[tag, ...fields]` —
  element 0 is the variant identifier; remaining elements bind
  positionally. Under-length: `invalid length N, expected struct
  variant E::V with M element(s)`; over-length: `invalid length N,
  expected M element(s) in sequence` (remaining count); unit variant
  extras: `invalid length N, expected 0 elements in sequence`.
- Content-buffered struct seq over-length inside tagged enums reads
  `expected N element(s) in sequence`; the direct-Value path keeps
  `expected fewer elements in array`.
- `RangeWrite::payload()` admission checks (`items` XOR `selection` ->
  `INVALID_COMMAND`, neither -> `items or selection required`) ported;
  `command.key()` contract versions wired (`explicit-range-task-v1`,
  `default-range-task-v1`, `explicit-republish-task-v1`,
  `explicit-yearly-task-v1`).

## Corpus deltas vs NM-063

+57 scenario lines: nested-path decode vectors (wrong types, missing and
unknown fields, unknown variants inside `selection`/`filter`/`version`),
internally-tagged seq forms (Selection/Filter/VersionChoice/Collection/
WriteSelection shapes incl. unit-variant seq), struct-in-buffered seq
over-length, and the `compute_metrics` validation corpus (input/metric
cardinality, empty/mismatched ids, `latest` rejection, 64-hex digest,
entity validation, `market.yearly` requirement, year range bounds,
1..=1000 `min_periods`, seq-form Compute, defaults).

## Verification

- `data_supply_probe.mjs` (extended corpus): 275 checks, 207
  byte-identical, 68 accepted seam divergences (identical code +
  outcome_certainty; message is the repository seam text), 0 fail.
  results.json + probe-results.jsonl in this directory.
- Selftest: `SELFTEST=1 node data_supply_probe.mjs` exits 1.
- Regression: `mcp_transport_probe.mjs` 175 checks 0 fail;
  `early_bins_probe.mjs` 122 pass / 1 documented divergence (bytes >
  2^53 Number.isSafeInteger boundary, deferred); both selftests exit 1.

## Boundaries

- `repository.observations` seam — SqliteMetrics not ported (S12c).
- `evaluate` + `Computation` assembly unreachable behind the seam.
- Restricted-access compute paths ported but not oracle-differentiable
  (mcp_data is local-only).
