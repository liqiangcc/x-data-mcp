CREATE TABLE verification_runtime_guards(
 task_id TEXT PRIMARY KEY REFERENCES tasks(task_id),
 claim_revision INTEGER NOT NULL CHECK(claim_revision>0),
 descriptor_json TEXT NOT NULL,
 descriptor_sha256 TEXT NOT NULL
) STRICT;
CREATE TABLE verification_inspections(
 request_id TEXT PRIMARY KEY REFERENCES requests(request_id),
 target_task_id TEXT NOT NULL REFERENCES tasks(task_id),
 audit_json TEXT NOT NULL,
 audit_sha256 TEXT NOT NULL
) STRICT;
