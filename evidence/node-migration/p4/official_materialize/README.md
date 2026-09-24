# P4 evidence — official source materialize (NM-075, S11a file-format half)

Differential verification of `src_node/official/official_market.mjs`
(TdxMarketSource::inspect / from_env / manifest_sha256 / archive_path /
materialize_session / materialize_daily + ArchiveIndex, member_bytes,
decoded_gbbq, security_events, day_names, qfq_bars, assemble_daily,
MarketDataPipeline::materialize_daily, merge_segment_bars) against a
Rust oracle linked to `x_data_mcp::official_market`. Same inputs; the
`MarketDataBatch` is compared byte-exact as `serde_json::to_string`
bytes, errors as `Display` text.

## Reproduce

```bash
cd /home/box/x-data-mcp-node-w4
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --lib
rustc --edition 2021 scripts/node_migration_p4/official_materialize_oracle.rs \
  --extern x_data_mcp=target/debug/libx_data_mcp.rlib \
  --extern chrono=$(ls target/debug/deps/libchrono-*.rlib | head -1) \
  --extern serde_json=$(ls target/debug/deps/libserde_json-*.rlib | head -1) \
  --extern serde=$(ls target/debug/deps/libserde-*.rlib | head -1) \
  -L dependency=target/debug/deps \
  -o target/debug/nm075_official_materialize_oracle

node scripts/node_migration_p4/official_materialize_probe.mjs \
  --out evidence/node-migration/p4/official_materialize          # exit 0
node scripts/node_migration_p4/official_materialize_probe.mjs --selftest
                                                                 # exit 1
```

Oracle: `scripts/node_migration_p4/official_materialize_oracle.rs`.
Fixture: retained gen-3 snapshot (`/home/box/retained-inputs/gen3`,
manifest `evidence/issue-4-update-gen3-acceptance/snapshot-manifest-gen3.json`,
manifest sha256 `1e8d1ca7…5311`) plus synthetic tamper roots generated
into `$TMPDIR` at run time — the synthetic gbbq member is built by
carving real encrypted records (one category-1 event for 600004, one
category-14 record for 600004, one record for 000001) so decode,
partition and the excluded-record count are exercised on real bytes.

## Result summary (this run)

- 47 cases: **47 pass, 0 fail** (33 differential vs oracle, 6 from_env
  vs oracle, 7 merge_segment_bars self-asserted — all `exact`
  byte/text compare except merge which asserts semantics).
- Real gen-3: `inspect`, `materialize_daily` for `sh.600004`,
  `sz.000002`, `sh.600000`, `sz.000001` (full batch JSON byte-equal,
  incl. snapshot_inputs bindings, acquired_at max(day,gbbq), coverage /
  unit-currency / adjustment evidence), shared-session `sess:` over two
  symbols, missing member (`sh.699999`, `sz.399999`), six unsupported-
  symbol rejection texts.
- Synthetic tamper roots (all verbatim `SourceError` text): gbbq member
  bad length, gbbq record non-UTF-8 decode failure, gbbq member absent
  (`found 0`), duplicate .day members (IndexMap dedup collapse —
  last-wins data proven via a distinguishable 1-record second member),
  both member-name spellings present (`found 2`), .day member absent
  (`found 0`), .day malformed (33 bytes), .day empty ("carries no
  records"), member CRC failure (`member bytes failed CRC: Invalid
  checksum`), non-zip archive (`invalid Zip archive: Could not find
  EOCD`), manifest sha mismatch (`artifact bytes do not match manifest:
  Gbbq`).
- Archive structure cases: a real zip64 chain (EOCD64 + locator +
  flagged EOCD32), prepended-junk archives (archive_offset detection),
  EOCD comment field, and a trailing bogus EOCD signature the backward
  scan rejects before falling through to the real EOCD.
- `from_env` 6-case matrix: unset vars, single-var, bad paths, and a
  valid pair — `Some`/`None` and manifest_sha256 parity.
- `merge_segment_bars` (private Rust fn — self-asserted): filling
  `None` optionals is enrichment; changing/losing known open/high/low/
  close/volume/amount/r7_raw fails `SOURCE_NOT_VERIFIED: source bar
  <date> conflicts with the fixed base`; output Map is key-sorted.

## Documented deltas (inside, not outside, the contract)

- `SourceError` is ported as an `Error` subclass carrying the Rust
  `Display` text verbatim (`SOURCE_NOT_VERIFIED:` / `SOURCE_UNSUPPORTED:`
  / `source invocation failed:` / `invalid source response:` /
  `SOURCE_TIMEOUT:` prefixes and inner wording). `io::Error` and serde
  wording inside those strings is a documented lane-wide delta already
  accepted by earlier slices (e.g. `archive open failed: ENOENT …` vs
  `archive open failed: No such file or directory (os error 2)`).
- ZIP structure parity includes zip64 EOCD/locator chains; member-level
  zip64 extra fields (sizes/offsets ≥ 4 GiB) are not interpreted — the
  official archives never use them and a crafted archive fails closed at
  the CRC/size checks with a possibly different inner error. Member
  names decode latin1 (official members are ASCII; cp437/UTF-8 flag
  handling is a documented delta).
- `merge_segment_bars`/`security_events_digest` are `pub(crate)` in
  Rust — no direct oracle; covered by the batch byte-compare (event
  digest is inside `snapshot_inputs`) and by self-asserted unit rows.
- `from_env` reads `process.env` (UTF-8); Rust reads `var_os` — non-UTF8
  env values are an unreachable edge here.

## Files

- `results.json` — summary + per-case records.
- `results.jsonl` — one record per case.
- `SHA256SUMS` — checksums of this directory's artifacts.
