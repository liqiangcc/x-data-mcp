# NM-104 — F-001-class audit: w2 lane wire-leak sweep

Date: 2026-09-24. Branch: `migration/node-devin-w2`. Baseline: `9ad2cb5`.
Scope: every u64/usize/u32-shaped value crossing a serialized boundary —
both directions (sqlite `safeIntegers` bigint rows inbound; JS-constructed
bigint outbound). `JSON.stringify(bigint)` throws `TypeError`; serde emits
u64 digits. Lossy `Number()` coercion above 2^53 is the same class
(silent digit corruption instead of crash).

## Real leaks found and fixed

### L1 — `ser()` scalar leaf: `JSON.stringify` crash on bigint (latent)

`src_node/storage/verification_storage.mjs` — `ser(v, shape)` is the
serde-canonical serializer feeding audit/digest/snapshot writes
(`ser(target,"snapshot")`, `ser(command,"command")`, `ser(value)`
terminal binds, `writeTaskSnapshot` outcome_json). Any free-`Value`
payload (`task.result`, `evidence`, `previous_result`, intent fields)
carrying a bigint crashed serialization; u64 digits >2^53 stored as
text were also corrupted on JSON.parse→re-encode.

Fix: `ser()` gained bigint→digits and F64→lexeme arms. All stored
`JSON.parse` sites on serde-typed columns switched to the new shared
`parseJsonValue` (serde-faithful: u64-exact bigint, F64 lexemes) in
`task_repository.mjs`, `verification_storage.mjs`,
`publication_checks.mjs`, `verification_inspection.mjs`,
`verification_facts.mjs`, `market_read.mjs`, `market_commit.mjs`,
`market_write.mjs`, `compact_read.mjs`, `compact_write.mjs`,
`collection.mjs`, `yearly_publication/repository.mjs`,
`request_repository.mjs`, `gap_verification/store.mjs`.

Write-side `JSON.stringify` on free Values swapped to `ser`/`serValue`:
`outcomeJson` (both sites), `execution_uncertainty` marker,
`headResultJson` (replaces `deepSortValue`), registration descriptor
(`ser(d,"descriptor")`), facts `evidence_sha256` digest inputs,
`market_read` resultDigest/evDigest/bars/compare sites,
`market_commit` command/receipt revision fields,
`record_codec.sortSer`, `compact_market.jcs` (F64 arm; bigint already
armed), `yearly_results` resultDigest, `publication_checks` deep-copy
(now serde round-trip `parseJsonValue(ser(v,"snapshot"))`),
`verification_inspection` clones, `yearly_pub` bound read.

### L2 — `tInt` decode: silent u64 digit loss >2^53

`src_node/mcp/serde_value.mjs` — `tInt` returned `Number(i)` for every
in-range int; serde `u64` accepts the full range exactly. Fixed: values
beyond ±2^53 keep `bigint`; serde error texts unchanged. All consumers
verified bigint-safe (bounds compares, `BigInt()` revision coercions,
sqlite params, serValue emits digits).

### L3 — probe sink crash: `fault_helpers.out()` (live F-001)

`scripts/node_migration_p4/fault_helpers.mjs` — `out(v)` used raw
`JSON.stringify`; `syncStatus()`/`dbRows` payloads carry u64 fields
(`bytes_done`, counters) as bigint via `safeIntegers`. The `mid_verify`
fault scenario crashed Node with exit 42 `Do not know how to serialize
a BigInt` where Rust completed. Fixed: `out` → `serValue`. Scenario now
MATCH (8/8, 0 divergences) — this is the crash-then-fix witness.

### L4 — `publication_verification/store.mjs` local `sortedJson`

Same raw-`JSON.stringify` leaf; deleted, uses `ser` (serde-canonical,
bigint-armed).

## Audit table — u64/usize/u32 fields at serialized boundaries

| field / surface | source | sink | why safe now |
|---|---|---|---|
| `acquisition_tasks.bytes_done` (u64) | sqlite bigint | `serValueSorted`/`serValue` in `sync_status`, task views | bigint arm → digits (was armed; probe-harness sink was the leak — L3) |
| `task.state_revision`/`accepted_run_id`/`owner_run_id` reads | sqlite bigint | `ser(..,"snapshot")` via `snapshotCanon` → `Number()` | canon coerces small revs; ser emits bigint digits if huge |
| item `outcome_json` (`success`,`error`,`evidence` Value) | caller Value | `ser(o,"outcome")` — struct order | bigint/F64 leaves emit serde bytes (L1) |
| `execution_uncertainty.previous_result` (Option<Value>) | stored result_json | `ser(marker)` | Value → serde bytes; bigint→digits (L1) |
| `result_json` head Value | parsed/counter | `ser(r)` / counter `resultJson` | free Value → ser; counters are Numbers |
| `command_json`/`work_json`/`frozen_inputs_json`/`target_json`/`scope_keys_json`/`terminal_json`/`audit_json`/`command_key_json`/`descriptor_json`/`envelope_json`/`bar_json`/`receipt_json` | stored serde text | `parseJsonValue` reads | u64 digits ≥16 chars → bigint (exact); re-encode identical — no lossy `JSON.parse` |
| request decode `expected_revision`,`state_revision` (u64) | wire | `tInt` → Number ≤2^53 else bigint | exact serde u64; sinks never see Number-loss (L2) |
| `page_size`,`min_periods`,`record_count`,progress counts (usize/u32) | wire | `tInt` | same; consumers compare/budget only |
| `start_year`,`end_year`,`year`,`scale`,`years[]` (i32/u32) | wire | `tInt` | always ≤2^53 → Number |
| MCP request args / cursors / digests | wire | `serValue` | bigint→digits, F64→ryu, sorted keys |
| MCP responses (`structuredContent`, text content) | service results | `transport.mjs` `serValue` | single sink, bigint-safe |
| `_consistency_input` `bytes` (u64) | input file | `decU64` → bigint; output `serPretty` | bigint arm → serValue digits (NM-077) |
| `fault_helpers` `out()` | probe results | `serValue` | was raw JSON.stringify → crashed on bigint status (L3) |
| `data_supply` `serValue` digests/cursors; `mcp.mjs` ApiError `range_conflict` message | args/scope strings | `serValue` / `JSON.stringify(string)` | strings only — no numeric reach |
| `gap_verification` `basisContentDigest` | basis strings | `JSON.stringify(string[])` | string fields only |
| `gap_verification` receipt/decision views | canon/ids | `serValue` via transport | strings + canon Numbers |
| `yearly_publication` digest `sha256(JSON.stringify([task_id,itemId]))` | strings | — | strings only |
| `verification_runtime` `descriptorJson`/`fileIdentity` | `fstatSync` | `ser(d,"descriptor")` | `Number(dev/ino)` coerce at source; serde order shape |
| `compact_market.jcs` evidence canon | evidence Value | `jcs` | bigint arm pre-existing; F64 arm added |

## Not fixed (out-of-lane, documented)

- `src_node/acquire/store.mjs` `JSON.parse(serValueSorted(...))`
  round-trip and `JSON.stringify(outcome)`: a u64 >2^53 inside a stored
  outcome loses digits (corruption, not crash). Not reachable for real
  file bytes (files < 2^53); w3's lane — flagged for owner review.
- `local_runtime/paths.mjs` identity writes: `fstatSync` Numbers —
  inode >2^53 would lose digits; unreachable on supported fs (fail-
  closed runtime guard) — documented.
- `diff_runner` scenario JSON.parse on op args — harness-side, op
  literals can't express >2^53 anyway — documented.

## Verification

- `bigint_wire_probe.mjs`: 16/16 PASS (ser bigint leaf, nested, F64,
  decode u64 exact, parseJsonValue round-trip, finishItem evidence
  bigint write→read-back→re-encode byte-identical, marker write path,
  serValue control). `--selftest` exits 1.
- Baseline crash witness: `JSON.stringify({v:2^60+1n})` →
  `TypeError: Do not know how to serialize a BigInt`; post-fix emits
  `{"v":1152921504606846977}` (serde digits).
- `fault_diff_probe.mjs`: **8/8 MATCH** — `mid_verify` went
  crash(exit 42)→MATCH. Built `target/fault_oracle` via rustc
  (+libx_data_mcp/serde_json/rusqlite/sha2/libc externs); RUST_BIN=
  `target/debug/acquire_service`.
- Regressions: pubcheck 46/0, gap 18/0, data_supply 275/0 (68 doc'd
  seam divergences), early_bins 123/0, transport 175/0, mcp_connect
  16/0, serde_writer 33/0, serde_decode 91/0, yearly_pub 38/0,
  diff_harness 33 scenarios/0 diffs (incl. verification_commit 77 ops,
  pubcheck_tail 25 ops, all market_* tamper arms), all P3 probes pass.
