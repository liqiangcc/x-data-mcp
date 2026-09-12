# Historical online TDX slice (not a production target)

Issue #55 moved these four files byte-for-byte from commit
`8e203832b77d38dd2c8b631ac38e427260f03c7c`:

- `src/tdx.rs`
- `scripts/tdx_bridge.py`
- `tests/tdx_live.rs`
- `tests/tdx_timeout.rs`

They retain the old eltdx source-day/source-year contract and old Rust imports
solely as historical implementation/test material. They are not exported,
compiled, installed or run by Cargo/CI. They are not a supported alternate
entrypoint; use Git history for their original build context.

The old live tests returned early without the environment switch and could
appear to pass without live requests. Their archive status is NOT_RUN, not a
new source pass. The bridge's known large-output pipe issue is not repaired.
Do not reactivate these files through a feature flag or runtime fallback.

Current production architecture: [official files](../../docs/production-source.md).
Existing source evidence and failure records under `evidence/` remain unchanged.
