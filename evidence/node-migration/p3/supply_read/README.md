# NM-074 supply_read — differential evidence (schema-18)

`supply_read.jsonl` — 66 ops, 0 diffs (oracle + Node emits committed).

- rust.jsonl / node.jsonl — per-op emit streams (status/code/message/result/unchanged).
- Fixture: schema-18 + valid_daily + valid_yearly + nm052_result(pub=res-ch,nohead) + results + check + catalog + yearly_result seeds.
- Covers: query (daily/yearly/catalog, latest + explicit-version + invalid arms), versions, verify/verify_versions, task/task_list (authorized + forbidden + not-found + verified publication), availability (ok + integrity_failure capture), range_result/yearly_result (ok + missing), verify_pub/verify_stop (ok + NeedsVerification), verify_range/verify_yearly (ok + Integrity), tamper arms (content_digest, market_update_results.result_sha256, yearly_execution_results.audit_json, publication_checks.audit_json, stopped_write_checks.audit_json), historical explicit-version reads.
