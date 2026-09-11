# TDX official DBF source evidence

Evidence-only snapshot collected on 2026-09-11 from the two official download URLs named by the TDX notification page:

- `http://www.tdx.com.cn/products/data/data/dbf/base.zip`
- `http://www.tdx.com.cn/products/data/data/dbf/gbbq.zip`

The downloaded ZIP files are intentionally not committed. Download headers, timing, byte counts, hashes, complete member manifests, parsed DBF schema/samples, and the standard-library parser are committed here.

## Download and archive facts

| archive | HTTP/effective URL | bytes | SHA-256 |
|---|---|---:|---|
| `base.zip` | `200`, HTTP URL unchanged | 16,098,051 | `80ad1681c260cea18bb6f3e5ca6d3ac1acd215eae7b533048c8735619a1dc433` |
| `gbbq.zip` | `200`, HTTP URL unchanged | 6,350,201 | `6cfdbfc5a67e80ba6773cd5e8cc6779a97a01e07d892e5e6a17c9b35dbef60aa` |

Both archives passed `unzip -t`. `base.zip` has 542 members and `gbbq.zip` has 26 members; the full compressed/uncompressed sizes, CRCs, and member timestamps are in the corresponding `*.members.json` files. Nested `zhb.zip` and `tend_std.zip` manifests are included as well.

The official notification page was fetched separately and its headers/hash are recorded. Its fetched body confirms the official download links; it is not treated as a field-definition document.

## `base.zip`

The archive contains one DBF, `base.dbf`, plus `.dat`, `.cfg`, `.map`, `.code`, `.name`, `.txt`, `.ini`, and `.md5` support files. `base.dbf` has 8,871 records and 40 fields. Its complete header/schema and raw-byte-preserving samples are in `base-dbf-schema-and-samples.json`.

Observed useful raw fields include:

- `GPDM` (`C`, length 6): six-character security code; `000001` retains its leading zeros.
- `SC` (`C`, length 1): a source market-like code, but no package field specification was found to establish an exchange mapping.
- `GXRQ`, `SSDATE`, `MODIDATE`, `GDRS`: update/listing/support values as named by the file, without assuming business semantics beyond the labels.
- `DY` and `HY`: classification-like codes. `tdxhy.cfg` and `tdxhy2.cfg` contain code mappings, but no names or hierarchy definition in the package establish that these are the requested TDX secondary/research industry taxonomy.

The DBF does not contain a security name, source security identifier, exchange text, industry name, or an auditable industry hierarchy label. Therefore it is not sufficient evidence for the #6 security-plus-TDX-secondary-industry catalog contract. The 8,871-record file looks like a broad basic/financial/support data package with a current-looking header/update date, not proof of a complete current directory snapshot.

## `gbbq.zip`

The main `gbbq` member is a 5,966,087-byte proprietary binary, not a DBF. `gbbq.map` has 7,391 numeric lines and contains entries for both target codes (`0000019723` and `6000009687`), but the package has no auditable record layout or field mapping for decoding those offsets. No package README or `gbbq` event-field specification was found. The bundled `infoharbor_*.txt` files are reference/market materials, not a schema for `gbbq`.

Consequently this package does not yet provide auditable date/category/value rows for `000001` or `600000`. It looks compatible with a corporate-action/share-change data role from its filename and map structure, but event-code meanings, ordering, units, join semantics with `.day`, revision behavior, and the factor formula remain UNKNOWN. The evidence level for `.day + gbbq -> local qfq` is therefore **INSUFFICIENT / UNKNOWN**; this work does not claim that the pair is enough to calculate or validate qfq.

See `source-review.md` for the evidence/interpretation boundary and `parse_tdx_dbf.py` for the reproducible offline parser.
