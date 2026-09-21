CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, checksum TEXT NOT NULL, applied_at TEXT NOT NULL) STRICT;
CREATE TABLE tasks(
 task_id TEXT PRIMARY KEY, command_key_json TEXT NOT NULL, status TEXT NOT NULL CHECK(status IN ('pending','running','cancel_requested','needs_verification','succeeded','partially_failed','failed','cancelled')),
 state_revision INTEGER NOT NULL CHECK(state_revision>=0), accepted_run_id TEXT NOT NULL CHECK(length(accepted_run_id)>0), owner_run_id TEXT,
 created_at TEXT NOT NULL, last_progress_at TEXT NOT NULL, frozen_work_json TEXT NOT NULL, result_json TEXT,
 retry_of TEXT REFERENCES tasks(task_id)
) STRICT;
CREATE TABLE requests(
 request_id TEXT PRIMARY KEY CHECK(length(request_id)>0), command_json TEXT NOT NULL, command_digest TEXT NOT NULL,
 outcome_kind TEXT NOT NULL CHECK(outcome_kind IN ('task','terminal')),task_id TEXT UNIQUE REFERENCES tasks(task_id),terminal_json TEXT,
 CHECK((outcome_kind='task' AND task_id IS NOT NULL AND terminal_json IS NULL) OR (outcome_kind='terminal' AND task_id IS NULL AND terminal_json IS NOT NULL))
) STRICT;
CREATE TABLE task_items(
 task_id TEXT NOT NULL REFERENCES tasks(task_id),item_id TEXT NOT NULL,target_json TEXT NOT NULL,progress_unit TEXT NOT NULL,
 frozen_inputs_json TEXT NOT NULL,scope_keys_json TEXT NOT NULL,stage TEXT NOT NULL CHECK(stage IN ('pending','running','finished')),outcome_json TEXT,
 PRIMARY KEY(task_id,item_id),CHECK((stage='finished')=(outcome_json IS NOT NULL))
) STRICT;
CREATE TABLE task_scopes(task_id TEXT NOT NULL REFERENCES tasks(task_id),scope_key TEXT NOT NULL,PRIMARY KEY(task_id,scope_key)) STRICT;
CREATE TABLE scope_occupancy(scope_key TEXT PRIMARY KEY,task_id TEXT NOT NULL REFERENCES tasks(task_id),state_revision INTEGER NOT NULL CHECK(state_revision>=0)) STRICT;
