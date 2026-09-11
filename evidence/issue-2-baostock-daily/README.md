# BaoStock qfq daily bounded evidence

This directory records one bounded, read-only BaoStock daily-data verification for Issue #2 under the 2026-09-11 contract. It does not request source-year data, modify production code, or change any `unknown`/`confirmed` status.

- Fixed symbols: `sh.600000`, `sz.000001`.
- Request: `frequency=d`, `adjustflag=2`, with string-valued rows preserved.
- Evidence scripts use an anonymous BaoStock protocol handshake only; no user account or credential was used.
- `probe-result.json` contains the machine-readable parsed string rows, timestamps, request parameters, response fields, `page_count`, flattened rows, first/last dates, and parsed-response hashes. It does not claim to preserve fresh-session per-page row boundaries.
- `environment.json` records the temporary PyPI distributions and SHA-256 values. No package was installed into a system or virtual environment.
- `source-review.md` separates observed response facts, reused official documentation, and conclusions/unknowns.

The probe is evidence for two-sample availability and bounded repeatability only. It is not evidence of full-market coverage, complete listing history, long-term stability, production authorization, or a fully proven qfq adjustment basis.
