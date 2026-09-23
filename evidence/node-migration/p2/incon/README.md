# NM-024 P2 `incon.dat` sectioned dictionary parser — evidence

Baseline `f94b5e79c7601fc5c009e7a1e710fec6894f1a3f` (`migration/node-devin`). Node v20.19.2.

## Contents

- `probe-results.jsonl` — 49 rows, all PASS.
- `rust-oracle.jsonl` — live `rustc 1.98.1` + `x_data_mcp`/`encoding_rs` rlibs (`target/debug/incon_oracle`, not committed): 21 synthetic + 17 helper + member digest + 4 secmap rows.
- `results.json` — ported functions, semantics, member provenance.
- `SHA256SUMS` — over the three files (excludes itself).

## Reproduction

```sh
# extract the deb member (single file, no full deb unpack)
ar p /home/box/retained-inputs/official-client-com.tdx.tdxcfv_7.64_amd64.deb data.tar.xz \
  | python3 -c "import sys,lzma,tarfile,io; t=tarfile.open(fileobj=io.BytesIO(lzma.decompress(sys.stdin.buffer.read()))); \
    open('/dev/shm/incon.dat','wb').write(t.extractfile('./opt/apps/com.tdx.tdxcfv/files/tc/incon.dat').read())"
node scripts/node_migration_p2/incon_probe.mjs .
```

Module: `src_node/parsers/incon.mjs` (pure JS; strict GB18030 via `TextDecoder(fatal)`).

## IMPORTANT member-source caveat

The task assumes `incon.dat` exists in MANIFEST-verified `official-base.zip` — **it does not** (verified by full listing; gen-3 `base.zip` also lacks it). Real-byte comparison instead uses the **official client package's** `incon.dat` (65,853B, sha256 `ca941e57…`, MANIFEST-registered `.deb` input): 37 sections incl. retained `#####` empties, 3,554 entries, digest `e5e90ee7…` identical to live Rust. Flagged for Lead; no silent substitution made.

## Helpers verified

- `codeLevel`: len 3|5|7, **any** ASCII letter + digits → `(len-1)/2` (`t01`, `Z999999`, `A1234` verified), else null.
- `researchLevel2`: level 2 → self, level 3 → first 5 chars, else null.
- `sectionMap`: first matching name only (verified on 14 repeated `#####` sections).
