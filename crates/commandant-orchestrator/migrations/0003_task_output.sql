-- What each task printed: its last bytes, in chunks tagged with their stream.
CREATE TABLE task_output (
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    seq     INTEGER NOT NULL,
    stream  INTEGER NOT NULL,           -- an OutputStream
    data    BLOB NOT NULL,
    PRIMARY KEY (task_id, seq)
);

-- Set once a task's output is dropped, so only the newest tasks keep theirs.
-- Tasks from before kept none.
ALTER TABLE tasks ADD COLUMN output_pruned INTEGER NOT NULL DEFAULT 0;
UPDATE tasks SET output_pruned = 1;
