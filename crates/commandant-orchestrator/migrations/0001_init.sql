CREATE TABLE admin_tokens (
    hash       TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL
);

CREATE TABLE join_tokens (
    hash       TEXT PRIMARY KEY,
    created_at INTEGER NOT NULL,
    expires_at INTEGER,                 -- NULL = never
    reusable   INTEGER NOT NULL,
    uses       INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE nodes (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    hostname    TEXT NOT NULL,
    os          TEXT NOT NULL,
    arch        TEXT NOT NULL,
    version     TEXT NOT NULL,
    secret_hash TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    last_seen   INTEGER NOT NULL
);

CREATE TABLE tasks (
    id          TEXT PRIMARY KEY,
    node_id     TEXT NOT NULL REFERENCES nodes(id) ON DELETE CASCADE,
    argv        TEXT NOT NULL,          -- JSON array
    status      TEXT NOT NULL,          -- running | succeeded | failed | cancelled | lost
    exit_code   INTEGER,
    error       TEXT,
    created_at  INTEGER NOT NULL,
    finished_at INTEGER
);

CREATE INDEX tasks_created_at ON tasks (created_at DESC);
