# Official-source review, 2026-09-12

This is a bounded review, not a claim that no official contract exists anywhere.
HTTP retrieval metadata and SHA256 of complete HTML responses are recorded in
`official-doc-review.json`. The local raw review copies are outside git; the
following summaries and locators are durable. Reproduction of the matrix does
not use mutable webpages. No official numeric client/API execution was performed.

1. [TDX formula function list](https://help.tdx.com.cn/gspt/docs/markdown/redword/functionlist.html):
   inspect SPLIT, DIVFACTOR, ROUND2 and MOD. SPLIT describes per-share dividends
   and a transformed distribution ratio. DIVFACTOR selects adjustment direction.
   ROUND2 documents rounding and distinguishes formula output display settings;
   MOD warns of limited effective precision. None of these inspected entries
   links GBBQ C1/C2/C3/C4 storage bits to a decimal scale or prescribes the actual
   QFQ reference-price rounding sequence. Generic formula rounding or displayed
   decimals cannot establish the hidden ingestion normalization.
2. [TDX get_market_data](https://help.tdx.com.cn/quant/docs/markdown/mindoc-1ctuhthaq5qmg/mindoc-1h10g60jt68sc.html):
   inspect returned-data notes and ForwardFactor. The documentation constrains
   when that factor is valid and says it is not directly usable to calculate
   adjusted prices; the adjusted result also depends on the requested interval.
   It does not define GBBQ business decimals or the half-cent boundary. This
   online interface is reviewed only as evidence, never restored as fallback.
3. [TDX download address notice](https://www.tdx.com.cn/article/52.html):
   establishes official historical base/gbbq download locations and discusses
   broker server configuration. It supplies neither the float-field schema nor
   a general authorization for this application's long-running server use.

Official search scope included GBBQ precision, corporate-action reference-price
rounding, and TDX dividend/factor APIs. The retrieved official documentation above
provides no sufficient normalization contract. Community decoder provenance is
unchanged from `../issue-3-cross-stock-qfq/source-provenance.json`; field mapping
and diagnostic formula remain community/research evidence, not official facts.

No new BaoStock responses were requested. Its stored #53 factor comparison is a
cross-source observation only. It cannot pick one of the five candidates that
explain the 600006 half-cent event. The experiment therefore deliberately ends
with NOT_VERIFIED rather than a production normalization function.
