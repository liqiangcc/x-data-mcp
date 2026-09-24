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


## NM-113 — round 2: emitted-bytes memoization

Profile of the 1407µs residual (NM-072 corpus, 7503 strings / 110K
chars / 0 dirty): utf8 writes ~683µs (fixed ~120ns Buffer.write call
overhead dominates ≤64-char strings), escape scan ~500µs (JS
charCodeAt loop), key sort ~213µs, walk ~38µs.

Rejected (measured): per-string regex cleanliness gate (+10% — call
overhead on ~15-char strings), JSON.stringify-as-str (+51% — UTF-16
round-trip), TextEncoder.encodeInto (+22% — subarray alloc),
buf.latin1Write / bigger initial buffer / pair-level memo (noise or
worse — the pair-key concat alone costs more than the saved lookup),
WeakMap subtree memo (breaks byte-identity under mutation — unsafe).

Landed: content-addressed emitted-bytes memoization —
  STR_ENC: string  -> '"'+escaped+'"'  (content <= 128 chars)
  KEY_ENC: key     -> '"'+escaped+'":' (fused colon)
Cached bytes are exactly what the un-memoized path emits — output is
byte-identical by construction. Bounded: 4096 entries each (~2MB max);
over-cap strings encode inline every time. Buffer.set on a hit is one
memcpy (~46ns) vs scan+utf8-write (~180ns) — corpus keys/values repeat
~3500/4000 times.

## Perf (same surface, BENCH_ITERS=400, code ac2ebac -> this commit)

| impl | p50_ns (median of 5 runs) |
|---|---|
| pre-NM-113 | 1,408,704 |
| post-NM-113 | 674,799 |

~2.1x on the NM-072 encode surface. In-process interleaved
(vs git baseline): repeat corpus 1.91x, escape-heavy 2.91x,
all-unique 1.23x (keys still hit; misses pay only Map.get+copy).
Rust reference 133µs — residual ~5.1x, structural floor: the
per-entry Map.get+memcpy + object-key sort + tree walk are the JS
floor without mutation-unsafe whole-subtree caching (rejected —
byte identity must survive caller mutation).

## Parity evidence

- serde_writer 33/33, serde_decode 91/91, mcp_transport 175/0,
  mcp_connect 16/0, early_bins 123/0, data_supply 275 (207+68 seam,
  0 fail), data_supply_bodies 26/0, gap_verification 18/0.
- Byte-identical vs git baseline on the parity corpus: full doc,
  all 128 ASCII chars, every surrogate/escape edge, >cap keys/values,
  F64/bigint/scalars, 200-deep nesting, toJSON strings — 11 case
  families, 0 diffs.
