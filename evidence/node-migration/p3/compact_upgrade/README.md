# compact_upgrade — NM-073 evidence

Rust oracle + Node runner output for
`scripts/node_migration_p3/scenarios/compact_upgrade.jsonl`
(32 ops, 0 diffs via `diff_compare.mjs`).

Coverage: `initialize`/`initialize19` (fresh create, File-exists),
`upgrade_compact` (18→19 success on empty; rejections: already-19,
version tamper `user_version=7`, checksum tamper, populated business
table `FRESH_REBUILD_REQUIRED`, missing path); `schema` = sha256 over
canonical DDL dump — migrated == fresh-19 byte-identical;
`compact_admin` (coordinator open via lease + open_writer +
verify_wal + schema-identity) checkpoint_passive/checkpoint_idle/
shutdown metrics; `coord_tx`/`coord_read` (coordinator transaction +
read boundary); `coord_write_leak` (recover_leak →
`MARKET_INTEGRITY: write closure leaked a transaction`);
post-shutdown rejection (`coordinator is shut down`); non-WAL
rejection (`journal_mode is not WAL`).

Migrated-fixture coverage: probe runs every `compact:true` seed
scenario a second time under `DIFF_INIT=up` (initialize + upgrade)
— `compact_{read,write,tail,commit_surface,tamper*}.migrated`
all PASS, 0 diffs.

sha256:
  rust.jsonl 5ceb12e28231cf2641b8a33974312f78e36dd9686d9cc0f2ec6c76467bf68bb6
  node.jsonl 189974daa8d5d5b312f136491be158057049acb919f221b2af4f9cc4a128f417
