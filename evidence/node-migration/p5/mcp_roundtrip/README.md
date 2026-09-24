# NM-115 — mcp_roundtrip 8.6x diagnosis + fix

Surface: `handleJson(stub, line)` on an 83-byte `tools/call` line —
pure in-process (parse + dispatch + envelope + serialize; NO socket,
NO worker hop in this bench).

## Attribution (pre-change, this box, warm)

| stage | ns | share |
|---|---|---|
| `parseJsonBytes` (105B line) | ~1,880 | 24% |
| `serValue` envelope + inner text | ~5,700 | 74% |
| dispatch/unwrap/object build | ~300 | 4% |

Root cause of the serValue term: `new ByteWriter()` ran
`Buffer.allocUnsafe(1<<16)` per call — a real ~2.1µs malloc, twice per
response (content text + envelope). `serValue({ok:true})` alone was
2,045ns — ~all alloc.

## Fix (src_node/mcp/serde_json.mjs)

- `serValue` reuses a pooled ByteWriter (`w.p=0`; `text()` copies the
  emitted span so nothing escapes; reentrant toJSON->serValue sees the
  pool empty and allocates fresh; >512KiB buffers aren't pooled).
  `serValueBytes` keeps fresh alloc — its subarray aliases the buffer.
- Decode side: clean-string path uses `latin1Slice` when the prescan
  saw only <0x80 bytes (identical output for ASCII spans; ~90ns vs
  ~120ns `utf8Slice`).
- `ident()` string-iterator -> index loop (same per-char error
  positions).

## Numbers (same harness)

- bench.mjs timed shape (40 warmup): p50 25,589 -> 13,118 ns (1.95x).
  Residual box noise is large; deep warmup (2000) settles ~3.1us.
- In-process interleaved (stable): 5,624 -> 3,107 ns (1.81x).
- Rust oracle (debug rlib, same bench): 5,111 ns — Node warm ~3.1us.
  Published 2us figure is release; residual vs release ~1.5x.
- serde_json_encode unchanged: 675-780us (pool also skips growth
  reallocs on repeat big docs).
- serde_json_decode: 1,133us on the 136KB surface (was ~1,219us).

## Parity

serde_writer 33/33, serde_decode 91/91, mcp_transport 175/0,
mcp_connect 16/0, data_supply 275 (207+68 seam, 0 fail),
data_supply_bodies 26/0 — plus byte-identical handleJson output vs
the pre-change module on success/error/notification arms.

## Residual

~1.5x vs release Rust is structural at the bench's 40-warmup budget —
Ignition/Sparkplug per-node dispatch dominates; deeper surgery (cold
-path inlining) not warranted for the transport shim.
