# Issue #6 catalog core slice

Status: implementation design and test mapping for the first local vertical
slice. This document does not claim a real-source or AC-12 pass.

## Scope

The slice models only the confirmed Issue #6 scope: securities currently
provided by a source for Shanghai/Shenzhen A shares, with explicit source
identity and Dongcai level-2 industry information. It adds no real source
adapter, scheduler, full-market collection, K-line collection, MCP tool,
delisting inference, or historical industry-change policy.

The source port returns a typed snapshot. The source must state its source
name, acquisition time, explicit exchange/security type, source security ID,
and completeness evidence. The core never derives an exchange from a code
prefix and never treats a non-empty list as complete.

## Chosen local contract

- `SecurityCatalogSource` is the source port; `CatalogSnapshot` carries
  explicit records and `CompletenessEvidence`.
- `SecurityRecord` keeps the original symbol, name, explicit exchange, source
  security ID, source name/time, and an `IndustryInfo` value. Industry is
  `Assigned`, `Missing(reason)`, or `Conflicted(reason)`; missing/conflicted
  industry does not remove the security.
- A complete snapshot must be non-empty, have a positive expected count equal
  to the record count, contain only Shanghai/Shenzhen A shares, and have no
  duplicate or contradictory identity mappings. Unknown/incomplete evidence,
  required-field errors, and source failures reject publication.
- When an effective version exists, every prior `(exchange, symbol)` member
  must occur in the candidate. A missing member rejects the whole candidate;
  it is not inferred to be delisted.
- Validation is finished before persistence. `JsonCatalogStore` writes one
  complete JSON document to a temporary sibling and renames it over the
  effective file. Readers therefore see either the old complete version or the
  new complete version. A rejected candidate never writes.
- A refresh result distinguishes `Published`, `Rejected` (old version
  retained), and `NotReady` (no old version exists). The latest effective
  version is readable after reconstructing `JsonCatalogStore`.

Routine representation choices are local: Rust enums are serde-tagged,
timestamps are RFC3339 UTC, the effective file is
`catalog.effective.json`, and the current implementation generates a local
version ID from `published_at = Utc::now()` plus a stable content-independent
prefix. This is not the final external version/time contract. These choices
do not settle the still-open source encoding, pagination snapshot,
authorization, or deployment contracts.

## Acceptance mapping

| Issue #6 AC | This slice | Evidence |
| --- | --- | --- |
| AC-01 | Typed target scope; explicit exchange/type; out-of-scope records reject | `tests/catalog.rs` target and scope tests |
| AC-02 | Dongcai level-2 is represented explicitly; missing/conflicted industry is retained and labeled | `tests/catalog.rs` industry tests |
| AC-03 | Published records expose identity and source mapping through `current()` | storage/restart test |
| AC-04 | Source/integrity/identity failures retain the old effective version | rejection-preserves-old tests |
| AC-05 | Missing industry remains a record and does not block publication | industry-missing test |
| AC-06 | Only a fully validated candidate is atomically published | publish/restart test |
| AC-07 | First failure returns `NotReady` and has no effective empty catalog | first-failure test |
| AC-08 | Prior members missing from a candidate reject publication without deletion | member-disappearance test |
| AC-09 | Unknown/incomplete count and duplicate records reject publication | completeness/duplicate tests |
| AC-10 | Required identity and source-mapping conflicts reject publication | identity-conflict tests |
| AC-11 | Industry conflict retains the security with an explicit conflict reason | industry-conflict test |
| AC-12 | Not implemented here: no real source, full-directory coverage, access or deployment evidence | explicitly reported in handoff |

The tests use only a fake source and temporary storage. They prove the core
rules and local persistence boundaries, not supplier coverage, live access,
or the final real-source contract.
