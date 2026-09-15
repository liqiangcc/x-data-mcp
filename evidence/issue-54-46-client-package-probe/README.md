# issue-54-46-client-package-probe

Final bounded slice for #46/#54: probe ONE official TDX client install package for
documentation or plain-data members that could close the remaining authority gaps.
Mechanical inventory only — no contract claims.

## Package selection (recorded in plan.json)

Official download page `www.tdx.com.cn/products/singledown.html` lists:

- `new_tdx64.exe` — primary 64-bit Windows client (185,469,360 B, LM 2026-08-04).
  **NOT_RUN**: no innoextract/7z/cabextract/unshield on this box; cannot be
  unpacked under allowed tooling. Not downloaded.
- `new_tdx.exe` — 32-bit Windows client. Same NOT_RUN.
- `com.tdx.tdxcfv_7.64_amd64.deb` — Kylin Linux amd64 client, official package
  (`tdx <nie@tdx.com.cn>`, 122,990,144 B, LM 2024-07-05). **SELECTED** — the only
  extractable official package; `ar`/`dpkg-deb` are installed. One-package limit
  spent here; recorded in retained-inputs.

Authorization: liqiangcc/x-data-mcp#71 comment-5676627357 (public downloads only).

## Files

- `plan.json` — frozen enumeration, selection rationale, extraction rules,
  allowed extensions, keyword list, comparison rules, status taxonomy, prohibitions.
- `scan_client_package.py` — deterministic scanner (dpkg-deb -x to /tmp).
- `test_scan_client_package.py` — 7 unit tests.
- `result.json` — generated output (result commit).
- `determinism.json` — two clean-checkout runs, byte-identical.

## Prohibited actions (enforced)

No disassembly/decompilation/strings-extraction on binaries; no wine/execution;
no semantic inference from binary contents; only `.help/.chm/.hlp/.txt/.pdf/.cfg/.dat/.ini`
members inventoried.

## Results (terminal)

Package: `com.tdx.tdxcfv` 7.64 amd64, 1,049 files, 29 doc/data members.

- **incon.dat**: `FOUND_MEMBER_CORROBORATION` — the client ships the same
  multi-namespace code→name dictionary (65,853 B), different bytes vs the gen-2
  gbbq.zip copy (67,543 B): one-generation-older, gen-2 adds `#SGXHY`/`#JPHY`
  sections; deb copy covers tdxhy codes T 110/111 (missing degenerate `T00`) and
  X 348/349 (missing `X4006`). Proves the client consumes this dictionary;
  proves nothing about tdxhy.cfg field positions.
- **All other same-name targets**: `ABSENT_IN_PACKAGE` — no tdxhy.cfg,
  tdxhy2.cfg, tdxzs*.cfg, gbbq, gbbq.map, base.dbf/base.map, infoharbor.*,
  infoharbor_ex.* ship in the client package (they are server-downloaded data).
- **Client manual corpus** (`tdxw-left.html`, 651KB): `FOUND_BUSINESS_LEVEL_ONLY`
  — prose states precise auto-adjustment incl. 送股/配股/分红, front/back
  adjustment, and rights-database maintenance/display. No binary binding.
- **Keyword scan** across all 29 members: every hit is column-help prose,
  industry-code text (e.g. `C13|农副食品加工业`), or coincidental bytes inside
  binary resources (tdxres.dat). No member documents GBBQ record layout,
  C1–C4 order/units/float32/rounding, or tdxhy.cfg field roles.
- **#46 remaining gaps**: `NOT_FOUND` — field↔code-space declaration,
  f0/f1/f3/f4 roles, A-share scope, tdxhy2 f1/f5 roles.
- **#54 remaining gap**: `NOT_FOUND` — business-field→GBBQ binary C1–C4 binding.
- **Windows installer**: `NOT_RUN` (no unpacker); **client behavior**: `NOT_RUN`
  (prohibited).

Result sha256 `0eb8f6921052b2ba63e61007f7e49b58ec591d98a8ca7a8b86116d2fd12f6c21`;
two clean-checkout runs byte-identical.
