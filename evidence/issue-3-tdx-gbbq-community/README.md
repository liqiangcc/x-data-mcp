# TDX GBBQ community-decoder evidence

Evidence-only offline validation of the official `gbbq` member previously downloaded in #47. No TDX download, data request, package installation, or business-code change was made in this slice.

## Provenance

- Official archive: `gbbq.zip`, SHA-256 `6cfdbfc5a67e80ba6773cd5e8cc6779a97a01e07d892e5e6a17c9b35dbef60aa`.
- Reused member: `gbbq`, 5,966,087 bytes, SHA-256 `24f46cecc9b085ee6c0ef126fab33b405b8057747793eb5d25d44e624769ff60`.
- Community parser: `wxyzZ/pytdx`, commit `0d6f80618e1bae0f8e48552e597af492f3640f5d`, `pytdx/reader/gbbq_reader.py`.
- Parser Git blob SHA-1: `0eda4935027ffc79b5c425eff0a70a6d492b21d1`; raw SHA-256: `e28fae76760b1247a960cab285aa357c2d263b3212e7cabfb3f1ebfcc127d6ba`.

The fixed parser comments cite reverse-engineering blog posts, so it is community evidence and not an official TDX data contract. The validation script extracts its key from the fixed local source at runtime and does not vendor the key table or third-party source. It uses Python standard library only.

## Results

- First four bytes: little-endian count `205727`.
- Exact structural check: `5966087 == 4 + 205727 * 29`.
- Decoded records: `205727`; invalid six-digit codes: `0`; calendar-invalid dates: `0`; calendar-valid but out-of-range/sentinel dates: `1` (`1899-12-30`).
- Target `000001`: 80 records, `1990-03-01..2026-06-30`, categories `[1,2,3,5,9]`, no `2017-05-25` record.
- Target `600000`: 88 records, `1999-11-10..2026-07-16`, categories `[1,2,3,5,6,8,9]`, one exact `2017-05-25` record.
- The `600000/2017-05-25` date matches the already committed BaoStock official-PDF qfq example window; this is a date cross-check only.
- Two offline runs were byte-identical; canonical target-summary SHA-256: `6664a14b997d367cc07761705e0e1eaab0751906d4064557e11ca009467c93f3`.

The four float32 values are recorded as raw bit patterns plus diagnostic representations. Category meanings, units, qfq formula/baseline, corporate-action semantics, `.day + gbbq` sufficiency, licensing, and production readiness remain **NOT_VERIFIED / UNKNOWN**.
