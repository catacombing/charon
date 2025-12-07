//! Geographic region management.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use bzip2::write::BzDecoder;
use calloop::LoopHandle;
use calloop::ping::{self, Ping};
use indexmap::IndexMap;
use reqwest::Client;
use reqwest::header::CONTENT_LENGTH;
use serde::Deserialize;
use tempfile::NamedTempFile;
use tokio::task::JoinSet;
use tracing::{debug, error, warn};

use crate::ui::view::View;
use crate::ui::view::download::DownloadView;
use crate::{Error, State};

/// Pre-parsed region data.
const REGIONS: &str = include_str!(concat!(env!("OUT_DIR"), "/regions.json"));

/// Required geocoder files for each region.
const GEOCODER_FILES: &[&str] =
    &["geonlp-normalized-id.kch", "geonlp-normalized.trie", "geonlp-primary.sqlite"];
/// Required postal files for each language.
const POSTAL_COUNTRY_FILES: &[&str] = &[
    "address_parser_postal_codes.dat",
    "address_parser_phrases.dat",
    "address_parser_vocab.trie",
    "address_parser_crf.dat",
];
/// Required global postal files.
const POSTAL_GLOBAL_FILES: &[&str] = &[
    "language_classifier/language_classifier.dat",
    "address_expansions/address_dictionary.dat",
    "transliteration/transliteration.dat",
    "numex/numex.dat",
];

/// Region data management.
pub struct Regions {
    data: RegionData,

    geocoder_cache_dir: PathBuf,
    postal_cache_dir: PathBuf,

    ui_waker: Ping,
    client: Client,
}

impl Regions {
    #[cfg_attr(feature = "profiling", profiling::function)]
    pub fn new(event_loop: LoopHandle<'static, State>, client: Client) -> Result<Self, Error> {
        // Deserialize region data generated at compile time.
        let data = RegionData::new()?;

        // Get cache storage locations.
        let cache_dir = dirs::cache_dir().ok_or(Error::MissingCacheDir)?.join("charon");
        let geocoder_cache_dir = cache_dir.join("geocoder");
        let postal_cache_dir = cache_dir.join("postal");

        // Register ping source to allow waking up UI on async region state changes.
        let (ui_waker, source) = ping::make_ping()?;
        event_loop.insert_source(source, |_, _, state| {
            state.window.views.get_mut::<DownloadView>(View::Download).unwrap().set_dirty();
            state.window.unstall();
        })?;

        let regions = Self { geocoder_cache_dir, postal_cache_dir, ui_waker, client, data };

        // Update region's download state from FS.
        regions.refresh_download_state();

        Ok(regions)
    }

    /// Get the root region.
    pub fn world(&self) -> &Region {
        &self.data.world_region
    }

    /// Download a region's data to the local cache.
    pub async fn download(&self, region: &Region) -> Result<(), Error> {
        let mut downloads: JoinSet<Result<_, Error>> = JoinSet::new();
        let tracker = region.download_tracker();
        tracker.reset();

        // Download geocoder files.
        if let Some((geocoder_path, region_name)) = region.geocoder_uri_path() {
            for file in GEOCODER_FILES {
                let path = self.geocoder_cache_dir.join(region_name).join(file);
                if path.exists() {
                    warn!("Invalid download for {path:?}: file exists");
                    continue;
                }

                let url = format!("{}/{geocoder_path}/{file}.bz2", self.data.geocoder_base);
                let client = self.client.clone();
                let tracker = tracker.clone();
                downloads
                    .spawn(async move { Self::download_bz2(client, tracker, &url, &path).await });
            }
        }

        if let Some((postal_path, country_code)) = region.postal_uri_path() {
            // Download postal country files.
            for file in POSTAL_COUNTRY_FILES {
                let path =
                    Region::postal_country_fs_path(&self.postal_cache_dir, country_code).join(file);
                if path.exists() {
                    debug!("skipping existing postal language: {country_code}");
                    continue;
                }

                let url = format!(
                    "{}/{postal_path}/address_parser/{file}.bz2",
                    self.data.postal_country_base
                );
                let client = self.client.clone();
                let tracker = tracker.clone();
                downloads
                    .spawn(async move { Self::download_bz2(client, tracker, &url, &path).await });
            }

            // Download postal global files.
            for file in POSTAL_GLOBAL_FILES {
                let path = self.postal_global_path().join(file);
                if path.exists() {
                    debug!("skipping existing global postal data");
                    continue;
                }

                let url = format!("{}/{file}.bz2", self.data.postal_global_base);
                let client = self.client.clone();
                let tracker = tracker.clone();
                downloads
                    .spawn(async move { Self::download_bz2(client, tracker, &url, &path).await });
            }
        }

        // Wait for all downloads to complete.
        //
        // Since we're nuking all existing data on any failure anyway, there's no reason
        // to let other downloads finish if any has failed.
        while let Some(result) = downloads.join_next().await {
            result??;
        }

        Ok(())
    }

    /// Delete a region's data from the local cache.
    ///
    /// This never removes the global postal data, since it's required to make
    /// search work with any region.
    #[cfg_attr(feature = "profiling", profiling::function)]
    pub fn delete(&self, region: &Region) {
        // Delete geocoder data.
        if let Some((_, region_name)) = region.geocoder_uri_path() {
            let path = self.geocoder_cache_dir.join(region_name);
            if let Err(err) = fs::remove_dir_all(&path) {
                error!("Failed to delete {path:?}: {err}");
            }
        }

        // Delet postal country files, if they're not required by another region.
        if let Some((postal_path, country_code)) = region.postal_uri_path()
            && !self.world().requires_postal_country(postal_path, &region.name)
        {
            let path = Region::postal_country_fs_root(&self.postal_cache_dir, country_code);
            if let Err(err) = fs::remove_dir_all(&path) {
                error!("Failed to delete {path:?}: {err}");
            }
        }
    }

    /// Recursively update download status based on current filesystem state.
    #[cfg_attr(feature = "profiling", profiling::function)]
    pub fn refresh_download_state(&self) {
        // Check if global postal files are installed.
        let postal_global_installed =
            POSTAL_GLOBAL_FILES.iter().all(|file| self.postal_global_path().join(file).exists());

        self.world().refresh_download_state(
            &self.geocoder_cache_dir,
            &self.postal_cache_dir,
            postal_global_installed,
        );

        // Ensure UI is updated on download state changes.
        self.ui_waker.ping();
    }

    /// Get the postal global file storage path.
    pub fn postal_global_path(&self) -> PathBuf {
        self.postal_cache_dir.join("global")
    }

    /// Get the root of postal's country storage for region.
    pub fn postal_country_root(&self, region: &Region) -> Option<PathBuf> {
        let (_, country_code) = region.postal_uri_path()?;
        Some(Region::postal_country_fs_root(&self.postal_cache_dir, country_code))
    }

    /// Get the geocoder file storage path for a region.
    pub fn geocoder_path(&self, region: &Region) -> Option<PathBuf> {
        let (_, region_name) = region.geocoder_uri_path()?;
        Some(Region::geocoder_fs_path(&self.geocoder_cache_dir, region_name))
    }

    /// Download a .bz2 file from `url` and write it uncompressed to `file`.
    async fn download_bz2(
        client: Client,
        tracker: DownloadTracker,
        url: &str,
        path: &Path,
    ) -> Result<(), Error> {
        // Send download request.
        let mut response = client.get(url).send().await?.error_for_status()?;

        // Create a streaming decoder into a tempfile.
        let parent = path.parent().ok_or(Error::UnexpectedRoot)?;
        tokio::fs::create_dir_all(&parent).await?;
        let mut file = NamedTempFile::new_in(parent)?;
        let mut decoder = BzDecoder::new(&mut file);

        let content_length = response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|h| h.to_str().ok()?.parse().ok())
            .unwrap_or(0);
        tracker.add_download(content_length);

        // Stream data through the decoder into the tempfile.
        while let Some(chunk) = response.chunk().await? {
            tracker.add_progress(chunk.len() as u64);
            decoder.write_all(&chunk)?;
        }
        decoder.finish()?;
        drop(decoder);

        // Atomically place tempfile into target location.
        file.persist(path)?;

        Ok(())
    }
}

/// Region data generated at compile time.
#[derive(Deserialize)]
struct RegionData {
    world_region: Region,

    postal_country_base: String,
    postal_global_base: String,
    geocoder_base: String,
}

impl RegionData {
    fn new() -> Result<Self, Error> {
        Ok(serde_json::from_str(REGIONS)?)
    }
}

/// Data for a geographic region.
#[derive(Deserialize, Debug)]
pub struct Region {
    pub name: String,
    pub regions: IndexMap<String, Region>,
    // Complete size of this region and all of its children.
    pub storage_size: u64,

    geocoder_path: Option<String>,
    postal_path: Option<String>,

    #[serde(skip)]
    download_state: AtomicU8,
    #[serde(skip)]
    download_pending: Arc<AtomicU64>,
    #[serde(skip)]
    download_done: Arc<AtomicU64>,
}

impl Region {
    /// Get region's data download state.
    pub fn download_state(&self) -> DownloadState {
        self.download_state.load(Ordering::Relaxed).into()
    }

    /// Mark region as downloading.
    pub fn set_download_state(&self, download_state: DownloadState) {
        self.download_state.store(download_state as u8, Ordering::Relaxed);
    }

    /// Get current download progress.
    pub fn download_progress(&self) -> f64 {
        let pending = self.download_pending.load(Ordering::Relaxed);
        let done = self.download_done.load(Ordering::Relaxed);
        done as f64 / pending as f64
    }

    /// Get the current install size in bytes.
    pub fn current_install_size(&self) -> u64 {
        let mut size = 0;

        // Add size for the region itself.
        //
        // The size of a downloadable region never includes its children, so we can just
        // add it without worrying about counting things twice.
        if self.is_installed() {
            size += self.storage_size;
        }

        // Sum up installed child regions.
        for region in self.regions.values() {
            size += region.current_install_size();
        }

        size
    }

    /// Execute a function for all child regions.
    #[cfg_attr(feature = "profiling", profiling::function)]
    pub fn for_installed(&self, f: &mut impl FnMut(&Self)) {
        if self.is_installed() {
            f(self);
        }

        for region in self.regions.values() {
            region.for_installed(f)
        }
    }

    /// Recursively update download status based on current filesystem state.
    #[cfg_attr(feature = "profiling", profiling::function)]
    fn refresh_download_state(
        &self,
        geocoder_cache_dir: &Path,
        postal_cache_dir: &Path,
        postal_global_installed: bool,
    ) {
        // Update all subregions.
        for region in self.regions.values() {
            region.refresh_download_state(
                geocoder_cache_dir,
                postal_cache_dir,
                postal_global_installed,
            );
        }

        // Short-circuit if no data is available.
        if self.geocoder_path.is_none() && self.postal_path.is_none() {
            self.set_download_state(DownloadState::NoData);
            return;
        }

        // Always mark region as available if global postal data is missing.
        if !postal_global_installed {
            self.set_download_state(DownloadState::Available);
            return;
        }

        // Assume all data has been downloaded.
        let mut download_state = DownloadState::Downloaded;

        // Check if geocoder data needs to be downloaded.
        if let Some((_, region_name)) = self.geocoder_uri_path() {
            let geocoder_installed = GEOCODER_FILES
                .iter()
                .all(|file| geocoder_cache_dir.join(region_name).join(file).exists());
            if !geocoder_installed {
                download_state = DownloadState::Available;
            }
        }

        // Check if postal data needs to be downloaded,
        // unless geocoder is already missing.
        if let Some((_, country_code)) =
            self.postal_uri_path().filter(|_| download_state == DownloadState::Downloaded)
        {
            let postal_installed = POSTAL_COUNTRY_FILES.iter().all(|file| {
                Self::postal_country_fs_path(postal_cache_dir, country_code).join(file).exists()
            });
            if !postal_installed {
                download_state = DownloadState::Available;
            }
        }

        self.set_download_state(download_state);
    }

    /// Get region's download progress tracker.
    fn download_tracker(&self) -> DownloadTracker {
        DownloadTracker {
            download_pending: self.download_pending.clone(),
            download_done: self.download_done.clone(),
        }
    }

    /// Get postal URI path and country code.
    fn postal_uri_path(&self) -> Option<(&str, &str)> {
        let postal_path = self.postal_path.as_deref()?;

        // Extract country code from postal path.
        let separator_index = match postal_path.rfind('/') {
            Some(separator_index) => separator_index,
            None => {
                error!("Invalid postal path: {postal_path}");
                return None;
            },
        };
        let country_code = &postal_path[separator_index + 1..];

        Some((postal_path, country_code))
    }

    /// Get geocoder URI path and region name.
    fn geocoder_uri_path(&self) -> Option<(&str, &str)> {
        let geocoder_path = self.geocoder_path.as_deref()?;

        // Extract region from postal path.
        let separator_index = match geocoder_path.rfind('/') {
            Some(separator_index) => separator_index,
            None => {
                error!("Invalid geocoder path: {geocoder_path}");
                return None;
            },
        };
        let region = &geocoder_path[separator_index + 1..];

        Some((geocoder_path, region))
    }

    /// Check if a postal country dataset is required by this region or its
    /// children.
    ///
    /// The `filter` argument can be used to ignore a specific region, assuming
    /// that it never requires the specified postal country dataset.
    fn requires_postal_country(&self, postal_path: &str, filter: &str) -> bool {
        if self.name != filter
            && self.postal_path.as_deref() == Some(postal_path)
            && self.download_state() == DownloadState::Downloaded
        {
            return true;
        }

        self.regions.values().any(|region| region.requires_postal_country(postal_path, filter))
    }

    /// Check whether this region's data is installed.
    ///
    /// This should be slightly faster than comparing `Self::download_state`
    /// since it avoids enum conversion.
    fn is_installed(&self) -> bool {
        self.download_state.load(Ordering::Relaxed) == DownloadState::Downloaded as u8
    }

    /// Get the postal country file storage path for a country code.
    fn postal_country_fs_path(postal_cache_dir: &Path, country_code: &str) -> PathBuf {
        postal_cache_dir.join("countries").join(country_code).join("address_parser")
    }

    /// Get the root of postal's country storage for a country code.
    fn postal_country_fs_root(postal_cache_dir: &Path, country_code: &str) -> PathBuf {
        postal_cache_dir.join("countries").join(country_code)
    }

    /// Get the geocoder file storage path for a region.
    fn geocoder_fs_path(geocoder_cache_dir: &Path, region_name: &str) -> PathBuf {
        geocoder_cache_dir.join(region_name)
    }
}

/// Download state of a region's data.
#[derive(PartialEq, Eq, Copy, Clone, Debug)]
pub enum DownloadState {
    NoData,
    Available,
    Downloading,
    Downloaded,
}

impl From<u8> for DownloadState {
    fn from(int: u8) -> Self {
        match int {
            0 => Self::NoData,
            1 => Self::Available,
            2 => Self::Downloading,
            3 => Self::Downloaded,
            _ => Self::NoData,
        }
    }
}

/// Tracker for region data download.
#[derive(Clone)]
struct DownloadTracker {
    download_pending: Arc<AtomicU64>,
    download_done: Arc<AtomicU64>,
}

impl DownloadTracker {
    /// Clear all previously tracked progress.
    fn reset(&self) {
        self.download_pending.store(0, Ordering::Relaxed);
        self.download_done.store(0, Ordering::Relaxed);
    }

    /// Add a new download with no progress.
    fn add_download(&self, size: u64) {
        self.download_pending.fetch_add(size, Ordering::Relaxed);
    }

    /// Indicate a certain number of bytes have been downloaded.
    fn add_progress(&self, size: u64) {
        self.download_done.fetch_add(size, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modrana_regions() {
        const GEOCODER_KARLSRUHE: &str =
            "geocoder-nlp/europe-germany-baden-wuerttemberg-karlsruhe-regbez";
        const GEOCEDER_BADEN: &str = "geocoder-nlp/europe-germany-baden-wuerttemberg";
        const POSTAL_DE: &str = "postal/countries-v1/DE";

        let world = RegionData::new().unwrap().world_region;
        assert_eq!(world.name, "World");
        assert_eq!(world.geocoder_path, None);
        assert_eq!(world.storage_size, 32457661728);

        let europe = world.regions.get("europe").unwrap();
        assert_eq!(europe.name, "Europe");
        assert_eq!(europe.geocoder_path, None);
        assert_eq!(europe.storage_size, 14428637247);

        let germany = europe.regions.get("germany").unwrap();
        assert_eq!(germany.name, "Germany");
        assert_eq!(germany.geocoder_path, None);
        assert_eq!(germany.postal_path, None);
        assert_eq!(germany.storage_size, 2792185814);

        let baden = germany.regions.get("baden-wuerttemberg").unwrap();
        assert_eq!(baden.name, "Baden-Württemberg");
        assert_eq!(baden.geocoder_path, Some(GEOCEDER_BADEN.into()));
        assert_eq!(baden.postal_path, Some(POSTAL_DE.into()));
        assert_eq!(baden.storage_size, 590606909);

        let karlsruhe = baden.regions.get("karlsruhe-regbez").unwrap();
        assert_eq!(karlsruhe.name, "Regierungsbezirk Karlsruhe");
        assert_eq!(karlsruhe.geocoder_path, Some(GEOCODER_KARLSRUHE.into()));
        assert_eq!(karlsruhe.postal_path, Some(POSTAL_DE.into()));
        assert_eq!(karlsruhe.storage_size, 320152253);
    }

    #[test]
    fn base_urls() {
        const POSTAL_COUNTRY_BASE: &str =
            "https://data.modrana.org/osm_scout_server/postal-country-2";
        const POSTAL_GLOBAL_BASE: &str =
            "https://data.modrana.org/osm_scout_server/postal-global-2/postal/global-v1";
        const GEOCODER_BASE: &str = "https://data.modrana.org/osm_scout_server/geocoder-nlp-39";

        let data = RegionData::new().unwrap();
        assert_eq!(data.postal_country_base, POSTAL_COUNTRY_BASE,);
        assert_eq!(data.postal_global_base, POSTAL_GLOBAL_BASE);
        assert_eq!(data.geocoder_base, GEOCODER_BASE);
    }
}
