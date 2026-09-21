-- Durable execution-start facts, written atomically with Change::StartItem.
-- Rows are never updated, deleted, or backfilled; absence only means the start
-- is not provable (pre-upgrade history has no rows).
CREATE TABLE task_item_execution_starts (
 task_id TEXT NOT NULL,
 item_id TEXT NOT NULL,
 run_id TEXT NOT NULL CHECK(length(run_id)>0),
 start_revision INTEGER NOT NULL CHECK(start_revision>0),
 started_at TEXT NOT NULL,
 PRIMARY KEY(task_id,item_id),
 FOREIGN KEY(task_id,item_id) REFERENCES task_items(task_id,item_id)
) STRICT;
CREATE TRIGGER task_item_execution_starts_no_update
 BEFORE UPDATE ON task_item_execution_starts
 BEGIN SELECT RAISE(ABORT,'execution start facts are immutable'); END;
CREATE TRIGGER task_item_execution_starts_no_delete
 BEFORE DELETE ON task_item_execution_starts
 BEGIN SELECT RAISE(ABORT,'execution start facts are immutable'); END;
