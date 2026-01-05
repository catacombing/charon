//! SQLite database handling.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use sqlx::Pool;
use sqlx::sqlite::{Sqlite, SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous};
use tokio::sync::SetOnce;
use tracing::error;

use crate::Error;

/// Reference counted database pool.
#[derive(Clone)]
pub struct Db {
    pool: Arc<SetOnce<Pool<Sqlite>>>,
}

impl Db {
    pub fn new() -> Result<Self, Error> {
        let db_path = Self::path()?;
        let tiles_path =
            dirs::cache_dir().ok_or(Error::MissingCacheDir)?.join("charon/tiles.sqlite");

        // Migrate tile storage DB to a more generic name.
        if tiles_path.exists() && !db_path.exists() {
            fs::rename(&tiles_path, &db_path)?;
        }

        let options = SqliteConnectOptions::new()
            .filename(db_path)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .create_if_missing(true);

        // Initialize DB connection in the background.
        let pool = Arc::new(SetOnce::new());
        let future_pool = pool.clone();
        tokio::spawn(async move {
            if let Err(err) = Self::init_pool(options, future_pool).await {
                error!("Failed to initialize SQLite pool: {err}");
            }
        });

        Ok(Self { pool })
    }

    /// Get access to the underlying pool.
    pub async fn pool(&self) -> &Pool<Sqlite> {
        self.pool.wait().await
    }

    /// Get the storage path for the sqlite DB.
    pub fn path() -> Result<PathBuf, Error> {
        Ok(dirs::cache_dir().ok_or(Error::MissingCacheDir)?.join("charon/storage.sqlite"))
    }

    /// Asynchronously initialize the database pool.
    async fn init_pool(
        options: SqliteConnectOptions,
        setter: Arc<SetOnce<Pool<Sqlite>>>,
    ) -> Result<(), Error> {
        let pool = Pool::connect_with(options).await?;

        // Run database migrations.
        sqlx::migrate!("./migrations").run(&pool).await?;

        let _ = setter.set(pool);

        Ok(())
    }
}
