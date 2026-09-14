# #133 base.zip candidate selection rules v1

This rule set is frozen before opening any member bytes. It is an evidence locator, not a decoder or industry contract.

## Input identity

- Archive path for this run: `/tmp/official-base.zip`.
- Required archive bytes: `16099192`.
- Required archive SHA-256: `39de51e26c0ee8fb3fd970ee870f4cb955b8fbbd7f84d752157f3998e49b71d9`.

The last value above is intentionally checked by the runner against the retained archive record before scanning. (If it does not match the retained record, the run stops.) The committed evidence record is the authoritative value; no filename or size match is sufficient.

## Fixed search tokens

Search raw uncompressed member bytes for these exact ASCII byte sequences, without case folding, trimming, prefix matching, numeric parsing, or encoding conversion:

- `X4202` — code in the retained official TQ document sample.
- `X420202` — raw code in the retained `tdxhy.cfg` line for `688318`.
- `T1205` — neighboring raw token in that same retained `tdxhy.cfg` line.
- `688318` — security token from the retained cross-check, used only to locate repeated raw evidence.

The first two are the required industry-code probes. The latter two are context probes, not semantic claims.

## Fixed candidate classes

1. **Primary classification candidates:** member basename exactly `tdxhy.cfg` or `tdxhy2.cfg`.
2. **Dictionary candidates:** member basename exactly one of `infoharbor_ex.code`, `infoharbor_ex.name`, `infoharbor_ex.dat`, `base.dbf`, `base.map`.
3. **Other members:** every other member is scanned and indexed, but a token hit there is only a locator (`AMBIGUOUS`), never a dictionary or hierarchy candidate.

No filename similarity, prefix such as `X4202*`, compressed size, CRC, or content resemblance may promote a member. No code prefix truncation, code→name mapping, or parent/child inference is performed.

## Result classification

- `FOUND_CANDIDATE`: a primary/dictionary candidate contains an exact fixed token hit. This means only “a potentially relevant member was located”.
- `AMBIGUOUS`: a token hit is found only in other members, or multiple candidate members/roles produce incompatible raw observations. It remains unresolved evidence.
- `NO_CANDIDATE_FOUND`: no fixed token hit occurs in any member. This is bounded to this archive and token set; it does not claim the official source lacks a dictionary.

For every hit, retain member path, uncompressed member SHA-256, uncompressed byte length, raw byte offset, matching bytes in hexadecimal and ASCII when safe, and ZIP local-header/data offsets. Preserve undecodable bytes as hex. Record all 542 scanned members in deterministic name order, including members with no hits.

The output must include the archive SHA/byte count and this rules file version. A successful locator result never closes #46/#6 and never changes production readiness.
