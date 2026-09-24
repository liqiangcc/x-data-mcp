# NM-111 — day_parse second optimization round

Lane grant: `src_node/` day-parse path files + p5 scripts/evidence.
Baseline `5fa3008`; only `src_node/parsers/day_file.mjs` changed.

## Changes (byte-identical output contract preserved)

- Aligned `Uint32Array` view when `bytes.byteOffset % 4 === 0` (guarded
  by a one-time little-endian probe) — 8 word loads per record instead
  of 32 byte reads; verbatim byte-assembly fallback otherwise
  (misaligned fixture exercises it — identical rows).
- Duplicated loop bodies: the aligned path indexes `words[w + k]`
  directly (no per-field closure/shift); fallback keeps byte assembly.
- Ordering check on the raw `YYYYMMDD` u32 — for valid dates it orders
  identically to the epoch day, so the Hinnant civil math reduces to a
  days-in-month validity check (`ymdValid`).
- `"YYYY-MM-"` prefix interned per calendar month (dates strictly
  increasing — changes at most once/month); `P2[100]` pad table.

## Byte-identity

`day_parse_golden.mjs` — 19/19 identical vs a frozen pre-change module:
3 retained members (sh000001 8725 + sh000002 8725 + sz000001 8453
records — sha256 of full record output equal) + 16 edge fixtures
(truncated, leap ok/bad, year 1/9999/10000, month 0/13, day 0/32,
equal/descending/ascending, empty, misaligned view).

## Measured (NM-072 harness, p50, box, node v20.19.2, iters=300
## warmup=40, concurrency=1; Rust oracle = release rlib)

| surface | before p50 | after p50 | rust p50 | residual |
|---|---|---|---|---|
| day_parse | 897,125 ns | 350,685 ns (range 270k–521k across runs) | 43,816 ns | ~8.0x (was ~20.5x) |

Honest flag: residual is now per-record heap allocation — 8725 ×
(record object + ohlc array + date string). The output contract fixes
that shape; a record-free API would close it but changes the contract
(out of lane). Reads/math/strings are now minor terms.

## Regression

- p3 `diff_harness_probe.mjs .` — all families PASS, 0 diffs.
- p4 acquire probes (store 32 checks PASS, service/api/govern PASS),
  bigint_wire 16/0.
- selftest `diff_harness_probe.mjs . diff` exits 1.

## Reproduce

```
node scripts/node_migration_p5/day_parse_golden.mjs <old.mjs> <dir>
bash scripts/node_migration_p5/run.sh /dev/shm/<dir> 300 40
```
