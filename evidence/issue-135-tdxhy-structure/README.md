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
metadata only. No unknown encoding is guessed.

## Result

`scan-result.json` records the complete byte structure, line endings, records,
blank lines, literal-`|` field-count distribution, fixed sample locations, and
six-digit token set/overlap/occurrence/cardinality facts. The mechanically
selected classification is `STRUCTURAL_RELATION_AMBIGUOUS` because the two
member token sets overlap and at least one intersecting token occurs on more
than one line in a member. The output reports the exact overlap and cardinality;
it does not infer security identity, parent/child hierarchy, industry level,
code-to-name semantics, or encoding. It does not close #46/#6 or establish
production readiness.

The committed result was generated only from the fixed archive and target
members. Two independent clean-checkout runs must be byte-identical; their
output SHA-256 values are recorded in the Issue/PR evidence comment.

## Scope exclusions

No other archive member was read for new semantic evidence. This slice adds no
network access, token search, third-party implementation, MCP/SQLite/QFQ or
OfficialFilePipeline change, fallback, or production wiring.
