# NM-007 P1 codec validation — evidence

Baseline `ff1d5506309fde836ad77be936ca1914dec3a141` (`migration/node-devin`). Node v20.19.2.

## Contents

- `probe-results.jsonl` — one JSON line per golden vector: `{family,id,status,detail,expected,actual}` — 39 PASS / 12 NOT_RUN / 0 FAIL over the 51 accepted P0 rows.
- `results.json` — consolidated record: environment, probe command/exit, per-family coverage, boundaries.
- `SHA256SUMS` — over the two files above (manifest intentionally excludes itself).

## Reproduction

```sh
cd <worktree>
npm ci                 # canonicalize@2.1.0 (pure-JS cross-check), better-sqlite3@11.10.0
node scripts/node_migration_p1/codec_probe.mjs .
```

## Notes

- Golden inputs come from `evidence/node-migration/p0/numeric/vectors.jsonl`; opaque literal inputs (bar specs, evidence JSON) are mirrored verbatim from `scripts/node_migration_p0/oracle.rs` — that harness is the input spec.
- `jcs()` is a narrow RFC 8785 subset covering the full JSON value domain (null/bool/number/string/array/object, UTF-16 code-unit key order). It is cross-checked against the `canonicalize` package on every emitted canonical form; it is NOT claimed as a general serializer (bigint inputs reject; non-finite numbers reject).
- `serdeJsonBytes` equals serde_json::to_vec bytes only on this input domain (BTreeMap key order, ASCII-safe literals) — verified by digest equality against the legacy goldens.
- Arithmetic families (qfq affine/forward-adjust, yearly aggregate) are NOT_RUN: domain arithmetic is algorithm migration, not codec validation.
