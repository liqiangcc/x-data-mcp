# Issue #54: bounded GBBQ decimal experiment

**GBBQ_FLOAT32_BUSINESS_DECIMAL_NORMALIZATION_NOT_VERIFIED**.
**TDX_OFFICIAL_QFQ_CONTRACT_NOT_PROVEN**. No production normalization contract
or runtime change is introduced. #54 remains open; the official-file gate remains
closed. Compatibility with a diagnostic candidate is not official semantics.

The current official archive byte evidence is retained in
[`official/archive-20260913.json`](official/archive-20260913.json), with its
scope and semantic limits explained in [`official/README.md`](official/README.md).
It confirms only the downloaded archive/member bytes and observed 29-byte
container shape; it does not change the normalization conclusion below.

## Frozen experiment and provenance

The baseline is main `9adcee4ebb1c6b07a281c747c4bdc61d377b582a` (merged #56).
`plan.json` was committed as `cfe7976` before event selection/calculation;
SHA256 `fe0e4a0855f3f4dd0c85a60cd74de41e67e6f5eb80b39d7dd9d484fea75f6106`.
All seven candidates apply identically to all four fields and all selected events.
No result-dependent tuning, tolerance or security-specific rule is used.

The universe is the **existing #53 four selected securities**, not a new stock
selection. The deterministic bounded rule yields nine unique events: three C1
float tails, two C3 tails, two C4 tails, two C2/C4 combinations (one overlaps a C4
tail), and the mandatory half-cent event. Only two C3 and two C4 tail events exist
in this bounded universe; the frozen “first three” cap does not invent a third.
There are no unresolved previous closes. This is not all-event or all-market
validation; sz.000004 has eligible C1-tail/rights events but none falls within
the frozen first-N caps after sorting, so it contributes no matrix row.

`inputs.json` preserves selected decoded record bytes/bits/index, selection
reasons, official .day member hashes, previous/next raw 32-byte records and previous
record index. Extraction checks the retained archive hash and chronological
records. Replay validates against unchanged #53 selected events/provenance/member
hashes. `--verify-artifacts` additionally reproduces extraction from the original
local hsjday archive and decrypts only nine indexed records from the retained
GBBQ member with the already pinned community decoder. It was run successfully.
No new market-data download or BaoStock query was made.

Evidence levels must stay separate:

- Official downloaded file bytes and hashes: retained source artifacts from #47/#48/#53.
- Record decode, C1/C2/C3/C4 field names/units and category interpretation:
  inherited **community reverse-engineering / research assumptions**, not an official contract.
- Decimal and reference calculations: reproducible diagnostic experiment only.
- Official document review: source-first but insufficient; see `source-review.md`.
- Official client/API numeric behavior: **NOT_RUN**; no client execution, login or Key.

The .day and gbbq artifacts have independent acquisition histories. Their separate
hashes pin the experiment, but do not prove a coherent common snapshot, freshness,
revision policy, or long-term server acquisition/use permission.

## Results

The matrix preserves every candidate and does not choose a winner.

- sh.600006 / 2025-07-17: previous close 7.25. `frozen_repr` and `exact_binary`
  produce a reference below 7.245, cent 7.24, diagnostic ratio 0.998621.
  `shortest_roundtrip` and all four fixed-scale candidates produce exact 7.245,
  cent 7.25, ratio 1.000000. Thus the same fixed rules explain this boundary
  counterfactually, but **five competing candidates** do so: no unique rule follows.
- C1 tails: sz.000002 / 1993-04-05, 1996-08-06, 2001-08-21; shortest decimals
  0.6, 1.4, 1.8. C3 tails: sh.600004 / 2005-12-20 (1.9), sh.600006 /
  2006-11-13 (3.3). C4 tails: sz.000002 / 1997-07-14 (2.37), 2000-01-10
  (2.727). These rows exercise multiple non-exact binary representations.
- C2/C4 combinations: sz.000002 / 1991-06-01 (4.4 / 5), 1997-07-14
  (4.5 / 2.37), plus the C4-tail row 2000-01-10 (7.5 / 2.727).
  A universal two-decimal candidate changes 2.727 to 2.73 and no longer roundtrips
  to the source bits. Matching a rounded reference can hide this information loss.
- Two different exact decimals, 0.05 and 0.050000001, both encode as 0x3d4ccccd.
  Neither float32 bits nor a short display uniquely identify the business decimal.

This matrix is narrower than a complete factor chain. The chosen reference-cent
HALF_UP and final single-event ratio rounding are **frozen research assumptions**.
They do not establish TDX's actual rounding order, half-tie behavior, per-field
scale, decimal conversion, intermediate precision or cumulative QFQ arithmetic.
The specific additional tails mentioned in #54 (1.57 / 0.747 / 0.301) are not
asserted tested here; the deterministic matrix covers other typical tails.

## Reproduce offline

Python 3.11+ standard library only, no production dependency:

```sh
python3 -B evidence/issue-54-gbbq-decimal/analyze_matrix.py --check
python3 -B -m unittest discover -s evidence/issue-54-gbbq-decimal -p 'test_*.py'
python3 -B evidence/issue-54-gbbq-decimal/analyze_matrix.py --output /tmp/issue54-matrix.json
```

Optional original-artifact audit (retained local files; never downloads):

```sh
python3 -B evidence/issue-54-gbbq-decimal/analyze_matrix.py --verify-artifacts /path/to/hsjday.zip /path/to/gbbq /path/to/pinned/gbbq_reader.py
```

The clean-checkout replay checks the committed bounded extraction, not fresh
acquisition or the community decoder independently. Original-artifact audit
checks extraction/decode linkage, not official business semantics. CI runs the
bounded replay and its integrity tests; passing CI is not a source-contract PASS.

Full #53 four-stock QFQ revalidation: **NOT_RUN**, because no official normalization
contract was established. Existing #53 aggregate remains
`CROSS_MARKET_VALIDATION_HAS_MISMATCH`, SHA256
`3880c0252eeb2605266f121a917f0e5184888e4df5528659448ce829b28ac1de`.
Its 600006 129-date / 516-OHLC mismatch and 000004 suspension classification are
unchanged. Old successful and failed evidence is neither rewritten nor deleted.
Live source tests: **NOT_RUN**. Official documentation HTTP retrieval was performed;
it is not a live market-source test.

## Evidence still needed before changing normalization

An independently auditable official definition or bounded official-client/API
observation must establish C1/C2/C3/C4 units and decimal scale/conversion, plus
where/how reference-price and subsequent factor rounding occur, including a
half-cent tie. A formatted display alone is insufficient. A discriminating
case must separate competing candidates; then the same established rule must
pass the #53 four-stock validation without tolerance or per-security adjustments.
Until then the production gate stays NOT_VERIFIED. #46/#6, authorization,
file-version consistency, parsers and real QFQ/yearly remain separate open gates.
