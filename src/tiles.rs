//! Map tile handling.

use std::collections::{HashMap, LinkedList};
use std::iter;
use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

use calloop::channel::Sender;
use reqwest::Client;
use skia_safe::{Data, Image};
use sqlx::sqlite::SqliteRow;
use sqlx::{FromRow, Row};
use tokio::runtime::Handle as RuntimeHandle;
use tokio::task::{self, JoinHandle};
use tokio::time;
use tracing::error;

use crate::Error;
use crate::config::Config;
use crate::db::Db;
use crate::geometry::{Point, Size};

/// Width and height of a single tile.
pub const TILE_SIZE: i32 = 256;

/// Maximum tile zoom level.
pub const MAX_ZOOM: u8 = 19;

/// How frequently old tiles are deleted from the database.
///
/// The total number of tiles in the database will always be between
/// `config.tiles.max_fs_tiles` and `config.tiles.max_fs_tiles` +
/// `FS_CACHE_CLEANUP_INTERVAL`.
///
/// A higher cleanup interval means less frequent database queries to remove old
/// entries.
const FS_CACHE_CLEANUP_INTERVAL: u16 = 1_000;

/// Maximum db tile age in seconds before an online refresh is attempted.
const MAX_FS_CACHE_TIME: u64 = 60 * 60 * 24 * 7;

/// Time before a failed download will be re-attempted.
const FAILED_DOWNLOAD_DELAY: Duration = Duration::from_secs(3);

/// Map tile cache.
///
/// This manages the local cache for all rendered tiles and can either
/// automatically or manually download new tiles and store them.
pub struct Tiles {
    download_state: DownloadState,
    lru_cache: LruCache,
}

impl Tiles {
    pub fn new(
        client: Client,
        db: Db,
        tile_tx: Sender<TileIndex>,
        config: &Config,
    ) -> Result<Self, Error> {
        // Initialize filesystem cache and remove outdated maps.
        let fs_cache = FsCache::new(config, db);
        let cleanup_cache = fs_cache.clone();
        tokio::spawn(async move { cleanup_cache.clean_cache().await });

        let download_state =
            DownloadState { fs_cache, tile_tx, client, server: config.tiles.server.clone() };

        Ok(Self { download_state, lru_cache: LruCache::new(config.tiles.max_mem_tiles) })
    }

    /// Get a raster map tile.
    #[cfg_attr(feature = "profiling", profiling::function)]
    pub fn get(&mut self, index: TileIndex) -> &mut Tile {
        self.preload(index);
        self.try_get(index).unwrap()
    }

    /// Get a raster tile from the cache, without loading it if missing.
    #[cfg_attr(feature = "profiling", profiling::function)]
    pub fn try_get(&mut self, index: TileIndex) -> Option<&mut Tile> {
        self.lru_cache.get(&index)
    }

    /// Ensure a map tile is downloaded.
    #[cfg_attr(feature = "profiling", profiling::function)]
    pub fn preload(&mut self, index: TileIndex) {
        // Ignore tile if it is already cached.
        if self.lru_cache.has_tile(&index) {
            return;
        }

        let download_state = self.download_state.clone();
        self.lru_cache.insert(Tile::new(download_state, index));
    }

    /// Handle config updates.
    pub fn update_config(&mut self, config: &Config) -> bool {
        let mut dirty = false;

        if self.download_state.server != config.tiles.server {
            self.download_state.fs_cache.set_tileserver(config.tiles.server.clone());
            self.download_state.server = config.tiles.server.clone();
            self.lru_cache.clear();
            dirty = true;
        }
        if self.lru_cache.capacity != config.tiles.max_mem_tiles {
            self.lru_cache.capacity = config.tiles.max_mem_tiles;
        }
        if self.download_state.fs_cache.capacity != config.tiles.max_fs_tiles {
            self.download_state.fs_cache.capacity = config.tiles.max_fs_tiles;
        }

        dirty
    }

    /// Access the underlying SQLite tiles DB.
    pub fn fs_cache(&self) -> &FsCache {
        &self.download_state.fs_cache
    }
}

/// Iterator over positioned tiles.
pub struct TileIter {
    tile_index: TileIndex,
    origin: Point,

    max_tiles_x: u32,
    max_tiles_y: u32,
    tile_count: u32,
    tile_size: i32,

    screen_size: Size,
    scale: f64,

    index: u32,
}

impl TileIter {
    pub fn new(screen_size: Size, mut tile_index: TileIndex, offset: Point, scale: f64) -> Self {
        // Get position of the tile's top-left on the screen.
        let x_offset = (offset.x as f64 * scale).round() as i32;
        let y_offset = (offset.y as f64 * scale).round() as i32;
        let x_origin = screen_size.width as i32 / 2 - x_offset;
        let y_origin = screen_size.height as i32 / 2 - y_offset;

        // Get top-left tile's indices and offset.
        let tile_size = (TILE_SIZE as f64 * scale).round() as i32;
        let x_delta = ((x_origin + tile_size - 1) / tile_size).min(tile_index.x as i32);
        tile_index.x -= x_delta as u32;
        let y_delta = ((y_origin + tile_size - 1) / tile_size).min(tile_index.y as i32);
        tile_index.y -= y_delta as u32;
        let origin = Point::new(x_origin - x_delta * tile_size, y_origin - y_delta * tile_size);

        // Calculate maximum tile indices.

        let tile_count = 1 << tile_index.z as u32;

        let available_x = screen_size.width as i32 - origin.x;
        let tiles_x = ((available_x + tile_size - 1) / tile_size) as u32;
        let max_tiles_x = tiles_x.min(tile_count - tile_index.x);

        let available_y = screen_size.height as i32 - origin.y;
        let tiles_y = ((available_y + tile_size - 1) / tile_size) as u32;
        let max_tiles_y = tiles_y.min(tile_count - tile_index.y);

        Self {
            max_tiles_x,
            max_tiles_y,
            screen_size,
            tile_index,
            tile_count,
            tile_size,
            origin,
            scale,
            index: Default::default(),
        }
    }

    /// Get iterator over tile indices surrounding the viewport.
    pub fn border_tiles(&self) -> impl Iterator<Item = TileIndex> {
        let min_x = self.tile_index.x.saturating_sub(1);
        let max_x = self.tile_index.x + self.max_tiles_x;

        let min_y = self.tile_index.y.saturating_sub(1);
        let max_y = self.tile_index.y + self.max_tiles_y;

        let x_range = min_x..(max_x + 1).min(self.tile_count);
        let y_range = min_y..(max_y + 1).min(self.tile_count);

        // Use empty ranges to skip rows/columns outside the tileset.
        let top_range = if self.tile_index.x > 0 { x_range.clone() } else { 0..0 };
        let bottom_range = if max_y < self.tile_count { x_range } else { 0..0 };
        let left_range = if self.tile_index.y > 0 { y_range.clone() } else { 0..0 };
        let right_range = if max_x < self.tile_count { y_range } else { 0..0 };

        (top_range.zip(iter::repeat(min_y)))
            .chain(bottom_range.zip(iter::repeat(max_y)))
            .chain(iter::repeat(min_x).zip(left_range))
            .chain(iter::repeat(max_x).zip(right_range))
            .map(|(x, y)| TileIndex::new(x, y, self.tile_index.z))
    }

    /// Get physical position of a map point on the screen.
    ///
    /// The supplied `tile_index` must have a `z` coordinate matching the
    /// iterator's `z` coordinate.
    pub fn screen_point(&self, tile_index: TileIndex, offset: Point) -> Option<Point> {
        let point = self.tile_point(tile_index, offset);

        // Check whether point is visible.
        if point.x < 0
            || point.y < 0
            || point.x >= self.screen_size.width as i32
            || point.y >= self.screen_size.height as i32
        {
            None
        } else {
            Some(point)
        }
    }

    /// Get physical position of a map point in screen coordinates.
    ///
    /// The supplied `tile_index` must have a `z` coordinate matching the
    /// iterator's `z` coordinate.
    pub fn tile_point(&self, tile_index: TileIndex, offset: Point) -> Point {
        debug_assert_eq!(tile_index.z, self.tile_index.z);

        let x_delta = tile_index.x as i32 - self.tile_index.x as i32;
        let y_delta = tile_index.y as i32 - self.tile_index.y as i32;

        // Apply fractional scale to tile offset.
        let mut point = self.origin + offset * self.scale;

        point.x += x_delta * self.tile_size;
        point.y += y_delta * self.tile_size;

        point
    }

    /// Target width and height of the tile.
    pub fn tile_size(&self) -> i32 {
        self.tile_size
    }
}

impl Iterator for TileIter {
    type Item = (TileIndex, Point);

    /// Get the next tile and its screen position.
    fn next(&mut self) -> Option<Self::Item> {
        let x_delta = self.index % self.max_tiles_x;
        let y_delta = self.index / self.max_tiles_x;

        // Stop if there's no more tiles available.
        if y_delta >= self.max_tiles_y {
            return None;
        }

        self.index += 1;

        let tile_x = self.tile_index.x + x_delta;
        let tile_y = self.tile_index.y + y_delta;
        let index = TileIndex::new(tile_x, tile_y, self.tile_index.z);

        let x = self.origin.x + x_delta as i32 * self.tile_size;
        let y = self.origin.y + y_delta as i32 * self.tile_size;
        let point = Point::new(x, y);

        Some((index, point))
    }
}

/// A raster map tile.
pub struct Tile {
    index: TileIndex,
    image: PendingImage,

    download_state: DownloadState,
}

impl Drop for Tile {
    fn drop(&mut self) {
        // Abort loading if the tile leaves the cache before finishing.
        if let PendingImage::Loading(Some(task)) = &self.image {
            task.abort();
        }
    }
}

impl Tile {
    /// Load a new tile.
    #[cfg_attr(feature = "profiling", profiling::function)]
    fn new(download_state: DownloadState, index: TileIndex) -> Self {
        // Spawn background task to load image from cache or network.
        let task_download_state = download_state.clone();
        let load_task = tokio::spawn(async move {
            // Try to load the tile from the filesystem DB.
            match task_download_state.fs_cache.get(index).await {
                Ok(Some(db_tile)) => {
                    // If image is outdated, download it in the background.
                    // We still return the outdated image to improve performance.
                    if db_tile.age_secs > MAX_FS_CACHE_TIME {
                        let task_download_state = task_download_state.clone();
                        tokio::spawn(Self::download(task_download_state, index));
                    }

                    // Notify renderer about new map load completion.
                    let _ = task_download_state.tile_tx.send(index);

                    return Ok(db_tile.image);
                },
                Ok(None) => (),
                Err(err) => error!("Failed to load tile {index:?} from cache: {err}"),
            }

            // If it's not in the filesystem cache, start a new download.
            Self::download(task_download_state, index).await
        });
        let image = PendingImage::Loading(Some(load_task));

        Self { download_state, index, image }
    }

    /// Get the tile's image.
    #[cfg_attr(feature = "profiling", profiling::function)]
    pub fn image(&mut self) -> Option<&Image> {
        // Process asynchronous loads once finished.
        if let PendingImage::Loading(task) = &mut self.image
            && task.as_ref().is_some_and(|task| task.is_finished())
        {
            let image =
                task::block_in_place(|| RuntimeHandle::current().block_on(task.take().unwrap()));

            match image {
                Ok(Ok(image)) => self.image = PendingImage::Done(image),
                // Handle errors for download failures, DB errors are never propagated.
                Ok(Err(err)) => {
                    error!("Image download failed: {err}");

                    // Retry download with a delay on failure.
                    let download_state = self.download_state.clone();
                    let index = self.index;
                    let download_task = tokio::spawn(async move {
                        time::sleep(FAILED_DOWNLOAD_DELAY).await;
                        Self::download(download_state, index).await
                    });
                    self.image = PendingImage::Loading(Some(download_task));
                },
                Err(err) => error!("Failed to join image future: {err}"),
            }
        }

        match &self.image {
            PendingImage::Done(image) => Some(image),
            _ => None,
        }
    }

    /// Load a new tile from the tileserver.
    async fn download(state: DownloadState, index: TileIndex) -> Result<Image, Error> {
        // Get image from tileserver.
        let url = state
            .server
            .replace("{x}", &index.x.to_string())
            .replace("{y}", &index.y.to_string())
            .replace("{z}", &index.z.to_string());
        let response = state.client.get(&url).send().await?.error_for_status()?;
        let data = response.bytes().await?;

        // Add tile to filesystem cache.
        state.fs_cache.insert(index, &data).await?;

        // Try to decode bytes as image.
        let image =
            Image::from_encoded(Data::new_copy(&data)).ok_or_else(|| Error::InvalidImage(url))?;

        // Notify renderer about new map download completion.
        let _ = state.tile_tx.send(index);

        Ok(image)
    }
}

/// Index uniquely identifying a map tile.
#[derive(Default, Hash, PartialEq, Eq, Copy, Clone, Debug)]
pub struct TileIndex {
    pub x: u32,
    pub y: u32,
    pub z: u8,
}

impl TileIndex {
    pub fn new(x: u32, y: u32, z: u8) -> Self {
        Self { x, y, z }
    }
}

/// Asynchronous image download state.
enum PendingImage {
    Loading(Option<JoinHandle<Result<Image, Error>>>),
    Done(Image),
}

/// An LRU cache for tiles.
#[derive(Default)]
struct LruCache {
    tiles: HashMap<TileIndex, Tile>,
    lru: LinkedList<TileIndex>,
    capacity: usize,
}

impl LruCache {
    fn new(capacity: usize) -> Self {
        Self { capacity, tiles: Default::default(), lru: Default::default() }
    }

    /// Add a new tile to the cache.
    fn insert(&mut self, tile: Tile) {
        let index = tile.index;
        if self.tiles.contains_key(&index) {
            // Remove old LRU entry if tile already exists.
            self.lru.extract_if(|cached| *cached == index).take(1).for_each(drop);
        } else {
            // Remove oldest entry if cache is full.
            while self.tiles.len() >= self.capacity {
                let lru = self.lru.pop_back().unwrap();
                self.tiles.remove(&lru);
            }

            // Add tile to the cache.
            self.tiles.insert(index, tile);
        }

        // Mark item as the least-recently used.
        self.lru.push_front(index);
    }

    /// Check if a tile exists in the cache.
    fn has_tile(&self, index: &TileIndex) -> bool {
        self.tiles.contains_key(index)
    }

    /// Load a tile from the cache.
    fn get(&mut self, index: &TileIndex) -> Option<&mut Tile> {
        self.tiles.get_mut(index)
    }

    /// Clear all cached tiles.
    fn clear(&mut self) {
        self.tiles.clear();
        self.lru.clear();
    }
}

/// A filesystem cach for tiles.
pub struct FsCache {
    db: Db,
    last_cleanup: Arc<AtomicU16>,
    tileserver: Arc<String>,
    capacity: u32,
}

impl FsCache {
    #[cfg_attr(feature = "profiling", profiling::function)]
    fn new(config: &Config, db: Db) -> Self {
        Self {
            db,
            last_cleanup: Arc::new(AtomicU16::new(0)),
            tileserver: config.tiles.server.clone(),
            capacity: config.tiles.max_fs_tiles,
        }
    }

    /// Close the SQLite database connection.
    pub async fn close(&self) {
        let pool = self.db.pool().await;

        // Defragment and truncate database file.
        //
        // This takes a while ~1s and blocks other database operations, so we just do it
        // on exit.
        if let Err(err) = sqlx::query("VACUUM").execute(pool).await {
            error!("SQLite vacuum failed: {err}");
        }

        pool.close().await;
    }

    /// Add a new tile to the cache.
    async fn insert(&self, index: TileIndex, data: &[u8]) -> Result<(), Error> {
        #[rustfmt::skip]
        sqlx::query(
            "INSERT INTO tile (tileserver, x, y, z, data) \
                VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT DO UPDATE \
                SET data = excluded.data, ctime = unixepoch(), atime = unixepoch()",
        )
        .bind(&*self.tileserver)
        .bind(index.x)
        .bind(index.y)
        .bind(index.z)
        .bind(data)
        .execute(self.db.pool().await)
        .await?;

        // Cleanup cache every `FS_CACHE_CLEANUP_INTERVAL` inserts.
        if self.last_cleanup.fetch_add(1, Ordering::Relaxed) >= FS_CACHE_CLEANUP_INTERVAL {
            self.last_cleanup.store(0, Ordering::Relaxed);

            // Report cache errors immediately, to avoid failing the download.
            if let Err(err) = self.clean_cache().await {
                error!("Failed tile DB cleanup: {err}");
            }
        }

        Ok(())
    }

    /// Read a tile from the cache.
    async fn get(&self, index: TileIndex) -> Result<Option<DbTile>, Error> {
        #[rustfmt::skip]
        let data = sqlx::query_as(
            "UPDATE tile SET atime = unixepoch() \
                WHERE tileserver = $1 AND x = $2 AND y = $3 and z = $4 \
             RETURNING unixepoch() - ctime as age_secs, data",
        )
        .bind(&*self.tileserver)
        .bind(index.x)
        .bind(index.y)
        .bind(index.z)
        .fetch_optional(self.db.pool().await)
        .await?;
        Ok(data)
    }

    /// Perform filesystem cache cleanup.
    async fn clean_cache(&self) -> Result<(), Error> {
        let pool = self.db.pool().await;

        // Delete least recently used tiles beyond the tile capacity.
        sqlx::query(
            "DELETE FROM tile WHERE id NOT IN (SELECT id FROM tile ORDER BY atime DESC LIMIT $1)",
        )
        .bind(self.capacity)
        .execute(pool)
        .await?;

        Ok(())
    }

    /// Update the tileserver URL.
    fn set_tileserver(&mut self, tileserver: Arc<String>) {
        self.tileserver = tileserver;
    }
}

impl Clone for FsCache {
    fn clone(&self) -> Self {
        let last_cleanup = self.last_cleanup.load(Ordering::Relaxed);

        Self {
            last_cleanup: Arc::new(AtomicU16::new(last_cleanup)),
            tileserver: self.tileserver.clone(),
            capacity: self.capacity,
            db: self.db.clone(),
        }
    }
}

/// Tile data retrieved from the database.
struct DbTile {
    age_secs: u64,
    image: Image,
}

impl FromRow<'_, SqliteRow> for DbTile {
    fn from_row(row: &SqliteRow) -> Result<Self, sqlx::Error> {
        let data: Vec<u8> = row.try_get("data")?;
        let age_secs = row.try_get("age_secs")?;

        let image = Image::from_encoded(Data::new_copy(&data))
            .ok_or_else(|| sqlx::Error::Decode("Invalid cached tile {index:?}".into()))?;

        Ok(Self { age_secs, image })
    }
}

/// State used in the download future.
///
/// Since this state is shared between all download futures, it **must** be
/// cheap to clone.
#[derive(Clone)]
struct DownloadState {
    tile_tx: Sender<TileIndex>,
    server: Arc<String>,
    fs_cache: FsCache,
    client: Client,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_tile_iter() {
        let size = Size::new(TILE_SIZE as u32, TILE_SIZE as u32);
        let index = TileIndex::new(8504, 5473, 14);
        let offset = Point::new(128, 128);

        let mut iter = TileIter::new(size, index, offset, 1.);

        let (iter_index, point) = iter.next().unwrap();
        assert_eq!(iter_index, index);
        assert_eq!(point, Point::new(0, 0));

        assert_eq!(iter.next(), None);
    }

    #[test]
    fn partial_tiles_iter() {
        let size = Size::new(300, 300);
        let index = TileIndex::new(8504, 5473, 14);
        let offset = Point::new(128, 128);

        let mut iter = TileIter::new(size, index, offset, 1.);

        for i in 0..9 {
            let tile_x = (8503 + i % 3) as u32;
            let tile_y = (5472 + i / 3) as u32;

            let x = -234 + i % 3 * TILE_SIZE;
            let y = -234 + i / 3 * TILE_SIZE;

            let (iter_index, point) = iter.next().unwrap();
            assert_eq!(iter_index, TileIndex::new(tile_x, tile_y, index.z));
            assert_eq!(point, Point::new(x, y));
        }
    }

    #[test]
    fn map_border_iter() {
        let size = Size::new(300, 300);
        let index = TileIndex::new(0, 0, 0);
        let offset = Point::new(128, 128);

        let mut iter = TileIter::new(size, index, offset, 1.);

        let (iter_index, point) = iter.next().unwrap();
        assert_eq!(iter_index, index);
        assert_eq!(point, Point::new(22, 22));

        assert_eq!(iter.next(), None);
    }

    #[test]
    fn surrounding_tiles() {
        let size = Size::new(TILE_SIZE as u32, TILE_SIZE as u32);
        let index = TileIndex::new(1, 1, 14);
        let offset = Point::new(128, 128);

        let iter = TileIter::new(size, index, offset, 1.);
        let mut border_tiles = iter.border_tiles();

        // Top row.
        assert_eq!(border_tiles.next().unwrap(), TileIndex::new(0, 0, 14));
        assert_eq!(border_tiles.next().unwrap(), TileIndex::new(1, 0, 14));
        assert_eq!(border_tiles.next().unwrap(), TileIndex::new(2, 0, 14));

        // Bottom row.
        assert_eq!(border_tiles.next().unwrap(), TileIndex::new(0, 2, 14));
        assert_eq!(border_tiles.next().unwrap(), TileIndex::new(1, 2, 14));
        assert_eq!(border_tiles.next().unwrap(), TileIndex::new(2, 2, 14));

        // Left column.
        assert_eq!(border_tiles.next().unwrap(), TileIndex::new(0, 0, 14));
        assert_eq!(border_tiles.next().unwrap(), TileIndex::new(0, 1, 14));
        assert_eq!(border_tiles.next().unwrap(), TileIndex::new(0, 2, 14));

        // Right column.
        assert_eq!(border_tiles.next().unwrap(), TileIndex::new(2, 0, 14));
        assert_eq!(border_tiles.next().unwrap(), TileIndex::new(2, 1, 14));
        assert_eq!(border_tiles.next().unwrap(), TileIndex::new(2, 2, 14));
    }
}
