# A-1/A-2 exchange list fixtures (M3 PR-3)

Minimal, network-free samples of the four exchange lists the A-1/A-2 adapter
reads. They are **fixtures**, not source evidence: they exist to prove the
parser, the A-share filter and the merge, and they are small enough to read.

Provenance of the shape (field names, envelope, filtering quirks) and of the
values marked *real* below:

- `docs/tasks/issue-446-m3-delisted-industry.md` §6.2/§7.1/§7.2;
- `docs/decisions.md` D-45 (A-1 fields) and D-46 (A-2 fields, the two code
  changes);
- `docs/tasks/issue-391-m2-security-master.md` §10 S-1/S-2 and §2.1 E-1…E-4;
- issue #396 and #398 evidence comments (raw-response observations);
- the 2026-10-30 real-response forensics (`reports/x-data-mcp/a1a2-forensics.md`):
  the SSE rows use `A_STOCK_CODE`/`COMPANY_CODE`/`COMPANY_ABBR`/
  `STATE_CODE_STOCK`/`PRODUCT_STATUS` (there is no `SECURITY_CODE`/
  `SECURITY_ABBR`/`COMPANY_STATUS` column) and `pageHelp` reports a real
  `total` with `pageNo`/`pageSize=2000`/`pageCount=1` when the request sends
  `pageHelp.pageSize`. The SSE fixture rows were rewritten from those frozen
  responses; the values below are kept from the earlier fixtures so the merge
  tests keep their meaning.

| file | request | notes |
|---|---|---|
| `sse_listing_main.json` | `sqlId=COMMON_SSE_CP_GPJCTPZ_GPLB_GP_L&STOCK_TYPE=1` | `pageHelp.total` is the real row count (5), `pageNo=1`, `pageSize=2000`, `pageCount=1`. Contains an **adversarial** `STOCK_TYPE=2` (B share) row, which must be filtered, and `DELIST_DATE:"-"` rows (not a delisting day). |
| `sse_listing_star.json` | same sqlId, `STOCK_TYPE=8` | STAR-market A shares (kept). |
| `sse_delisting_main.json` | same sqlId, `STOCK_TYPE=1&COMPANY_STATUS=3` | terminated main-board A shares. |
| `sse_delisting_star.json` | same sqlId, `STOCK_TYPE=8&COMPANY_STATUS=3` | terminated STAR shares. |
| `sse_query_page_ignored.json` | `sqlId=…GPLB_GP_L&pageHelp.pageNo=1&pageHelp.pageSize=2000` | **negative fixture**: the endpoint ignored `pageSize` and declares `pageCount=2`/`total=100` while returning the same 50 rows on every page. The list plan must fail closed (`page_size_ignored`, `declared total … but collected …`) instead of taking page 1 as the list. |
| `szse_listing.json` | `SHOWTYPE=JSON&CATALOGID=1110` | current A-share list only; a B-share row (`200002`) must be filtered. |
| `szse_delisting_main.json` | `CATALOGID=1793_ssgs&TABKEY=tab2&selectModule=main` | terminated/suspended list, `main` module. |
| `szse_delisting_nm.json` | `…&selectModule=nm` | the same `000003` row as `main`, to prove the two modules de-duplicate. |

Values:

- *real* (from the admission evidence): `600001` listing 1998-06-18 / delisting
  2009-12-29; `600018` listing 2000-07-19 (TDX `SSDATE` 2006-10-26 is the
  subject-change case); `601607` listing 1994-03-24 (the row that absorbed
  `600849`); `302132` A-share listing 2010-08-27 (inherited from `300114`);
  `000003` delisting 2002-06-14.
- *fixture only* (needed to exercise a filter or the merge, not evidence):
  `600680`/`900901` (A+B pair with different listing days), `688555`/`688981`,
  `000001`/`000002`/`000004`/`301569`, `200002`/`200003`, every
  `DELIST_DATE` on `600680`/`688555`, and the 50 rows of
  `sse_query_page_ignored.json`.

sha256 of the frozen sample bytes (the adapter never re-derives the values from
the sha; this is only so a reviewer can tell whether a fixture was edited):

| file | bytes | sha256 |
|---|---|---|
| `sse_listing_main.json` | 1498 | `21b673f52e143602af4864467d83ba02648c43606db9165f8118d97e326598ed` |
| `sse_listing_star.json` | 662 | `9793c2c3c8306a5a1b9b1a2e1269b23ba6b07918a7b2129641297400c2ee6c0c` |
| `sse_delisting_main.json` | 657 | `757d7e46231153280e206bc2576992f2860cda3fca21f0d6a1ce78287715e456` |
| `sse_delisting_star.json` | 377 | `d2efef2caa75b7b37d3436f6c916ee57e33de2fd837932a0492cc6c76af24961` |
| `sse_query_page_ignored.json` | 12897 | `d0d6489bb1a29fb0caa6c7353f834c81a0dc33f89119ccb9c11be944ca2e58f0` |
| `szse_listing.json` | 413 | `76961eb35ae50642e98d6973e78450a312c8a39e12b9f4776fd24dab87bb6bfb` |
| `szse_delisting_main.json` | 216 | `ea018791e722eeec98b0cb6c41f91ca39b6638b9b625d3e923deaa6679d33f15` |
| `szse_delisting_nm.json` | 142 | `f1581225e17b6b94411f1f57f53314bd8c97d8442087917e443412d8e95e6dd4` |

These files are read with `include_bytes!`, so editing one changes the test
binary; update this table in the same commit.
