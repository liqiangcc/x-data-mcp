# NM-090 — serde_json byte-level writer (P5 spike 2)

Byte-level emit path for `src_node/mcp/serde_json.mjs` `serValue` —
the deferred parity-risk slice of the 26.6x serde enc gap.

## Change

`serValue(v)` now emits into a growable `Buffer.allocUnsafe` writer
(`ByteWriter`), one `toString("utf8")` at the end. Also exports
`serValueBytes(v)` -> Buffer for byte consumers.

- Strings: one `charCodeAt` scan; escapable = C0<0x20 (escape table
  `\b \t \n \f \r \" \\` + `\u00xx` lowercase hex) plus **lone
  surrogates** -> `\udXXX` (JSON.stringify policy — serde_json cannot
  produce them since Rust strings are valid UTF-8; parity target is
  the prior Node impl). Valid surrogate pairs and all >=0x20 chars
  pass through to `Buffer.write(...,"utf8")`.
- Numbers: verbatim — integers `String(v)`, non-integers/F64 via the
  proven `serF64` (UNTOUCHED), bigint `toString()`.
- Keys: UTF-8 byte order == code-point order; alloc-free comparator
  (`charCodeAt`, falling back to `codePointAt` when either side holds
  a surrogate) — replaces `Buffer.compare(Buffer.from(k))` per
  comparison.
- Objects/arrays: iterative push into the same writer; declaration
  order = insertion order for objects (Object.keys), BTreeMap order
  via sorted keys.

## Parity evidence

- `serde_writer_probe.mjs`: **33 checks, 33 PASS** — NM-081 corpus
  (F64 -0/1e±300, bigint, toJSON, nested, ä/💩/C0 key+value escapes)
  plus adversarial edges (all 32 C0 chars, DEL/0x1f/0x20 boundary,
  lone lead/trail surrogates, surrogate-vs-BMP key ordering, empty
  containers, 200-deep nesting, number/F64 edges, undefined leaf,
  unicode key mixes, roundtrip stability).
- Full regression byte-identical: mcp_transport 175/0 fail,
  data_supply 275/0 fail (207+68 seam), early_bins 122/1 documented
  divergence, gap_verification 18/0 fail.
- `--selftest` exits 1.

## Perf (NM-072 harness slice — bench.mjs serde_json_encode surface,
identical fixture/timed/stats, same workdir, BENCH_ITERS=400)

| impl | p50_ns | p95_ns | mean_ns |
|---|---|---|---|
| old (recursive + map/join + Buffer-alloc keycmp) | 3,576,018 | 5,431,322 | 3,759,336 |
| new (byte-level) | 1,407,172 | 1,664,155 | 1,448,075 |

~2.5x faster on this surface (runs: 1346/1377/1385/1407 µs p50).
Rust reference (NM-081): 133µs — residual ~10x. The remaining gap is
the per-char escape scan + utf8 encode done in JS instead of C++;
further closing it needs a different mechanism (out of scope).
