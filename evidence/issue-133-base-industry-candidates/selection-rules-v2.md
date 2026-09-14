# #133 base.zip candidate selection rules v2

This rule set replaces the invalid v1 classification derivation. It is frozen
before any v2 member scan and does not use the v1 hit counts or locations to
choose a favorable rule.

## Fixed input

- Archive argument: `/tmp/official-base.zip`.
- Required bytes: `16099192`.
- Required archive SHA-256: `39de51e26c0ee8fb3fd970ee870f4cb955b8fbbd7f84d752157f3998e49b71d9`.
- Retained index: `evidence/issue-6-tdx-dbf-source/official/archive-20260913.json`.
- Required member count: exactly 542, with every member bytes/sha256 matching that index.

## Fixed probes and candidate classes

Industry probes are exactly these two raw ASCII byte sequences:

- `X4202`
- `X420202`

They are searched independently as exact bytes, including their intentional
substring overlap. No prefix matching, truncation, case folding, decoding, or
numeric interpretation is performed.

Context probes are exactly `T1205` and `688318`. They are searched and
reported as context evidence only. A context hit cannot change the overall
classification.

Candidate classes are fixed by member basename:

- `PRIMARY_CLASSIFICATION`: `tdxhy.cfg`, `tdxhy2.cfg`.
- `DICTIONARY_CANDIDATE`: `infoharbor_ex.code`, `infoharbor_ex.name`,
  `infoharbor_ex.dat`, `base.dbf`, `base.map`.
- `OTHER_MEMBER`: every other member.

Only exact industry-probe hits in the first two classes are candidate hits.
Industry hits in `OTHER_MEMBER` are retained as raw locators but do not create
a candidate.

## Mechanical result classification

For each member, compute `industry_hits` and `context_hits` separately. Let
`candidate_hits` be all industry hits whose member class is
`PRIMARY_CLASSIFICATION` or `DICTIONARY_CANDIDATE`. Let
`candidate_classes` be the distinct candidate classes represented by those
hits.

- `NO_CANDIDATE_FOUND` iff `candidate_hits` is empty. Context hits, including
  hits in `OTHER_MEMBER`, do not alter this result.
- `FOUND_CANDIDATE` iff `candidate_hits` is non-empty and
  `len(candidate_classes) == 1`.
- `AMBIGUOUS` iff `candidate_hits` is non-empty and
  `len(candidate_classes) >= 2`.

The last condition is an explicit operational ambiguity: the fixed industry
probe set locates more than one fixed candidate class. It does not claim the
classes are semantically incompatible, nor does it infer hierarchy or mapping.
There is no result path based on context-probe counts.

## Required evidence output

Record all 542 members in deterministic `(name, zip_local_header_offset)`
order. For every industry or context hit retain member path, member SHA-256,
uncompressed length, raw uncompressed byte offset, matched bytes in hex and
ASCII only when printable, and ZIP local-header/data offsets. Record archive
identity, retained-index match, this rules version, candidate hits and context
hits separately. Preserve undecodable bytes as hex.

The v2 result is a bounded locator report. `FOUND_CANDIDATE` does not establish
code→name, encoding, parent/child hierarchy, field meaning, complete TDX
research-industry level-2 coverage, or production readiness. No result closes
#46/#6. No archive bytes are committed and no network access is permitted.
