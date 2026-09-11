# Community GBBQ decoder evidence review

## Scope and provenance

This evidence-only slice reuses the official `gbbq` member downloaded and hashed in #47. It makes no TDX download and no market-data request. The reused member is 5,966,087 bytes with SHA-256 `24f46cecc9b085ee6c0ef126fab33b405b8057747793eb5d25d44e624769ff60`; the containing official archive hash is recorded in `source-provenance.json`.

The decoder source is `wxyzZ/pytdx/pytdx/reader/gbbq_reader.py` at commit `0d6f80618e1bae0f8e48552e597af492f3640f5d`. Its Git blob SHA-1 is `0eda4935027ffc79b5c425eff0a70a6d492b21d1`; the downloaded raw file SHA-256 is `e28fae76760b1247a960cab285aa357c2d263b3212e7cabfb3f1ebfcc127d6ba`. The source comments explicitly cite two reverse-engineering blog posts. This is community evidence, not an official TDX contract. The original file imports pandas, but the committed reproduction uses only Python standard-library modules and extracts the key from the fixed local source at runtime; the key table and full third-party source are not vendored.

## Structural preflight and decode

The first four bytes, interpreted little-endian, give `205727`. The official member length satisfies exactly:

`5966087 == 4 + 205727 * 29`.

The fixed algorithm decrypts three encrypted 8-byte chunks and appends five clear bytes, then unpacks the parser-declared `<B7sIBffff` 29-byte record. The validation decoded all `205727` records without overrun or exception. It found zero invalid six-digit code records, zero calendar-invalid dates, and one calendar-valid but out-of-range/sentinel `1899-12-30` value. That exception is retained in the machine summary and is not silently treated as a normal event date.

All decoded numeric category values were retained as numbers only. Their business meanings are **NOT_VERIFIED**; this review does not call them dividend, rights, split, or any other event type. The four float32 fields are recorded as raw uint32 bit patterns plus Python `repr` diagnostics. No unit, currency, or amount/business interpretation is asserted.

The full decoded stream is not committed. Only records for `000001` and `600000` are retained in `target-summary.json`.

## Target results and date cross-check

- `000001`: 80 records, date range `1990-03-01..2026-06-30`, categories `[1, 2, 3, 5, 9]`; no record with date `2017-05-25`.
- `600000`: 88 records, date range `1999-11-10..2026-07-16`, categories `[1, 2, 3, 5, 6, 8, 9]`; one exact `2017-05-25` record at decoded index `127199`, category `1`, with clear bytes preserved.

The date is cross-checked only against the already committed BaoStock qfq documentation-example evidence for `sh.600000` covering `2017-05-24..2017-05-26`. The result is `DATE_EXACT_MATCH_ONLY`: it does not prove category meaning, an event causal link, a qfq factor, or that the files are sufficient together.

The stdlib decoder was run twice against the same raw member and fixed source. The two canonical JSON outputs were byte-identical with SHA-256 `6664a14b997d367cc07761705e0e1eaab0751906d4064557e11ca009467c93f3`.

## Layered conclusion

- Community decoder structural status: **COMMUNITY_DECODER_STRUCTURALLY_COMPATIBLE** for this official 2026-09-11 file: count/length, full record decode, code/date plausibility, and deterministic repeat all pass, subject to the community reverse-engineering provenance.
- Event date cross-check: **DATE_EXACT_MATCH_ONLY** for `600000/2017-05-25`; `000001` has no record on that date in this file.
- Category/numeric field semantics: **NOT_VERIFIED**.
- qfq formula, baseline, corporate-action factor chain, `.day + gbbq` sufficiency, coverage, revisions, units/currency, licensing, and production authorization: **NOT_VERIFIED / UNKNOWN**.

Successful decryption must not be promoted to a qfq validation or production-source approval.
