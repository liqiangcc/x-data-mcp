-- Aggregate item counters on the task row. Single-item transitions update
-- them atomically instead of rewriting every item/occupancy row.
-- The table is rebuilt rather than ALTERed so the migration is mechanical
-- for any prior schema that lacks the counter columns.
CREATE TABLE tasks_v15(
 task_id TEXT PRIMARY KEY, command_key_json TEXT NOT NULL, status TEXT NOT NULL CHECK(status IN ('pending','running','cancel_requested','needs_verification','succeeded','partially_failed','failed','cancelled','interrupted')),
 state_revision INTEGER NOT NULL CHECK(state_revision>=0), accepted_run_id TEXT NOT NULL CHECK(length(accepted_run_id)>0), owner_run_id TEXT,
 created_at TEXT NOT NULL, last_progress_at TEXT NOT NULL, frozen_work_json TEXT NOT NULL, result_json TEXT,
 retry_of TEXT REFERENCES tasks_v15(task_id),
 item_count INTEGER NOT NULL DEFAULT 0 CHECK(item_count>=0),
 items_pending INTEGER NOT NULL DEFAULT 0 CHECK(items_pending>=0),
 items_running INTEGER NOT NULL DEFAULT 0 CHECK(items_running>=0),
 items_finished INTEGER NOT NULL DEFAULT 0 CHECK(items_finished>=0),
 items_succeeded INTEGER NOT NULL DEFAULT 0 CHECK(items_succeeded>=0)
) STRICT;
INSERT INTO tasks_v15(task_id,command_key_json,status,state_revision,accepted_run_id,owner_run_id,created_at,last_progress_at,frozen_work_json,result_json,retry_of,item_count,items_pending,items_running,items_finished,items_succeeded)
 SELECT task_id,command_key_json,status,state_revision,accepted_run_id,owner_run_id,created_at,last_progress_at,frozen_work_json,result_json,retry_of,
 (SELECT count(*) FROM task_items i WHERE i.task_id=tasks.task_id),
 (SELECT count(*) FROM task_items i WHERE i.task_id=tasks.task_id AND i.stage='pending'),
 (SELECT count(*) FROM task_items i WHERE i.task_id=tasks.task_id AND i.stage='running'),
 (SELECT count(*) FROM task_items i WHERE i.task_id=tasks.task_id AND i.stage='finished'),
 (SELECT count(*) FROM task_items i WHERE i.task_id=tasks.task_id AND i.stage='finished' AND json_extract(i.outcome_json,'$.success')=1)
 FROM tasks;
DROP TABLE tasks;
ALTER TABLE tasks_v15 RENAME TO tasks;
