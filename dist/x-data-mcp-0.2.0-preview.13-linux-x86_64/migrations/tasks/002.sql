-- Explicit schema upgrade; ordinary reads never install this index.
CREATE INDEX tasks_status_created ON tasks(status,created_at,task_id);
CREATE INDEX occupancy_task ON scope_occupancy(task_id);
