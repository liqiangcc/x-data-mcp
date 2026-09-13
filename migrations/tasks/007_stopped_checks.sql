-- Explicit lifecycle migration only. Upgrade disables FK enforcement before its
-- transaction, then validates ALL foreign keys before commit and re-enables it.
CREATE TABLE tasks_v7(
 task_id TEXT PRIMARY KEY, command_key_json TEXT NOT NULL, status TEXT NOT NULL CHECK(status IN ('pending','running','cancel_requested','needs_verification','succeeded','partially_failed','failed','cancelled','interrupted')),
 state_revision INTEGER NOT NULL CHECK(state_revision>=0), accepted_run_id TEXT NOT NULL CHECK(length(accepted_run_id)>0), owner_run_id TEXT,
 created_at TEXT NOT NULL, last_progress_at TEXT NOT NULL, frozen_work_json TEXT NOT NULL, result_json TEXT,
 retry_of TEXT REFERENCES tasks(task_id)
) STRICT;
INSERT INTO tasks_v7 SELECT * FROM tasks;
DROP TABLE tasks;
ALTER TABLE tasks_v7 RENAME TO tasks;
CREATE INDEX tasks_status_created ON tasks(status,created_at,task_id);
CREATE TABLE stopped_write_checks(
 verification_task_id TEXT PRIMARY KEY REFERENCES tasks(task_id),
 target_task_id TEXT NOT NULL REFERENCES tasks(task_id),
 audit_json TEXT NOT NULL,
 audit_sha256 TEXT NOT NULL
) STRICT;
