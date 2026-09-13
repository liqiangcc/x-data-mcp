# bounded official `.day` member verification

`official-sh600006-range.json` records a single member extracted from the official
`hsjday.zip` using HTTP Range requests. The full 549 MB archive was not downloaded,
stored, imported, or scanned. The response archive hash and member metadata match the
retained official package evidence; the installed Rust `inspect_day` binary reproduced
the member SHA, 32-byte record count, and first/last dates.

This is structural evidence for the `.day` parser only. It does not establish amount
or volume units, QFQ/GBBQ semantics, archive authorization, cross-file snapshot
consistency, or production readiness. QFQ and live source remain `NOT_RUN`.
