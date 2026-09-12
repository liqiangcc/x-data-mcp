-- A reuse result is not a second publication of the same immutable version.
CREATE TABLE market_update_results (
 result_id TEXT PRIMARY KEY,
 task_id TEXT NOT NULL REFERENCES tasks(task_id),
 item_id TEXT NOT NULL,
 base_version_id TEXT NOT NULL REFERENCES data_versions(version_id),
 result_version_id TEXT NOT NULL REFERENCES data_versions(version_id),
 no_change INTEGER NOT NULL CHECK(no_change IN (0,1)),
 audit_json TEXT NOT NULL,
 result_sha256 TEXT NOT NULL,
 receipt_json TEXT NOT NULL,
 FOREIGN KEY(task_id,item_id) REFERENCES task_items(task_id,item_id),
 UNIQUE(task_id,item_id)
) STRICT;
