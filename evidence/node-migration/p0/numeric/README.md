# NM-003 numeric/digest golden vectors — evidence

Baseline `32c29e0c57832b74925316ec8b601c16d5696fae` (`migration/node-devin`).

## Contents

- `vectors.jsonl` — 51 rows, one JSON per line: `{family,id,input,observed,semantics,source,oracle_sha,run_ref,provenance}`. Provenance split: 44 rows `observed_public_api` (actual return/error of the cited public function in run `nm003-oracle-run-1`), 6 rows `source_derived_expression` (the `amount_f32_projection` family — verbatim mirror of the private expression at `src/official_market.rs:396-397`; the enclosing `materialize_daily` env path is NOT_RUN), 1 row `justified_not_run` (`digest_rfc8785` reject/boundary — `serde_json_canonicalizer::to_string` is a total serializer over `serde_json::Value`, so no reject case exists in-domain; reject coverage lives at the `canonical_evidence` layer).
- `summary.json` — manifest: run record (command + exit code), family counts, observed vs source-derived split, NOT_RUN list, file hash.
- `SHA256SUMS` — sha256 over the above.
- Harness source (committed in repo): `scripts/node_migration_p0/oracle.rs`.

## Reproduction (Box)

```sh
cd <worktree>
TMPDIR=/dev/shm ~/.cargo/bin/cargo build --locked          # builds lib rlib
~/.cargo/bin/rustc --edition 2024 -C debuginfo=0 -L dependency=target/debug/deps \
  --extern x_data_mcp=$(ls target/debug/deps/libx_data_mcp-*.rlib | head -1) \
  --extern serde_json=$(ls target/debug/deps/libserde_json-*.rlib | head -1) \
  --extern serde=$(ls target/debug/deps/libserde-*.rlib | head -1) \
  --extern rust_decimal=$(ls target/debug/deps/librust_decimal-*.rlib | head -1) \
  --extern chrono=$(ls target/debug/deps/libchrono-*.rlib | head -1) \
  --extern serde_json_canonicalizer=$(ls target/debug/deps/libserde_json_canonicalizer-*.rlib | head -1) \
  scripts/node_migration_p0/oracle.rs -o target/debug/nm003-oracle
target/debug/nm003-oracle > vectors.jsonl                  # deterministic; diff vs committed
```

The harness reads one committed fixture file — `tests/fixtures/issue79/legacy-records.json` relative to the worktree root — for the legacy-digest vector; all other inputs are source literals (no env, no clock, no randomness).

Caveats:

- `amount_f32_projection` rows are `source_derived_expression`, not public-API output: the private expression `Decimal::from_str(&f64::from(f32::from_bits(bits)).to_string()).ok()` from `src/official_market.rs:396-397` is mirrored verbatim; the enclosing materialize path is NOT_RUN (needs env-pinned official snapshot).
- The `ratio_change` 512-bit integer budget cannot be reached from Decimal inputs (96-bit mantissa, scale ≤ 28 → ≤ ~230 bits); noted, not demonstrated.
- Binary lands in `target/debug/` (gitignored); output binary ~71MB — do not target `/dev/shm` (64MB cap).
