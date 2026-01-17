//! Rust bindings for geocoder-nlp.
//!
//! Geocoding is the process of taking a text-based description of a location,
//! such as an address or the name of a place, and returning geographic
//! coordinates.
//!
//! This crate statically compiles [geocoder-nlp], which is an offline-capable
//! geocoder using [postal] for address normalization.
//!
//! While geocoder-nlp and libpostal are compiled statically, it will still link
//! dynamically to kyotocabinet, sqlite3, and marisa. All of these are required
//! runtime dependencies.
//!
//! Additionally boost is required as a compile-time dependency.
//!
//! [geocoder-nlp]: https://github.com/rinigus/geocoder-nlp
//! [postal]: https://github.com/openvenues/libpostal
//!
//! # Examples
//!
//! ```no_run
//! use geocoder_nlp::Geocoder;
//!
//! let mut geocoder = Geocoder::new("/tmp/postal", "/tmp/postal", "/tmp/geocoder").unwrap();
//!
//! // Get all results matching `Rúa` in our selected dataset.
//! let mut results = geocoder.search("Rúa", None).unwrap();
//!
//! // Output results in descending relevance.
//! while let Some(result) = results.next() {
//!     println!("Title: {}", result.title());
//!     println!("Latitude: {}, Longitude: {}", result.latitude(), result.longitude());
//!     println!("Address: {} {}", result.postal_code(), result.address());
//!     println!();
//! }
//! ```
use std::borrow::Cow;
use std::fmt::{self, Debug, Formatter};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use cxx::{CxxVector, UniquePtr, let_cxx_string};

mod ffi;

/// Geocoding error.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// The specified geocoder-nlp dataset failed to load.
    #[error("Failed to load geocoder dataset")]
    GeocoderLoadFailed,
    /// [`Geocoder::search`] was called with an uninitialized postal instance.
    #[error("Failed to initialize postal")]
    PostalInit,
}

/// Geocoder used for POI and address search.
pub struct Geocoder {
    geocoder: UniquePtr<ffi::Geocoder>,
    postal: UniquePtr<ffi::Postal>,
}

impl Geocoder {
    /// Create a new geocoder.
    ///
    /// See [`Self::set_geocoder_path`] and [`Self::set_postal_paths`] for
    /// details about the expected datasets at these locations.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use geocoder_nlp::Geocoder;
    ///
    /// let _geocoder = Geocoder::new("/tmp/postal", "/tmp/postal", "/tmp/geocoder").unwrap();
    /// ```
    pub fn new(
        postal_global_path: impl AsRef<Path>,
        postal_country_path: impl AsRef<Path>,
        geocoder_path: impl AsRef<Path>,
    ) -> Result<Self, Error> {
        let_cxx_string!(postal_global = postal_global_path.as_ref().as_os_str().as_bytes());
        let_cxx_string!(postal_country = postal_country_path.as_ref().as_os_str().as_bytes());
        let mut postal = ffi::new_postal();
        postal.pin_mut().set_postal_datadir(&postal_global, &postal_country);
        postal.pin_mut().set_use_primitive(false);

        let mut geocoder = Self { postal, geocoder: ffi::new_geocoder() };

        geocoder.set_geocoder_path(geocoder_path)?;
        geocoder.geocoder.pin_mut().set_max_results(10);
        geocoder.geocoder.pin_mut().set_max_queries_per_hierarchy(30);

        Ok(geocoder)
    }

    /// Update the geocoder dataset path.
    ///
    /// The dataset for geocoder-nlp is stored by region and can be imported
    /// from the nominatim database. See the [geocoder-nlp importer] for
    /// details about importing your own data.
    ///
    /// An example for what this data looks like can be found on the
    /// [modrana.org webserver]. This server is maintained by a third party;
    /// usage of this library does not automatically authorize you to use
    /// this data in your application.
    ///
    /// The geocoder dataset path is expected to point to a directory which
    /// contains the following three uncompressed files:
    ///  - geonlp-normalized-id.kch
    ///  - geonlp-normalized.trie
    ///  - geonlp-primary.sqlite
    ///
    /// [modrana.org webserver]: https://data.modrana.org/osm_scout_server/geocoder-nlp-39/geocoder-nlp/
    /// [geocoder-nlp importer]: https://github.com/rinigus/geocoder-nlp/tree/master/importer
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use geocoder_nlp::Geocoder;
    ///
    /// let mut geocoder = Geocoder::new("/tmp/postal", "/tmp/postal", "/tmp/geocoder").unwrap();
    ///
    /// geocoder.set_geocoder_path("/tmp/geocoder2").unwrap();
    /// ```
    pub fn set_geocoder_path(&mut self, path: impl AsRef<Path>) -> Result<(), Error> {
        let_cxx_string!(path = path.as_ref().as_os_str().as_bytes());
        if self.geocoder.pin_mut().load(&path) { Ok(()) } else { Err(Error::GeocoderLoadFailed) }
    }

    /// Update the postal global and country dataset.
    ///
    /// You can run postal's tool to download all necessary data:
    ///
    /// ```sh
    /// libpostal_data download all
    /// ```
    ///
    /// This will download the postal normalization data required to normalize
    /// addresses in any language, which takes around 1.9G. If you download all
    /// this data to the same location, you can point both the `global` and
    /// `country` path at this directory.
    ///
    /// To reduce the storage size required, you can create the `address_parser`
    /// data for each country and only download it for the countries you use. To
    /// do this, simply point the `country` path at the directory for the
    /// language you wish to use.
    ///
    /// You can find an example for separated [global] and [country] datasets on
    /// the modrana.org webserver. This server is maintained by a third
    /// party; usage of this library does not automatically authorize you to use
    /// this data in your application.
    ///
    /// The global dataset path is expected to contain the following
    /// directories:
    ///  - address_expansions
    ///  - language_classifier
    ///  - numex
    ///  - transliteration
    ///
    /// The country dataset path is expected to contain the following
    /// directories:
    ///  - address_parser
    ///
    /// The contents of these directories must match the modrana.org webserver
    /// examples, with all files decompressed.
    ///
    /// [global]: https://data.modrana.org/osm_scout_server/postal-global-2/postal/global-v1/
    /// [country]: https://data.modrana.org/osm_scout_server/postal-country-2/postal/countries-v1/
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use geocoder_nlp::Geocoder;
    ///
    /// let mut geocoder = Geocoder::new("/tmp/postal", "/tmp/postal", "/tmp/geocoder").unwrap();
    ///
    /// geocoder.set_postal_paths("/tmp/postal2", "/tmp/postal2");
    /// ```
    pub fn set_postal_paths(
        &mut self,
        global_path: impl AsRef<Path>,
        country_path: impl AsRef<Path>,
    ) {
        let_cxx_string!(global = global_path.as_ref().as_os_str().as_bytes());
        let_cxx_string!(country = country_path.as_ref().as_os_str().as_bytes());
        self.postal.pin_mut().set_postal_datadir(&global, &country);
    }

    /// Update the postal country dataset.
    ///
    /// In contrast to [`Self::set_postal_paths`] this allows updating the
    /// country dataset without having to reload the language agnostic
    /// postal data.
    ///
    /// See [`Self::set_postal_paths`] for details on the data format expected
    /// for the postal country dataset.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use geocoder_nlp::Geocoder;
    ///
    /// let mut geocoder = Geocoder::new("/tmp/postal", "/tmp/postal", "/tmp/geocoder").unwrap();
    ///
    /// geocoder.set_postal_country_path("/tmp/postal2");
    /// ```
    pub fn set_postal_country_path(&mut self, country_path: impl AsRef<Path>) {
        let_cxx_string!(country = country_path.as_ref().as_os_str().as_bytes());
        self.postal.pin_mut().set_postal_datadir_country(&country);
    }

    /// Search for an address or POI.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use geocoder_nlp::Geocoder;
    ///
    /// let mut geocoder = Geocoder::new("/tmp/postal", "/tmp/postal", "/tmp/geocoder").unwrap();
    ///
    /// // Get all results matching `Rúa` in our selected dataset.
    /// let mut results = geocoder.search("Rúa", None).unwrap();
    ///
    /// // Output results in descending relevance.
    /// while let Some(result) = results.next() {
    ///     println!("Title: {}", result.title());
    ///     println!("Latitude: {}, Longitude: {}", result.latitude(), result.longitude());
    ///     println!("Address: {} {}", result.postal_code(), result.address());
    ///     println!();
    /// }
    /// ```
    pub fn search(
        &mut self,
        query: &str,
        reference: Option<SearchReference>,
    ) -> Result<SearchIter, Error> {
        // Try to parse address with postal.
        let mut parse_results = CxxVector::new();
        let mut non_normalized = ffi::new_parse_result();
        let_cxx_string!(query = query);
        let success =
            self.postal.pin_mut().parse(&query, parse_results.pin_mut(), non_normalized.pin_mut());

        if !success {
            return Err(Error::PostalInit);
        }

        let mut results = CxxVector::new();
        let reference = reference.map_or_else(ffi::empty_geo_reference, |r| r.into());
        self.geocoder.pin_mut().search(&parse_results, results.pin_mut(), 0, &reference);

        Ok(SearchIter { results, index: 0 })
    }

    /// Find POIs at the specified location.
    ///
    /// The search radius is not limited internally, which will become
    /// pathological at some point based on radius and geocoder region size.
    /// Since result rank is based on distance, the radius can usually be
    /// limited to a kilometer or less without affecting results.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use geocoder_nlp::Geocoder;
    ///
    /// let mut geocoder = Geocoder::new("/tmp/postal", "/tmp/postal", "/tmp/geocoder").unwrap();
    ///
    /// // Get entities at the specified lat/lon in a 10 meter radius.
    /// let mut results = geocoder.reverse(42.224966, -8.670664, 10.).unwrap();
    ///
    /// // Output results in descending relevance.
    /// while let Some(result) = results.next() {
    ///     println!("Title: {}", result.title());
    ///     println!("Latitude: {}, Longitude: {}", result.latitude(), result.longitude());
    ///     println!("Address: {} {}", result.postal_code(), result.address());
    ///     println!();
    /// }
    /// ```
    pub fn reverse(
        &mut self,
        latitude: f64,
        longitude: f64,
        radius: f64,
    ) -> Result<SearchIter, Error> {
        let mut results = CxxVector::new();
        let success = self.geocoder.pin_mut().search_nearby(
            &CxxVector::new(),
            &CxxVector::new(),
            latitude,
            longitude,
            radius,
            results.pin_mut(),
            self.postal.pin_mut(),
        );

        if !success {
            return Err(Error::PostalInit);
        }

        Ok(SearchIter { results, index: 0 })
    }

    /// Get the maximum number of results returned by [`Self::search`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use geocoder_nlp::Geocoder;
    ///
    /// let geocoder = Geocoder::new("/tmp/postal", "/tmp/postal", "/tmp/geocoder").unwrap();
    ///
    /// assert_eq!(geocoder.max_results(), 10);
    /// ```
    pub fn max_results(&self) -> u64 {
        self.geocoder.get_max_results()
    }

    /// Set the number of search results returned.
    ///
    /// This limit applies to both [`Self::search`] and [`Self::reverse`].
    ///
    /// The default limit is `10`.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use geocoder_nlp::Geocoder;
    ///
    /// let mut geocoder = Geocoder::new("/tmp/postal", "/tmp/postal", "/tmp/geocoder").unwrap();
    ///
    /// geocoder.set_max_results(20);
    ///
    /// assert_eq!(geocoder.max_results(), 20);
    /// ```
    pub fn set_max_results(&mut self, max_results: u64) {
        self.geocoder.pin_mut().set_max_results(max_results)
    }
}

/// Reference point for [`Geocoder::search`].
///
/// This should usually be the current GPS location of the user.
///
/// # Examples
///
/// ```no_run
/// use geocoder_nlp::{Geocoder, SearchReference};
///
/// let mut reference = SearchReference::new(42.225197, -8.670981);
/// reference.set_zoom(18);
///
/// let mut geocoder = Geocoder::new("/tmp/postal", "/tmp/postal", "/tmp/geocoder").unwrap();
/// geocoder.search("Rúa", Some(reference));
/// ```
pub struct SearchReference {
    lat: f64,
    lon: f64,
    zoom: Option<i32>,
    importance: Option<f64>,
}

impl SearchReference {
    /// Crate a new reference point from latitude and longitude.
    ///
    /// # Examples
    ///
    /// ```
    /// use geocoder_nlp::SearchReference;
    ///
    /// let _reference = SearchReference::new(42.225197, -8.670981);
    /// ```
    pub fn new(lat: f64, lon: f64) -> Self {
        Self { lat, lon, importance: Default::default(), zoom: Default::default() }
    }

    /// Set the reference point's zoom level.
    ///
    /// The zoom level refers to the Y index of the current tile.
    /// With an increased zoom level, the importance of distant results will
    /// diminish.
    ///
    /// Zoom levels 18 and above are all considered the same, so a level higher
    /// than 18 no longer amplifies the impact of the search distance.
    ///
    /// # Examples
    ///
    /// ```
    /// use geocoder_nlp::SearchReference;
    ///
    /// let mut reference = SearchReference::new(42.225197, -8.670981);
    ///
    /// reference.set_zoom(14);
    /// ```
    pub fn set_zoom(&mut self, zoom: u8) {
        self.zoom = Some(zoom as i32);
    }

    /// Set the reference point's importance.
    ///
    /// A higher importance will increase the impact of search distance on the
    /// result. A value of `0` is equivalent to not passing a search reference.
    ///
    /// The default is `0.75`.
    ///
    /// # Examples
    ///
    /// ```
    /// use geocoder_nlp::SearchReference;
    ///
    /// let mut reference = SearchReference::new(42.225197, -8.670981);
    ///
    /// reference.set_importance(1.);
    /// ```
    pub fn set_importance(&mut self, importance: f64) {
        self.importance = Some(importance);
    }
}

impl From<SearchReference> for UniquePtr<ffi::GeoReference> {
    fn from(reference: SearchReference) -> Self {
        let importance = reference.importance.unwrap_or(0.75);
        let zoom = reference.zoom.unwrap_or(16);
        ffi::new_geo_reference(reference.lat, reference.lon, zoom, importance)
    }
}

/// Iterator over search results.
///
/// See [`Geocoder::search`].
pub struct SearchIter {
    results: UniquePtr<CxxVector<ffi::GeoResult>>,
    index: usize,
}

impl SearchIter {
    /// Get the next search result.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<SearchResult<'_>> {
        let result = self.results.get(self.index)?;
        self.index += 1;
        Some(SearchResult { result })
    }
}

/// Geocoding search result.
pub struct SearchResult<'a> {
    result: &'a ffi::GeoResult,
}

impl<'a> SearchResult<'a> {
    /// Geographical latitude of the result.
    pub fn latitude(&self) -> f64 {
        self.result.get_latitude()
    }

    /// Geographical longitude of the result.
    pub fn longitude(&self) -> f64 {
        self.result.get_longitude()
    }

    /// Distance of the result to the search reference.
    ///
    /// If no search reference was passed, the distance will always be `0`.
    pub fn distance(&self) -> f64 {
        self.result.get_distance()
    }

    /// Title of the result's entity.
    pub fn title(&self) -> Cow<'a, str> {
        self.result.get_title().to_string_lossy()
    }

    /// Postal code of the result's entity.
    pub fn postal_code(&self) -> Cow<'a, str> {
        self.result.get_postal_code().to_string_lossy()
    }

    /// Address of the result's entity.
    ///
    /// The address does not include the postal code. To get the postal code,
    /// see [`Self::postal_code`].
    pub fn address(&self) -> Cow<'a, str> {
        self.result.get_address().to_string_lossy()
    }

    /// OSM tag of the result's entity.
    pub fn entity_type(&self) -> Cow<'a, str> {
        self.result.get_type().to_string_lossy()
    }

    /// Phone number of the result's entity.
    pub fn phone(&self) -> Cow<'a, str> {
        self.result.get_phone().to_string_lossy()
    }

    /// Website of the result's entity.
    pub fn website(&self) -> Cow<'a, str> {
        self.result.get_website().to_string_lossy()
    }

    /// Importance of the search result.
    ///
    /// A lower value indicates a more likely match for the search query.
    ///
    /// Negative values are possible with a reference point while in the
    /// vicinity of the search result.
    pub fn search_rank(&self) -> f64 {
        self.result.get_search_rank()
    }
}

impl<'a> Debug for SearchResult<'a> {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), fmt::Error> {
        f.debug_struct("SearchResult")
            .field("latitude", &self.latitude())
            .field("longitude", &self.longitude())
            .field("distance", &self.distance())
            .field("title", &self.title())
            .field("postal_code", &self.postal_code())
            .field("address", &self.address())
            .field("entity_type", &self.entity_type())
            .field("phone", &self.phone())
            .field("website", &self.website())
            .field("search_rank", &self.search_rank())
            .finish()
    }
}
