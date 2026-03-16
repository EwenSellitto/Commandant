// use std::{path::Path, sync::Arc, time::Duration, str::FromStr};
//
// use sqlx::{ConnectOptions as _, sqlite::{SqliteAutoVacuum, SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions}};
//
// #[derive(Debug, Clone)]
// pub(crate) struct Database {
//     pool: Arc<sqlx::Pool<sqlx::Sqlite>>,
// }
//
// impl Database {
//     pub async fn init(db_file: &str) -> Result<(), sqlx::Error> {
//         let mut conn = SqliteConnectOptions::from_str(db_file)?
//             .journal_mode(SqliteJournalMode::Wal)
//             .create_if_missing(true)
//             .connect()
//             .await?;
//
//         // Migrations HERE
//
//
//
//         Ok(())
//     }
//
//     pub async fn connect(db_file: &Path) -> Result<Self, sqlx::Error> {
//         let pool = SqlitePoolOptions::new()
//             .max_connections(20)
//             .idle_timeout(Duration::from_secs(60))
//             .acquire_timeout(Duration::from_secs(5))
//             .connect_with(
//                 SqliteConnectOptions::new()
//                     .auto_vacuum(SqliteAutoVacuum::Incremental)
//                     .journal_mode(SqliteJournalMode::Wal)
//                     .filename(db_file),
//             )
//             .await?;
//
//         Ok(Self {
//             pool: Arc::new(pool),
//         })
//     }
// }
