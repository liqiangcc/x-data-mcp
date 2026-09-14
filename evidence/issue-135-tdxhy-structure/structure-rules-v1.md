# tdxhy.cfg / tdxhy2.cfg structure-rules-v1

This rule set is frozen before either target member is opened. It defines an
offline, evidence-only structural scan for Issue #135. It does not define
industry semantics, hierarchy, code-to-name mappings, or an encoding.

## Fixed input and member scope

- The input archive must be the retained #133 archive, identified by exact
  byte length `16099192` and SHA-256
  `39de51e26c0ee8fb3fd970ee870f4cb955b8fbbd7f84d752157f3998e49b71d9`.
- The archive member table must be checked against retained
  `archive-20260913.json` member identity. A member identity mismatch is a
  failed input gate, not a best-effort scan.
- Exactly these two members are in scope: `tdxhy.cfg` and `tdxhy2.cfg`.
  Other archive members are not read for this slice's semantic result.
- The scanner reads member bytes as bytes. Unknown encoding is retained as raw
  bytes/hex and is never guessed or decoded to infer names.

## Mechanical observations

For each target member, report only byte-observable facts:

- byte length, SHA-256, and whether the final line has a line ending;
- line-ending counts/forms (`LF`, `CRLF`, `CR`, or no terminator), blank-line
  count, and record count;
- each non-blank raw line's byte offset, byte length, and hex/raw representation
  where text is not safely printable;
- literal ASCII byte `|` is the only field separator. Field index is the
  zero-based position between literal separators; field count is separators + 1
  for each non-blank line (including empty fields).
- report the complete field-count distribution and do not normalize whitespace,
  delimiters, or line endings before measuring.

A six-digit numeric token is mechanically a contiguous run of exactly six ASCII
bytes in `0` through `9` with an ASCII non-digit boundary on each side (or a
member/line boundary). It is reported with member path, line/byte offset, raw
bytes, and field index when it lies in a `|` field. It is not called a security
code unless it exactly equals one of the fixed sample tokens below.

## Fixed samples and overlap

The only sample tokens are `688318`, `600000`, and `000001`. For each sample and
member, report every occurrence with raw line bytes, absolute member byte offset,
line offset, and literal-`|` field index (or `null` when outside a field), plus
occurrence count. Missing samples are reported as missing.

For all mechanically detected six-digit tokens, form a per-member set of the
raw token values. Report:

- left-set cardinality, right-set cardinality, intersection cardinality;
- counts of tokens only in `tdxhy.cfg` and only in `tdxhy2.cfg`;
- per-member occurrence counts for every token in the intersection;
- for each intersecting token, the number of lines containing it in each member
  and the Cartesian cardinality of its left/right line occurrences.

Set construction is over exact six-byte ASCII values, with duplicates removed;
occurrence and line cardinalities are counted separately. Results must be
sorted by raw token bytes and then by offset so repeated runs are deterministic.
The intersection is a structural overlap only and is not evidence of a complete
security universe or a business relationship.

## Fixed result classification

The scanner emits exactly one of these classifications, using only the
mechanical overlap facts above:

- `STRUCTURAL_RELATION_FOUND`: the two target members pass the archive/member
  identity gate and their six-digit token sets have a non-empty intersection,
  with exactly one line occurrence for every intersecting token in each member.
- `STRUCTURAL_RELATION_AMBIGUOUS`: the identity gate passes and the
  intersection is non-empty, but at least one intersecting token has more than
  one line occurrence in either member (its left/right line Cartesian
  cardinality is therefore greater than one).
- `NO_STRUCTURAL_RELATION_FOUND`: the identity gate passes and the intersection
  is empty.

If the archive/member identity gate fails, the output is a deterministic input
failure (`INPUT_IDENTITY_NOT_VERIFIED`) and must not be recast as a structural
classification. Classification never infers parent/child levels from
`X420202`/`X4202`, field position, member filename, shared prefixes, or
co-occurrence on a line.

## Reproducibility and exclusions

The scan must be runnable offline from the retained archive path supplied by the
operator. A second clean checkout must produce byte-identical JSON output;
output ordering and serialization are part of the evidence. The archive itself
is not committed.

This slice does not add probes, fetch files, inspect third-party code, decode
unknown text, modify MCP/SQLite/QFQ/OfficialFilePipeline, add production
fallbacks, or close #46/#6. Any future semantic claim requires separate
independent evidence.
