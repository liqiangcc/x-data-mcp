# Source review

## Scope and provenance

This is a source-first, evidence-only review of the official TDX downloads, not a production-source approval, data import, or qfq implementation. The request was limited to `base.zip` and `gbbq.zip`; no market-data probe was run.

The official notification page (`https://www.tdx.com.cn/article/52.html`) was fetched at the same investigation and confirms the official download URLs. The exact HTTP response metadata, UTC/Asia/Shanghai timing, and hashes are in `base.zip.download-meta`, `gbbq.zip.download-meta`, and the corresponding `.headers` files. `base.zip` and `gbbq.zip` were each fetched once, returned HTTP 200 with `application/zip`, matched their content-lengths, and passed `unzip -t`.

## Base archive and DBF

`base.zip` contains 542 members, including one `base.dbf`; the full member manifest is committed. The DBF parser found:

- version byte `0x03`, header date `2026-09-11`, header record count `8871`;
- header length `1313`, record length `481`, code-page byte `0x7a`;
- 40 fields, zero records whose deletion flag was `0x2a`, and a valid `0x1a` EOF marker;
- `GPDM` is a six-byte character field and raw `000001` is preserved with leading zeros.

The code-page byte has not been assigned an encoding here. Any CP936 decoding in the machine evidence is explicitly diagnostic, not a confirmed encoding contract. No package README or `base.dbf` field-definition document was present.

The raw schema shows code/market-like and classification-like fields (`SC`, `GPDM`, `DY`, `HY`) and many numeric basic/financial fields. The package also contains configuration mappings such as `tdxhy.cfg` and `tdxhy2.cfg`; however, the downloaded package does not supply the names, level semantics, or source identity required to prove a TDX secondary/research industry taxonomy. `base.dbf` has no auditable security name, source security ID, exchange text, or industry name. The target rows are present (`000001`, `600000`) and retain leading zeros, but they do not close #6 AC-12 or prove complete directory coverage.

## GBBQ archive and decoding limit

`gbbq.zip` contains 26 members. The main `gbbq` member is proprietary binary (`5,966,087` bytes; SHA-256 `24f46cecc9b085ee6c0ef126fab33b405b8057747793eb5d25d44e624769ff60`), while `gbbq.map` is text with 7,391 numeric entries. The map has target entries `0000019723` and `6000009687`.

There is no package README or explicit `gbbq` schema/event-code mapping. The main binary has no directly readable target-code/date/category evidence under the standard-library-only inspection used here, and map offsets do not by themselves establish a record layout. The bundled text files concern reference/market information and are not treated as `gbbq` documentation.

Thus dates, category codes, and numeric event values for the two target securities cannot be listed without guessing. The package may contain share/corporate-action data, but that interpretation is not an auditable field contract from these files alone. The current evidence level for `.day + gbbq -> local qfq` is **INSUFFICIENT / UNKNOWN**. Missing evidence includes a documented record layout, event-code meanings, date/order semantics, units, the join to daily prices, the factor formula/baseline, and revision rules.

## #6 mapping conclusion

`base.zip` is a technically readable official package and a possible input for code and some source-maintained attributes. It does not, from the downloaded artifacts alone, provide the complete identity/name/exchange/industry contract needed for the security catalog. `gbbq.zip` supplies an indexed opaque binary but not a verifiable event feed. Neither archive proves licensing/production authorization, full coverage, long-term stability, or AC-12.

No values were converted to floating point and no event semantics were inferred from numeric codes. The historical TDX yearly vertical-slice code and other source evidence are outside this evidence-only change; this material does not make source year data a production requirement and does not alter the current unknown qfq gate.
