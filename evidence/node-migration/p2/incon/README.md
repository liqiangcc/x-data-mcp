# NM-024 P2 `incon.dat` sectioned dictionary parser — evidence

Baseline `f94b5e79c7601fc5c009e7a1e710fec6894f1a3f` (`migration/node-devin`). Node v20.19.2.

## Contents

- `probe-results.jsonl` — 54 rows, all PASS (R2: gen-2 authoritative member).
- `rust-oracle.jsonl` — live `rustc 1.98.1` + `x_data_mcp`/`encoding_rs` rlibs (`target/debug/incon_oracle`, not committed): 21 synthetic + 17 helper + member digest + 4 secmap rows.
- `results.json` — ported functions, semantics, member provenance.
- `SHA256SUMS` — over the three files (excludes itself).

## Reproduction

```sh
node scripts/node_migration_p2/incon_probe.mjs .
# Probe self-contains: outer official-gbbq.zip sha256 vs manifest,
# nested zhb.zip!incon.dat extraction (in-process zipMember), nested
# member sha256 checks. No /dev/shm reliance.
# Optional deb sample uses scripts/node_migration_p2/deb_member_extract.py
```

Module: `src_node/parsers/incon.mjs` (pure JS; strict GB18030 via `TextDecoder(fatal)`).

## Authoritative gen-2 member (REVIEW-NM-024 source correction)

`incon.dat` absent from both `base.zip` files. The correct gen-2 source per Rust `src/catalog_tdx/source.rs` is **`official-gbbq.zip!zhb.zip!incon.dat`**: outer `daad6290…` (manifest-verified at runtime), nested `zhb.zip` `e1ded566…` (1,411,229B), member `86f758b2…` (67,543B). Parsed: 41 sections incl. 20 `#####` empties + trailing, 3,662 entries, digest `fd14965e…` identical to live Rust.

Optional separate sample retained: client deb `tc/incon.dat` (65,853B, `ca941e57…`, 37 sections/3,554 entries) — different source, byte-equivalence only, **no gen-2 alignment claim**.

## Helpers verified

- `codeLevel`: len 3|5|7, **any** ASCII letter + digits → `(len-1)/2` (`t01`, `Z999999`, `A1234` verified), else null.
- `researchLevel2`: level 2 → self, level 3 → first 5 chars, else null.
- `sectionMap`: first matching name only (verified on 14 repeated `#####` sections).

## R1 reproducible extraction (REVIEW-NM-024)

`deb_manifest_sha` verifies the deb sha256 against `retained-inputs/manifest-sha256.txt` at runtime; `member_extract` runs the committed bounded extractor and verifies member sha256 `ca941e57…` before parsing. Separate-source limitation retained: deb member does not establish same-snapshot dictionary alignment with gen-2 catalog members.
