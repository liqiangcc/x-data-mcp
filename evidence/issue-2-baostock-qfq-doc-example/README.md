# BaoStock qfq documentation-example evidence

This evidence-only slice reproduces the bounded `sh.600000`, `2017-05-24..2017-05-26` example under Issue #2's current qfq-daily contract. It makes no source-year request and does not change production code or evidence status.

- `adjustflag=2` qfq and `adjustflag=3` no-adjust were each requested once in a fresh anonymous BaoStock service session.
- `probe-result.json` preserves request/response timestamps, response fields, all returned values as strings, the package/PDF scope, and field-by-field comparison classifications.
- `probe.py` is the exact bounded probe script. `environment.json` records the temporary package versions and wheel hashes.
- `source-review.md` separates local client/source facts, the official PDF example, live observations, and layered conclusions.

The qfq live values differ from the supplied PDF example and are retained as-is. No float tolerance, correction, or production claim is applied.
