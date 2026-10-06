//! SQLite persistence for nodes, tokens and task history.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use commandant_common::lookup::{self, Match};
use commandant_proto::{TaskFinished, TaskOutput};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use sqlx::types::Json;

pub use commandant_common::time::now;

/// Error recorded on tasks whose node went away mid-run.
pub const NODE_DISCONNECTED: &str = "node disconnected";
/// Only this many of the newest tasks keep their output.
pub const KEPT_OUTPUTS: u32 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Lost,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Lost => "lost",
        }
    }

    pub fn of(finished: &TaskFinished) -> Self {
        if finished.cancelled {
            Self::Cancelled
        } else if finished.exit_code == Some(0) {
            Self::Succeeded
        } else {
            Self::Failed
        }
    }
}

/// One command run, or one prompt turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    Command,
    Prompt,
}

impl TaskKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Prompt => "prompt",
        }
    }
}

/// What a task is, as recorded when it is dispatched.
#[derive(Debug, Clone)]
pub struct NewTask {
    pub kind: TaskKind,
    /// What `task ls` shows.
    pub argv: Vec<String>,
    /// Empty when the worker picks.
    pub cwd: String,
    /// The session a prompt continues; empty for a new one, whose id the
    /// worker reports when the turn ends.
    pub session_id: String,
}

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct NodeRecord {
    pub id: String,
    pub name: String,
    pub hostname: String,
    pub os: String,
    pub arch: String,
    pub version: String,
    pub created_at: i64,
    pub last_seen: i64,
}

/// What a worker reports about itself in its Hello.
pub struct NodeFacts<'a> {
    pub hostname: &'a str,
    pub os: &'a str,
    pub arch: &'a str,
    pub version: &'a str,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TaskRecord {
    pub id: String,
    pub node_id: String,
    pub node_name: String,
    #[sqlx(json)]
    pub argv: Vec<String>,
    pub status: String,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    pub created_at: i64,
    pub finished_at: Option<i64>,
    pub output_pruned: bool,
    /// The columns below are None for tasks from before they existed.
    pub kind: Option<String>,
    pub cwd: Option<String>,
    pub session_id: Option<String>,
}

impl TaskRecord {
    /// The final event of a task that is over, as its watchers see it.
    pub fn finished(&self) -> TaskFinished {
        TaskFinished {
            task_id: self.id.clone(),
            exit_code: self.exit_code,
            error: self.error.clone().unwrap_or_default(),
            cancelled: self.status == TaskStatus::Cancelled.as_str(),
            session_id: self.session_id.clone().unwrap_or_default(),
            ..Default::default()
        }
    }
}

impl Store {
    pub async fn open(path: &Path) -> Result<Self> {
        create_private(path)?;
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    #[cfg(test)]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    // --- admin tokens -------------------------------------------------------

    pub async fn admin_token_hashes(&self) -> Result<Vec<String>> {
        Ok(sqlx::query_scalar("SELECT hash FROM admin_tokens")
            .fetch_all(&self.pool)
            .await?)
    }

    /// The oldest admin token the database holds in full.
    pub async fn admin_token(&self) -> Result<Option<String>> {
        Ok(sqlx::query_scalar(
            "SELECT token FROM admin_tokens WHERE token IS NOT NULL ORDER BY created_at LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn add_admin_token(&self, hash: &str, token: &str) -> Result<()> {
        sqlx::query("INSERT INTO admin_tokens (hash, token, created_at) VALUES (?, ?, ?)")
            .bind(hash)
            .bind(token)
            .bind(now())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Keeps the full token of an admin token stored as a hash only. Returns
    /// false if no admin token has that hash.
    pub async fn keep_admin_token(&self, hash: &str, token: &str) -> Result<bool> {
        let res = sqlx::query("UPDATE admin_tokens SET token = ? WHERE hash = ?")
            .bind(token)
            .bind(hash)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() == 1)
    }

    // --- join tokens --------------------------------------------------------

    pub async fn add_join_token(
        &self,
        hash: &str,
        expires_at: Option<i64>,
        reusable: bool,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO join_tokens (hash, created_at, expires_at, reusable) VALUES (?, ?, ?, ?)",
        )
        .bind(hash)
        .bind(now())
        .bind(expires_at)
        .bind(reusable)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Atomically uses a join token. Returns false if unknown, expired or spent.
    pub async fn consume_join_token(&self, hash: &str) -> Result<bool> {
        let res = sqlx::query(
            "UPDATE join_tokens SET uses = uses + 1
             WHERE hash = ? AND (reusable = 1 OR uses = 0)
               AND (expires_at IS NULL OR expires_at > ?)",
        )
        .bind(hash)
        .bind(now())
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    // --- nodes --------------------------------------------------------------

    /// Returns false if another node has the name.
    pub async fn insert_node(
        &self,
        id: &str,
        name: &str,
        secret_hash: &str,
        facts: &NodeFacts<'_>,
    ) -> Result<bool> {
        let ts = now();
        let inserted = sqlx::query(
            "INSERT INTO nodes (id, name, hostname, os, arch, version, secret_hash, created_at, last_seen)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(name)
        .bind(facts.hostname)
        .bind(facts.os)
        .bind(facts.arch)
        .bind(facts.version)
        .bind(secret_hash)
        .bind(ts)
        .bind(ts)
        .execute(&self.pool)
        .await;
        match inserted {
            Err(sqlx::Error::Database(db)) if db.is_unique_violation() => Ok(false),
            inserted => inserted.map(|_| true).map_err(Into::into),
        }
    }

    pub async fn node_secret_hash(&self, id: &str) -> Result<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT secret_hash FROM nodes WHERE id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    /// Refreshes a reconnecting node's facts and last-seen time.
    pub async fn update_node_facts(&self, id: &str, facts: &NodeFacts<'_>) -> Result<()> {
        sqlx::query(
            "UPDATE nodes SET hostname = ?, os = ?, arch = ?, version = ?, last_seen = ? WHERE id = ?",
        )
        .bind(facts.hostname)
        .bind(facts.os)
        .bind(facts.arch)
        .bind(facts.version)
        .bind(now())
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn touch_node(&self, id: &str) -> Result<()> {
        sqlx::query("UPDATE nodes SET last_seen = ? WHERE id = ?")
            .bind(now())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn node_name_taken(&self, name: &str) -> Result<bool> {
        Ok(
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM nodes WHERE name = ?)")
                .bind(name)
                .fetch_one(&self.pool)
                .await?,
        )
    }

    pub async fn list_nodes(&self) -> Result<Vec<NodeRecord>> {
        Ok(sqlx::query_as("SELECT * FROM nodes ORDER BY name")
            .fetch_all(&self.pool)
            .await?)
    }

    pub async fn delete_node(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM nodes WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // --- tasks --------------------------------------------------------------

    pub async fn insert_task(&self, id: &str, node_id: &str, task: &NewTask) -> Result<()> {
        sqlx::query(
            "INSERT INTO tasks (id, node_id, kind, argv, cwd, session_id, status, created_at)
             VALUES (?, ?, ?, ?, NULLIF(?, ''), NULLIF(?, ''), ?, ?)",
        )
        .bind(id)
        .bind(node_id)
        .bind(task.kind.as_str())
        .bind(Json(&task.argv))
        .bind(&task.cwd)
        .bind(&task.session_id)
        .bind(TaskStatus::Running.as_str())
        .bind(now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Records how a running task ended, as its worker reported, and the
    /// output it kept.
    pub async fn finish_task(&self, finished: &TaskFinished, output: &[TaskOutput]) -> Result<()> {
        let error = Some(finished.error.as_str()).filter(|e| !e.is_empty());
        let end = TaskEnd {
            status: TaskStatus::of(finished),
            exit_code: finished.exit_code,
            error,
            session_id: &finished.session_id,
        };
        self.record_end(&finished.task_id, end, output).await
    }

    /// Records how a running task ended and the output it kept, then drops
    /// the output of all but the newest tasks.
    async fn record_end(&self, id: &str, end: TaskEnd<'_>, output: &[TaskOutput]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        // A new session's first turn only now learns its session.
        let res = sqlx::query(
            "UPDATE tasks SET status = ?, exit_code = ?, error = ?, finished_at = ?,
                 session_id = COALESCE(session_id, NULLIF(?, ''))
             WHERE id = ? AND status = ?",
        )
        .bind(end.status.as_str())
        .bind(end.exit_code)
        .bind(end.error)
        .bind(now())
        .bind(end.session_id)
        .bind(id)
        .bind(TaskStatus::Running.as_str())
        .execute(&mut *tx)
        .await?;
        if res.rows_affected() == 1 {
            for (seq, chunk) in output.iter().enumerate() {
                sqlx::query(
                    "INSERT INTO task_output (task_id, seq, stream, data) VALUES (?, ?, ?, ?)",
                )
                .bind(id)
                .bind(seq as i64)
                .bind(chunk.stream)
                .bind(&chunk.data)
                .execute(&mut *tx)
                .await?;
            }
        }
        // Everything past the newest KEPT_OUTPUTS tasks.
        sqlx::query(
            "UPDATE tasks SET output_pruned = 1 WHERE output_pruned = 0 AND id IN
             (SELECT id FROM tasks ORDER BY created_at DESC, rowid DESC LIMIT -1 OFFSET ?)",
        )
        .bind(KEPT_OUTPUTS)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "DELETE FROM task_output WHERE task_id IN
             (SELECT id FROM tasks ORDER BY created_at DESC, rowid DESC LIMIT -1 OFFSET ?)",
        )
        .bind(KEPT_OUTPUTS)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Records that a task's node went away before it finished.
    pub async fn lose_task(&self, id: &str, output: &[TaskOutput]) -> Result<()> {
        let end = TaskEnd {
            status: TaskStatus::Lost,
            exit_code: None,
            error: Some(NODE_DISCONNECTED),
            session_id: "",
        };
        self.record_end(id, end, output).await
    }

    /// Tasks still running after a restart can never report back.
    pub async fn mark_running_tasks_lost(&self) -> Result<u64> {
        let res = sqlx::query(
            "UPDATE tasks SET status = ?, error = 'orchestrator restarted', finished_at = ?
             WHERE status = ?",
        )
        .bind(TaskStatus::Lost.as_str())
        .bind(now())
        .bind(TaskStatus::Running.as_str())
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// The newest tasks, of every node or only `node_id`'s.
    pub async fn list_tasks(&self, limit: u32, node_id: Option<&str>) -> Result<Vec<TaskRecord>> {
        Ok(sqlx::query_as(
            "SELECT t.*, n.name AS node_name FROM tasks t JOIN nodes n ON n.id = t.node_id
             WHERE ?1 IS NULL OR t.node_id = ?1
             ORDER BY t.created_at DESC, t.rowid DESC LIMIT ?2",
        )
        .bind(node_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Finds a task by id or unambiguous id prefix.
    pub async fn find_task(&self, needle: &str) -> Result<Match<TaskRecord>> {
        // The exact id first, else two that start with it: enough to tell
        // one from several.
        let tasks: Vec<TaskRecord> = sqlx::query_as(
            "SELECT t.*, n.name AS node_name FROM tasks t JOIN nodes n ON n.id = t.node_id
             WHERE substr(t.id, 1, length(?1)) = ?1 ORDER BY t.id = ?1 DESC LIMIT 2",
        )
        .bind(needle)
        .fetch_all(&self.pool)
        .await?;
        Ok(lookup::find(tasks, needle, |t| &t.id))
    }

    pub async fn task(&self, id: &str) -> Result<Option<TaskRecord>> {
        Ok(sqlx::query_as(
            "SELECT t.*, n.name AS node_name FROM tasks t JOIN nodes n ON n.id = t.node_id
             WHERE t.id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// The output a finished task kept, oldest first.
    pub async fn task_output(&self, id: &str) -> Result<Vec<TaskOutput>> {
        let chunks: Vec<(i32, Vec<u8>)> =
            sqlx::query_as("SELECT stream, data FROM task_output WHERE task_id = ? ORDER BY seq")
                .bind(id)
                .fetch_all(&self.pool)
                .await?;
        Ok(chunks
            .into_iter()
            .map(|(stream, data)| TaskOutput {
                task_id: id.to_string(),
                stream,
                data,
            })
            .collect())
    }
}

/// How a task ended.
struct TaskEnd<'a> {
    status: TaskStatus,
    exit_code: Option<i32>,
    error: Option<&'a str>,
    /// Empty when the worker reported none.
    session_id: &'a str,
}

/// Creates the database file readable by its owner only, since it holds the
/// admin token, or makes an existing one so. SQLite creates its `-wal` and
/// `-shm` files with the same permissions; ones left from before are fixed.
fn create_private(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(false);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    #[cfg(unix)]
    for suffix in ["", "-wal", "-shm"] {
        use std::os::unix::fs::PermissionsExt;
        let mut file = path.as_os_str().to_owned();
        file.push(suffix);
        let file = Path::new(&file);
        match std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(e).with_context(|| format!("restricting {}", file.display()));
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FACTS: NodeFacts<'static> = NodeFacts {
        hostname: "h",
        os: "linux",
        arch: "x86_64",
        version: "0",
    };

    async fn store(name: &str) -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("commandant-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::open(&dir.join("db.sqlite")).await.unwrap();
        for node in ["a", "b"] {
            assert!(store.insert_node(node, node, "x", &FACTS).await.unwrap());
        }
        (store, dir)
    }

    fn command(cwd: &str) -> NewTask {
        NewTask {
            kind: TaskKind::Command,
            argv: vec!["ls".into()],
            cwd: cwd.into(),
            session_id: String::new(),
        }
    }

    fn prompt(session_id: &str) -> NewTask {
        NewTask {
            kind: TaskKind::Prompt,
            argv: vec!["opencode".into(), "hi".into()],
            cwd: "/work".into(),
            session_id: session_id.into(),
        }
    }

    fn done(task_id: &str, session_id: &str) -> TaskFinished {
        TaskFinished {
            task_id: task_id.into(),
            exit_code: Some(0),
            session_id: session_id.into(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn tasks_know_their_kind_directory_and_session() {
        let (store, dir) = store("task-details").await;
        store.insert_task("c", "a", &command("/tmp")).await.unwrap();
        store
            .insert_task("old", "a", &prompt("ses_old"))
            .await
            .unwrap();
        store.insert_task("new", "b", &prompt("")).await.unwrap();

        // A new session's first turn learns its id once it ends; a resumed
        // one keeps the one it was asked for.
        assert_eq!(store.task("new").await.unwrap().unwrap().session_id, None);
        store
            .finish_task(&done("new", "ses_new"), &[])
            .await
            .unwrap();
        store
            .finish_task(&done("old", "ses_other"), &[])
            .await
            .unwrap();
        store.finish_task(&done("c", ""), &[]).await.unwrap();

        let c = store.task("c").await.unwrap().unwrap();
        assert_eq!(c.kind.as_deref(), Some("command"));
        assert_eq!(c.cwd.as_deref(), Some("/tmp"));
        assert_eq!(c.session_id, None);
        let old = store.task("old").await.unwrap().unwrap();
        assert_eq!(old.kind.as_deref(), Some("prompt"));
        assert_eq!(old.session_id.as_deref(), Some("ses_old"));
        let new = store.task("new").await.unwrap().unwrap();
        assert_eq!(new.session_id.as_deref(), Some("ses_new"));

        // Rows from before these columns list with them empty.
        sqlx::query(
            "INSERT INTO tasks (id, node_id, argv, status, created_at)
             VALUES ('legacy', 'a', '[\"ls\"]', 'succeeded', 0)",
        )
        .execute(store.pool())
        .await
        .unwrap();
        let legacy = store.task("legacy").await.unwrap().unwrap();
        assert_eq!(
            (legacy.kind, legacy.cwd, legacy.session_id),
            (None, None, None)
        );

        // Listing can keep to one node.
        let ids = |tasks: Vec<TaskRecord>| tasks.into_iter().map(|t| t.id).collect::<Vec<_>>();
        let all = store.list_tasks(10, None).await.unwrap();
        assert_eq!(all.len(), 4);
        assert_eq!(ids(store.list_tasks(10, Some("b")).await.unwrap()), ["new"]);
        let on_a = ids(store.list_tasks(10, Some("a")).await.unwrap());
        assert_eq!(on_a, ["old", "c", "legacy"]);

        std::fs::remove_dir_all(dir).unwrap();
    }
}
