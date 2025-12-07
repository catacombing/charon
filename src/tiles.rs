//! Map tile handling.

use std::collections::{HashMap, LinkedList};
use std::fs::File;
use std::io::ErrorKind as IoErrorKind;
use std::iter;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

use calloop::channel::Sender;
use memmap2::Mmap;
use reqwest::Client;
use skia_safe::{Data, Image};
use tempfile::NamedTempFile;
use tokio::runtime::Handle as RuntimeHandle;
use tokio::task::{self, JoinHandle};
use tokio::{fs as tokio_fs, time};
use tracing::{debug, error};

use crate::Error;
use crate::config::Config;
use crate::geometry::{Point, Size};

/// Width and height of a single tile.
pub const TILE_SIZE: i32 = 256;

/// Maximum tile zoom level.
pub const MAX_ZOOM: u8 = 19;

/// How frequently old filesystem entries are vacated from the cache.
///
/// The total number of memory used by the filesystem cache will always be
/// between `MAX_FS_CACHED_TILES` and `MAX_FS_CACHED_TILES` +
/// `FS_CACHE_CLEANUP_INTERVAL`.
///
/// A higher cleanup interval means less frequent filesystem traversal to find
/// tiles which should be vacated.
const FS_CACHE_CLEANUP_INTERVAL: u16 = 500;

/// Maximum age for tiles cached on the filesystem.
const MAX_FS_CACHE_TIME: Duration = Duration::from_secs(60 * 60 * 24 * 7);

/// Time before a failed download will be re-attempted.
const FAILED_DOWNLOAD_DELAY: Duration = Duration::from_secs(3);

/// Map tile cache.
///
/// This manages the local cache for all rendered tiles and can either
/// automatically or manually download new tiles and store them.
pub struct Tiles {
    download_state: DownloadState,
    lru_cache: LruCache,
    tile_dir: PathBuf,
}

impl Tiles {
    pub fn new(client: Client, tile_tx: Sender<TileIndex>, config: &Config) -> Result<Self, Error> {
        // Initialize filesystem cache and remove outdated maps.
        let tile_dir = dirs::cache_dir().ok_or(Error::MissingCacheDir)?.join("charon/tiles");
        let server_tile_dir = Self::server_tile_dir(&tile_dir, config);
        let fs_cache = Arc::new(FsCache::new(server_tile_dir, config.tiles.max_fs_tiles));
        let cleanup_cache = fs_cache.clone();
        tokio::spawn(async move { cleanup_cache.clean_cache().await });

        let download_state =
            DownloadState { fs_cache, tile_tx, client, server: config.tiles.server.clone() };

        Ok(Self { download_state, tile_dir, lru_cache: LruCache::new(config.tiles.max_mem_tiles) })
    }

    /// Get a raster map tile.
    #[cfg_attr(feature = "profiling", profiling::function)]
    pub fn get(&mut self, index: TileIndex) -> &mut Tile {
        if !self.lru_cache.has_tile(&index) {
            let download_state = self.download_state.clone();
            self.lru_cache.insert(Tile::new(download_state, index));
        }

        self.lru_cache.get(&index).unwrap()
    }

    /// Ensure a map tile is downloaded.
    #[cfg_attr(feature = "profiling", profiling::function)]
    pub fn preload(&mut self, index: TileIndex) {
        // Ignore tile if it is already cached in memory or FS.
        if self.lru_cache.has_tile(&index) || self.download_state.fs_cache.has_tile(index) {
            return;
        }

        let download_state = self.download_state.clone();
        self.lru_cache.insert(Tile::new(download_state, index));
    }

    /// Handle config updates.
    pub fn update_config(&mut self, config: &Config) -> bool {
        let mut dirty = false;

        if self.download_state.server != config.tiles.server {
            let server_tile_dir = Self::server_tile_dir(&self.tile_dir, config);
            Arc::make_mut(&mut self.download_state.fs_cache).dir = server_tile_dir;

            self.download_state.server = config.tiles.server.clone();
            self.lru_cache.clear();
            dirty = true;
        }
        if self.lru_cache.capacity != config.tiles.max_mem_tiles {
            self.lru_cache.capacity = config.tiles.max_mem_tiles;
        }
        if self.download_state.fs_cache.capacity != config.tiles.max_fs_tiles {
            let fs_cache = Arc::make_mut(&mut self.download_state.fs_cache);
            fs_cache.capacity = config.tiles.max_fs_tiles;
        }

        dirty
    }

    /// Get tileserver-specific tile dir from the general-purpose tile dir path.
    fn server_tile_dir(tile_dir: &Path, config: &Config) -> PathBuf {
        tile_dir.join(config.tiles.server.replace('/', "_"))
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

    index: u32,
}

impl TileIter {
    pub fn new(size: Size, mut tile_index: TileIndex, offset: Point, scale: f64) -> Self {
        // Get position of the tile's top-left on the screen.
        let x_offset = (offset.x as f64 * scale).round() as i32;
        let y_offset = (offset.y as f64 * scale).round() as i32;
        let x_origin = size.width as i32 / 2 - x_offset;
        let y_origin = size.height as i32 / 2 - y_offset;

        // Get top-left tile's indices and offset.
        let tile_size = (TILE_SIZE as f64 * scale).round() as i32;
        let x_delta = ((x_origin + tile_size - 1) / tile_size).min(tile_index.x as i32);
        tile_index.x -= x_delta as u32;
        let y_delta = ((y_origin + tile_size - 1) / tile_size).min(tile_index.y as i32);
        tile_index.y -= y_delta as u32;
        let origin = Point::new(x_origin - x_delta * tile_size, y_origin - y_delta * tile_size);

        // Calculate maximum tile indices.

        let tile_count = 1 << tile_index.z as u32;

        let available_x = size.width as i32 - origin.x;
        let tiles_x = ((available_x + tile_size - 1) / tile_size) as u32;
        let max_tiles_x = tiles_x.min(tile_count - tile_index.x);

        let available_y = size.height as i32 - origin.y;
        let tiles_y = ((available_y + tile_size - 1) / tile_size) as u32;
        let max_tiles_y = tiles_y.min(tile_count - tile_index.y);

        Self {
            max_tiles_x,
            max_tiles_y,
            tile_index,
            tile_count,
            tile_size,
            origin,
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
        // Abort download if the tile leaves the cache before finishing.
        if let PendingImage::Downloading(Some(task)) = &self.image {
            task.abort();
        }
    }
}

impl Tile {
    /// Load a new tile.
    #[cfg_attr(feature = "profiling", profiling::function)]
    fn new(download_state: DownloadState, index: TileIndex) -> Self {
        // Try to load file from filesystem cache first.
        match download_state.fs_cache.get(index) {
            Ok(Some(data)) => match Image::from_encoded(Data::new_copy(&data)) {
                Some(image) => {
                    return Self { download_state, index, image: PendingImage::Done(image) };
                },
                None => {
                    let path = download_state.fs_cache.tile_path(index);
                    error!("Invalid cached tile: {path:?}");
                },
            },
            Ok(None) => (),
            Err(err) => error!("Failed to load tile {index:?} from cache: {err}"),
        }

        // If it's not in the filesystem cache, start a new download.
        let task_download_state = download_state.clone();
        let download_task = tokio::spawn(Self::download(task_download_state, index));
        let image = PendingImage::Downloading(Some(download_task));

        Self { download_state, index, image }
    }

    /// Get the tile's image.
    #[cfg_attr(feature = "profiling", profiling::function)]
    pub fn image(&mut self) -> Option<&Image> {
        // Process asynchronous downloads once finished.
        if let PendingImage::Downloading(task) = &mut self.image
            && task.as_ref().is_some_and(|task| task.is_finished())
        {
            let image =
                task::block_in_place(|| RuntimeHandle::current().block_on(task.take().unwrap()));

            match image {
                Ok(Ok(image)) => self.image = PendingImage::Done(image),
                Ok(Err(err)) => {
                    error!("Image download failed: {err}");

                    // Retry download with a delay on failure.
                    let download_state = self.download_state.clone();
                    let index = self.index;
                    let download_task = tokio::spawn(async move {
                        time::sleep(FAILED_DOWNLOAD_DELAY).await;
                        Self::download(download_state, index).await
                    });
                    self.image = PendingImage::Downloading(Some(download_task));
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
        let uri = state
            .server
            .replace("{x}", &index.x.to_string())
            .replace("{y}", &index.y.to_string())
            .replace("{z}", &index.z.to_string());
        let response = state.client.get(&uri).send().await?.error_for_status()?;
        let data = response.bytes().await?;

        // Add tile to filesystem cache.
        state.fs_cache.insert(index, &data).await?;

        // Try to decode bytes as image.
        let image =
            Image::from_encoded(Data::new_copy(&data)).ok_or_else(|| Error::InvalidImage(uri))?;

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
    Downloading(Option<JoinHandle<Result<Image, Error>>>),
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
#[derive(Default)]
struct FsCache {
    dir: PathBuf,
    capacity: usize,
    last_cleanup: AtomicU16,
}

impl FsCache {
    fn new(dir: PathBuf, capacity: usize) -> Self {
        Self { last_cleanup: AtomicU16::new(0), capacity, dir }
    }

    /// Add a new tile to the cache.
    async fn insert(&self, index: TileIndex, data: &[u8]) -> Result<(), Error> {
        let path = self.tile_path(index);

        // Atomically write image data to the file.
        tokio_fs::create_dir_all(&self.dir).await?;
        let file = NamedTempFile::new_in(&self.dir)?;
        tokio_fs::write(&file, data).await?;
        file.persist(path)?;

        // Cleanup cache every `FS_CACHE_CLEANUP_INTERVAL` inserts.
        if self.last_cleanup.fetch_add(1, Ordering::Relaxed) >= FS_CACHE_CLEANUP_INTERVAL {
            self.last_cleanup.store(0, Ordering::Relaxed);

            // Report cache errors immediately, to avoid failing the download.
            self.clean_cache().await;
        }

        Ok(())
    }

    /// Read a tile from the cache.
    #[cfg_attr(feature = "profiling", profiling::function)]
    fn get(&self, index: TileIndex) -> Result<Option<Mmap>, Error> {
        let path = self.tile_path(index);

        // Try to open the file, returning `None` if the tile is not cached.
        let file = match File::open(path) {
            Ok(file) => file,
            Err(err) if err.kind() == IoErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err.into()),
        };

        let data = unsafe { Mmap::map(&file)? };

        Ok(Some(data))
    }

    /// Check if a tile exists in the FS cache.
    fn has_tile(&self, index: TileIndex) -> bool {
        let path = self.tile_path(index);
        path.exists()
    }

    /// Perform filesystem cache cleanup.
    async fn clean_cache(&self) {
        let mut read_dir = match tokio_fs::read_dir(&self.dir).await {
            Ok(read_dir) => read_dir,
            Err(err) if err.kind() == IoErrorKind::NotFound => return,
            Err(err) => {
                error!("Cache directory access failed: {err}");
                return;
            },
        };

        // Get all stored tiles, ordered by creation time.
        let mut cached_files = Vec::new();
        while let Ok(Some(entry)) = read_dir.next_entry().await {
            if let Ok(metadata) = entry.metadata().await
                && let Ok(created) = metadata.created()
                && let Ok(elapsed) = created.elapsed()
            {
                cached_files.push((entry.path(), elapsed));
            }
        }
        cached_files.sort_unstable_by_key(|(_, time)| *time);

        // Delete all files beyond capacity or `MAX_FS_CACHE_TIME`.
        for (i, (path, created)) in cached_files.into_iter().enumerate().rev() {
            if i >= self.capacity || created > MAX_FS_CACHE_TIME {
                let _ = tokio_fs::remove_file(&path).await;
                debug!("Removed {path:?} from cache");
            } else {
                break;
            }
        }
    }

    /// Get the cache path for a tile.
    fn tile_path(&self, index: TileIndex) -> PathBuf {
        let file_name = format!("{}_{}_{}.png", index.z, index.x, index.y);
        self.dir.join(&file_name)
    }
}

impl Clone for FsCache {
    fn clone(&self) -> Self {
        Self {
            last_cleanup: AtomicU16::new(self.last_cleanup.load(Ordering::Relaxed)),
            capacity: self.capacity,
            dir: self.dir.clone(),
        }
    }
}

/// State used in the download future.
///
/// Since this state is shared between all download futures, it **must** be
/// cheap to clone.
#[derive(Clone)]
struct DownloadState {
    tile_tx: Sender<TileIndex>,
    fs_cache: Arc<FsCache>,
    server: Arc<String>,
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
