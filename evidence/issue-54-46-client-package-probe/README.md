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
