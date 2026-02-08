//! SQLite database handling.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use sqlx::sqlite::{Sqlite, SqliteConnectOptions, SqliteJournalMode};
use sqlx::{Pool, QueryBuilder};
use tokio::sync::SetOnce;
use tracing::error;

use crate::Error;
use crate::tiles::{OFFLINE_TILESERVER, TileIndex};

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

        // Ensure Charon's cache directory exists.
        let db_dir = db_path.parent().ok_or(Error::MissingCacheDir)?;
        fs::create_dir_all(db_dir)?;

        // Migrate tile storage DB to a more generic name.
        if tiles_path.exists() && !db_path.exists() {
            fs::rename(&tiles_path, &db_path)?;
        }

        let options = SqliteConnectOptions::new()
            .filename(db_path)
            .journal_mode(SqliteJournalMode::Wal)
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

    /// Add new offline tiles to the database.
    pub async fn insert_offline_tiles<B: AsRef<[u8]>>(
        &self,
        region_id: u32,
        tiles: &[(TileIndex, B)],
    ) -> Result<(), Error> {
        // Add the tile to the normal tile storage table.
        self.insert_tiles::<B>(OFFLINE_TILESERVER, tiles).await?;

        // Track the offline tiles for deletion/install status.
        let mut query = QueryBuilder::new("INSERT INTO offline_tile (region_id, x, y, z)");
        query.push_values(tiles, |mut b, (tile_index, _data)| {
            b.push_bind(region_id)
                .push_bind(tile_index.x)
                .push_bind(tile_index.y)
                .push_bind(tile_index.z);
        });
        query.push(" ON CONFLICT DO NOTHING ");

        query.build().execute(self.pool().await).await?;

        Ok(())
    }

    /// Add new tiles to the database.
    pub async fn insert_tiles<B: AsRef<[u8]>>(
        &self,
        tileserver: &str,
        tiles: &[(TileIndex, B)],
    ) -> Result<(), Error> {
        let mut query = QueryBuilder::new("INSERT INTO tile (tileserver, x, y, z, data)");
        query.push_values(tiles, |mut b, (tile_index, data)| {
            b.push_bind(tileserver)
                .push_bind(tile_index.x)
                .push_bind(tile_index.y)
                .push_bind(tile_index.z)
                .push_bind(data.as_ref());
        });
        query.push(
            " ON CONFLICT DO UPDATE SET data = excluded.data, ctime = unixepoch(), atime =  \
             unixepoch() ",
        );

        query.build().execute(self.pool().await).await?;

        Ok(())
    }

    /// Delete all offline tiles for a region
    pub async fn delete_offline_tiles(&self, region_id: u32) -> Result<(), Error> {
        let pool = self.pool().await;

        // Delete the tiles from the dedicated offline tiles table.
        sqlx::query("DELETE FROM offline_tile WHERE region_id = $1")
            .bind(region_id)
            .execute(pool)
            .await?;

        // Delete all tiles which aren't also part of any other region.
        #[rustfmt::skip]
        sqlx::query(
            "DELETE FROM tile
                WHERE tileserver = $1
                AND (x, y, z) NOT IN (SELECT x, y, z FROM offline_tile)"
        )
        .bind(OFFLINE_TILESERVER)
        .execute(pool)
        .await?;

        Ok(())
    }

    /// Close the SQLite database connection.
    pub async fn close(&self) {
        let pool = self.pool().await;

        // Store query planner optimization details on exit.
        //
        // While the sqlx connection options for sqlite allow doing this automatically
        // on close, doing this manually allows us to control when exactly it is run and
        // write potential errors to our log.
        if let Err(err) = sqlx::query("PRAGMA optimize").execute(pool).await {
            error!("SQLite optimize failed: {err}");
        }

        // Defragment and truncate database file.
        //
        // This takes a while ~1s and blocks other database operations, so we just do it
        // on exit.
        if let Err(err) = sqlx::query("VACUUM").execute(pool).await {
            error!("SQLite vacuum failed: {err}");
        }

        pool.close().await;
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
