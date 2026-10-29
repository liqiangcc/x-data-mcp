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
- issue #396 and #398 evidence comments (raw-response observations).

| file | request | notes |
|---|---|---|
| `sse_listing_main.json` | `sqlId=COMMON_SSE_CP_GPJCTPZ_GPLB_GP_L&STOCK_TYPE=1` | `pageHelp.total` is `0` (real SSE behaviour for this sqlId; it means "not reported"). Contains an **adversarial** `STOCK_TYPE=2` (B share) row, which must be filtered. |
| `sse_listing_star.json` | same sqlId, `STOCK_TYPE=8` | STAR-market A shares (kept). |
| `sse_delisting_main.json` | same sqlId, `STOCK_TYPE=1&COMPANY_STATUS=3` | terminated main-board A shares. |
| `sse_delisting_star.json` | same sqlId, `STOCK_TYPE=8&COMPANY_STATUS=3` | terminated STAR shares. |
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
  `000001`/`000002`/`000004`/`301569`, `200002`/`200003`, and every
  `DELIST_DATE` on `600680`/`688555`.

sha256 of the frozen sample bytes (the adapter never re-derives the values from
the sha; this is only so a reviewer can tell whether a fixture was edited):

| file | bytes | sha256 |
|---|---|---|
| `sse_listing_main.json` | 810 | `b832b9c908e5dde605ea28b73cb5c109d047580867f509c0551d5d6c5ce31cab` |
| `sse_listing_star.json` | 374 | `7383d8ed5f7029b017b1a955c8cd96ac118dbeabe01d628b3d6e879808cdb480` |
| `sse_delisting_main.json` | 365 | `5bb52d4f5dd158f39d5be1eef45207308a974a26b139056a99061cf52edb3fa8` |
| `sse_delisting_star.json` | 220 | `d2a54e809bd71bb014bd420bb44c439132140a1ca86b1b12591b17b3f3d46e20` |
| `szse_listing.json` | 413 | `76961eb35ae50642e98d6973e78450a312c8a39e12b9f4776fd24dab87bb6bfb` |
| `szse_delisting_main.json` | 216 | `ea018791e722eeec98b0cb6c41f91ca39b6638b9b625d3e923deaa6679d33f15` |
| `szse_delisting_nm.json` | 142 | `f1581225e17b6b94411f1f57f53314bd8c97d8442087917e443412d8e95e6dd4` |

These files are read with `include_bytes!`, so editing one changes the test
binary; update this table in the same commit.
