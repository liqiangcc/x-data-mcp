# Issue #135: tdxhy.cfg / tdxhy2.cfg structure evidence

Evidence-only offline scan of exactly two members from the retained #133
archive. The archive is not committed.

## Frozen input and rules

- Archive: `/tmp/official-base.zip`
- Bytes: `16099192`
- SHA-256: `39de51e26c0ee8fb3fd970ee870f4cb955b8fbbd7f84d752157f3998e49b71d9`
- Retained member identity index: `evidence/issue-6-tdx-dbf-source/official/archive-20260913.json`
- Rules freeze: `248f51e6b7268c7b93851bd867181f930ee85c79`
- Target members: `tdxhy.cfg`, `tdxhy2.cfg` only
- Fixed samples: `688318`, `600000`, `000001`

Run offline with:

```sh
python3 evidence/issue-135-tdxhy-structure/scan_tdxhy_structure.py \
  /tmp/official-base.zip \
  evidence/issue-135-tdxhy-structure/scan-result.json
```

The scanner validates archive bytes/SHA and both target member bytes/SHA against
the retained index before producing output. Every retained sample line and
six-digit token hit carries deterministic raw hex; optional ASCII is display
metadata only. In complete token statistics, each hit tuple is
`[member_offset, line_number, line_offset, field_index, raw_hex]`; fixed sample
hits additionally retain the complete raw line hex. No unknown encoding is
guessed.

## Result

`scan-result.json` records the complete byte structure, line endings, records,
blank lines, literal-`|` field-count distribution, fixed sample locations, and
six-digit token set/overlap/occurrence/cardinality facts. The mechanically
selected classification is `STRUCTURAL_RELATION_AMBIGUOUS` because the two
member token sets overlap and at least one intersecting token has more than one
line occurrence on one side under the frozen rule. This is a structural
classification only; it does not infer security identity, parent/child
hierarchy, industry level, code-to-name semantics, or encoding. It does not
close #46/#6 or establish production readiness.

The fixed mechanical totals are:

- `tdxhy.cfg`: 150332 bytes, SHA-256
  `df7cb7051d0f73a0d1381e9b36eafa33c8f5081d28ddef87cad406dd28ed8ea6`;
  5659 CRLF-terminated lines/records, 0 blank lines, field-count distribution
  `{6: 5659}`, and 6020 distinct six-digit tokens.
- `tdxhy2.cfg`: 249145 bytes, SHA-256
  `401cfc600e74a95ea750713b96d2caa348157f70f52703478785981810cee480`;
  6753 CRLF-terminated lines/records, 0 blank lines, field-count distribution
  `{5: 6753}`, and 7338 distinct six-digit tokens.
- Fixed samples `688318`, `600000`, and `000001` occur once in each member.
- Token sets: left/right/intersection cardinalities `6020/7338/5660`, with
  only-left `360` and only-right `1678`; maximum intersecting-token line
  Cartesian cardinality is `116`.

The committed result was generated only from the fixed archive and target
members. Two independent clean-checkout runs must be byte-identical; their
output SHA-256 values are recorded in the Issue/PR evidence comment. At
`e08411131612834e5380813491e7e78f1b4cf051`, detached clean checkouts `a` and
`b` both produced SHA-256
`f4d36f0e8356b8df1ddbcd1c50b963fdbf22349e4dca424599daac82899e8ddb`; `cmp`
reported identical output. Both checkouts were detached at exact HEAD
`e08411131612834e5380813491e7e78f1b4cf051`; mechanical comparison with the
committed result reported `MECHANICAL_MATCH=PASS`. Each scanner run
independently revalidated the fixed archive and target member identities before
scanning.

## Scope exclusions

No other archive member was read for new semantic evidence. This slice adds no
network access, token search, third-party implementation, MCP/SQLite/QFQ or
OfficialFilePipeline change, fallback, or production wiring.
