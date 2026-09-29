//! SQLite persistence for nodes, tokens and task history.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use commandant_proto::TaskFinished;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};
use sqlx::types::Json;

pub use commandant_common::time::now;

/// Error recorded on tasks whose node went away mid-run.
pub const NODE_DISCONNECTED: &str = "node disconnected";

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
}

#[derive(Debug)]
pub enum InsertNodeError {
    NameTaken,
    Other(sqlx::Error),
}

impl std::error::Error for InsertNodeError {}

impl std::fmt::Display for InsertNodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NameTaken => f.write_str("a node with this name already exists"),
            Self::Other(e) => e.fmt(f),
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

    pub async fn insert_node(
        &self,
        id: &str,
        name: &str,
        secret_hash: &str,
        facts: &NodeFacts<'_>,
    ) -> Result<(), InsertNodeError> {
        let ts = now();
        sqlx::query(
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
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.is_unique_violation() => InsertNodeError::NameTaken,
            _ => InsertNodeError::Other(e),
        })?;
        Ok(())
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

    pub async fn insert_task(&self, id: &str, node_id: &str, argv: &[String]) -> Result<()> {
        sqlx::query(
            "INSERT INTO tasks (id, node_id, argv, status, created_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(node_id)
        .bind(Json(argv))
        .bind(TaskStatus::Running.as_str())
        .bind(now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn finish_task(
        &self,
        id: &str,
        status: TaskStatus,
        exit_code: Option<i32>,
        error: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE tasks SET status = ?, exit_code = ?, error = ?, finished_at = ?
             WHERE id = ? AND status = ?",
        )
        .bind(status.as_str())
        .bind(exit_code)
        .bind(error)
        .bind(now())
        .bind(id)
        .bind(TaskStatus::Running.as_str())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Records that a task's node went away before it finished.
    pub async fn lose_task(&self, id: &str) -> Result<()> {
        self.finish_task(id, TaskStatus::Lost, None, Some(NODE_DISCONNECTED))
            .await
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

    pub async fn list_tasks(&self, limit: u32) -> Result<Vec<TaskRecord>> {
        Ok(sqlx::query_as(
            "SELECT t.*, n.name AS node_name FROM tasks t JOIN nodes n ON n.id = t.node_id
             ORDER BY t.created_at DESC, t.rowid DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }
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
