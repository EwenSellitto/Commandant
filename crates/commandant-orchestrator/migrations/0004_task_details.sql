-- What each task is: a command or a prompt turn, the directory it was asked
-- to run in, and a prompt's session. NULL for tasks from before.
ALTER TABLE tasks ADD COLUMN kind TEXT;           -- command | prompt
ALTER TABLE tasks ADD COLUMN cwd TEXT;
ALTER TABLE tasks ADD COLUMN session_id TEXT;

-- For `task ls --node`.
CREATE INDEX tasks_node_created_at ON tasks (node_id, created_at DESC);
