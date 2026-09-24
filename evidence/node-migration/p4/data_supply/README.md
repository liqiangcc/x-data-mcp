# NM-063 — S12a data-supply wire types + registry + DataApi skeleton

Differential evidence for `src_node/data_supply/` against the Rust
`mcp_data` oracle (target/debug/mcp_data, SqliteSupply on a scratch
path — fails closed with STORAGE_UNAVAILABLE, which is exactly the seam
boundary the skeleton replaces).

## Lane

- `src_node/data_supply/wire.mjs` — serde_json::from_value-compatible
  decoders for every wire type in `src/data_supply/wire.rs` plus the
  registry-local (CapabilitiesQuery/WorkerInput), metrics::wire and
  cross-module verification request shapes needed by dispatch.
- `src_node/data_supply/access.mjs` — Access + market_storage::Error
  surface (StorageError kinds / Display).
- `src_node/data_supply/query.mjs` — QueryPlan::new + entities +
  page_size (pure pre-repository validation) + Queries seam.
- `src_node/data_supply/observe.mjs` — Observer cursor cache +
  tasks/progress/availability pre-repository validation + seam.
- `src_node/data_supply/registry.mjs` + `registry_descriptors.json` —
  the 20-entry descriptor table; schemas byte-captured from the oracle
  (regenerate via `gen_registry_descriptors.mjs`).
- `src_node/data_supply/mcp.mjs` — DataApi ToolHandler skeleton:
  dispatch, dataset/contract prechecks, local-owner gates, mapped() /
  task_error() / call() outcome_certainty overlay, RepositorySeam.
- `src_node/mcp/transport.mjs` — one semantic fix inside the NM-058
  lane: `call` arguments now keep F64 (serde from_value sees
  `floating point 1.0`; unwrapping to Number would have reported
  `integer 1`).

## Run

    cargo build --bin mcp_data
    TMPDIR=/dev/shm node scripts/node_migration_p4/data_supply_probe.mjs
    TMPDIR=/dev/shm SELFTEST=1 node scripts/node_migration_p4/data_supply_probe.mjs   # exits 1

## Results

218 request lines: **159 byte-identical, 0 failures, 59 accepted
divergence**. All 59 divergences are the repository-seam class:
identical `code` (storage_error) and `outcome_certainty`, message body
`STORAGE_UNAVAILABLE: unable to open database file: <scratch>` (oracle)
vs `STORAGE_UNAVAILABLE: repository seam not wired (S12c pending): <op>`
(skeleton). Typed fail-closed errors at the seam — never silent success.

Coverage: all 20 tools; tools/list byte-identical; initialize /
notification / missing-name / unknown-method / malformed-JSON envelopes;
serde decode classes (unknown/missing/duplicate-shape fields, invalid
type/value/variant, seq-form arity, internally-tagged enum quirks —
seq -> missing field, unit variants ignore extras, `{"v":null}` map
form, `expected string or map`); NaiveDate/DateTime chrono classes with
canonicalization; usize/u64/i32 boundaries incl. f64-overflow lexemes;
QueryPlan/observer pre-repo validation (selection, ranges, version maps,
entity dedup/sort/code, page_size, inverted ranges, expired cursor);
prechecks (unknown_dataset/unsupported_contract/unsupported_operation/
unknown_tool/empty_request_id); metrics::compute pre-repo validation;
get_data_capabilities (env-scrubbed) byte-identical including detailed
per-dataset examples and schema digests.

## Documented boundaries

- Repository-facing messages diverge by design (seam text vs sqlite-open
  failure); code/outcome_certainty/envelope identical.
- Restricted (non-local) Access paths are ported (tools filtering,
  requires_local, per-entity require) but not oracle-differentiable —
  mcp_data is local-only; covered by code review against mod.rs/mcp.rs.
- X_DATA_MCP_* env facts are scrubbed in the probe; env-set cases
  (generation root configured, official manifest installed) are ported
  but exercised only for the absent-env side.

## Artifacts

- `probe-results.jsonl` — per-line verdicts (pass/divergence/fail).
- `negative-selftest.jsonl` — injected byte flip + detection proof.
- `results.json` — counts and oracle identity.
- `SHA256SUMS` — checksums of this directory.


## NM-088 prep — real handler bodies (2026-09-24, w2)

`src_node/data_supply/mcp.mjs` `query.mjs` `observe.mjs` +
`src_node/metrics/service.mjs` `sqlite_metrics.mjs` — all DataApi
handler bodies ported verbatim over the SqliteSupply interface shape
(NM-074, in-flight in the w1 worktree) and the TaskControl service
surface (S6/S7). The `RepositorySeam`/`ControlSeam` classes remain as
the documented fail-closed defaults until those lanes land.

Evidence:
- `bodies-probe-results.jsonl` — 26 checks on the ported bodies
  (cursor LRU/key+access binding, page shapes, filters, progress
  counts, availability classes, acceptance/control arms, compute).
- End-to-end verified against w1's `sqlite_supply.mjs` on a
  `gap_fixture` DB: query_data/list_data_versions/list_tasks/
  get_task_progress/get_data_availability all returned real rows.
- `probe-results.jsonl` — 275-line differential unchanged (207 pass /
  68 documented seam divergences / 0 fail); matcher retires post-merge.
