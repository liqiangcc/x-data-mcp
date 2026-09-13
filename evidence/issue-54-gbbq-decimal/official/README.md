# Official GBBQ archive snapshot evidence

This directory records one bounded retrieval of the official `gbbq.zip` URL
for Issue #54. The archive was downloaded once on 2026-09-13 and its bytes and
member hashes are recorded in `archive-20260913.json`; the archive itself is
not committed.

The retained `gbbq` member has the observed shape `4 + count * 29` (205727
records, 29 bytes each), matching the existing lossless structural parser.
This is only a container and byte-integrity observation. It does not establish
the meaning of categories or float32 fields, business decimal scale, rounding,
corporate-action semantics, QFQ baselines, or `.day`/GBBQ snapshot
consistency. The current archive is also a separate acquisition from the
older retained archive, so no cross-file version claim is made.

The result remains `NOT_VERIFIED`, `qfq=NOT_RUN`, and
`NOT_PRODUCTION_READY`. Community decoders remain evidence and cross-checks,
not an official semantic contract. No production import or source-path change
was made.
