CREATE TABLE publication_checks (
 verification_task_id TEXT PRIMARY KEY REFERENCES tasks(task_id),
 target_task_id TEXT NOT NULL REFERENCES tasks(task_id),
 audit_json TEXT NOT NULL,
 audit_sha256 TEXT NOT NULL
) STRICT;
