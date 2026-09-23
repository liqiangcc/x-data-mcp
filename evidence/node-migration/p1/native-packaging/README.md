# NM-010 P1 native addon packaging — evidence

Baseline `a6338e812b50c0978614170bfd4a76bec624e4a0` (`migration/node-devin`). Node v20.19.2.

## Contents

- `probe-results.jsonl` — 7 cases, all PASS.
- `results.json` — artifact + ABI metadata, reproducibility, install path, stop rules, npm ci record.
- `nm010-build1.json` / `nm010-build2.json` — raw `build.mjs` output for the two rebuilds.
- `SHA256SUMS` — over all four files (excludes itself).

## Reproduction

```sh
npm ci                 # installs deps; postinstall -> npm run build:native
npm run verify:native  # smoke: flock + SO_PEERCRED, exit 0
```

Build alone: `npm run build:native` → `gcc -shared -fPIC -O2 -I/usr/include/node` (override via `CC`, `NODE_INCLUDE`).

## Proven

- **Byte-identical rebuild**: three builds (NM-009 artifact + two fresh) all produce sha256 `11289943e6…`.
- **Hard-stop rules**: missing headers (`NODE_INCLUDE=/nonexistent`) → exit 1 BLOCKED; missing compiler → exit 1 BLOCKED; missing artifact → `index.mjs` throws (exit 3). No fallback anywhere.
- **Real `npm ci`**: 39 packages, postinstall built the addon, `verify:native` PASS, 0 vulnerabilities.
- ABI metadata emitted: node v20.19.2, napi_version 9, node_module_version 115.
