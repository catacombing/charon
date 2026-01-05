//! Geocoding abstraction layer.

use std::cmp::Ordering;
use std::sync::{Arc, mpsc};

use calloop::channel::Event;
use calloop::{LoopHandle, channel};
use geocoder_nlp::SearchReference;
use reqwest::Client;

use crate::config::Config;
use crate::geometry::GeoPoint;
use crate::region::Regions;
use crate::ui::view::search::QueryId;
use crate::{Error, State};

mod geojson;
mod nlp;
mod photon;

/// Multi-provider geocoder.
pub struct Geocoder {
    photon_query_tx: Option<mpsc::Sender<QueryEvent>>,
    nlp_query_tx: mpsc::Sender<QueryEvent>,

    result_tx: channel::Sender<(QueryId, QueryResultEvent)>,
    photon_url: Arc<String>,
    client: Client,

    results: Vec<QueryResult>,
    last_query: QueryId,
    photon_searching: bool,
    nlp_searching: bool,
}

impl Geocoder {
    pub fn new(
        event_loop: LoopHandle<'static, State>,
        config: &Config,
        client: Client,
        regions: Arc<Regions>,
    ) -> Result<Self, Error> {
        let (result_tx, result_rx) = channel::channel();

        // Handle new geocoding results.
        event_loop.insert_source(result_rx, |event, _, state| {
            let search_view = state.window.views.search();
            let geocoder = search_view.geocoder_mut();

            let query_event = match event {
                // Ignore events for old queries.
                Event::Msg((id, _)) if id != geocoder.last_query => return,
                Event::Msg((_, query_event)) => query_event,
                Event::Closed => return,
            };

            match query_event {
                // Update search results.
                QueryResultEvent::Results(results) => {
                    // Add results and sort them with the best match first.
                    geocoder.results.extend(results);
                    geocoder.results.sort_unstable_by(|a, b| match (a.rank, b.rank) {
                        (QueryResultRank::Photon(a), QueryResultRank::Photon(b)) => a.cmp(&b),
                        (QueryResultRank::Photon(_), QueryResultRank::Nlp(_)) => Ordering::Less,
                        (QueryResultRank::Nlp(a), QueryResultRank::Nlp(b)) => a.total_cmp(&b),
                        (QueryResultRank::Nlp(_), QueryResultRank::Photon(_)) => Ordering::Greater,
                    });
                },
                // Mark current Photon search as done.
                QueryResultEvent::PhotonDone => geocoder.photon_searching = false,
                // Mark current Geocoder NLP search as done.
                QueryResultEvent::NlpDone => geocoder.nlp_searching = false,
            }

            // Notify user about geocoding failure.
            if !geocoder.searching() && geocoder.results.is_empty() {
                search_view.set_error("No Entity Found");
            }

            search_view.set_dirty();
            state.window.unstall();
        })?;

        // Spawn Geocoder NLP thread.
        let (nlp_query_tx, nlp_query_rx) = mpsc::channel::<QueryEvent>();
        nlp::Geocoder::spawn(regions, nlp_query_rx, result_tx.clone())?;

        // Spawn Photon geocoder.
        let photon_query_tx = (!config.search.photon_url.is_empty()).then(|| {
            let (photon_query_tx, photon_query_rx) = mpsc::channel::<QueryEvent>();
            photon::Geocoder::spawn(client.clone(), config, photon_query_rx, result_tx.clone());
            photon_query_tx
        });

        Ok(Self {
            photon_query_tx,
            nlp_query_tx,
            result_tx,
            client,
            photon_url: config.search.photon_url.clone(),
            last_query: QueryId::new(),
            photon_searching: Default::default(),
            nlp_searching: Default::default(),
            results: Default::default(),
        })
    }

    /// Submit a search query.
    pub fn search(&mut self, query: SearchQuery) {
        self.query(QueryEvent::Search(query));
    }

    /// Submit a reverse geocoding query.
    pub fn reverse(&mut self, query: ReverseQuery) {
        self.query(QueryEvent::Reverse(query));
    }

    /// Clear the current search.
    pub fn reset(&mut self) {
        self.last_query = QueryId::new();
        self.photon_searching = false;
        self.nlp_searching = false;
        self.results.clear();
    }

    /// Get current query results.
    pub fn results(&self) -> &[QueryResult] {
        &self.results
    }

    /// Check if search is finished.
    pub fn searching(&self) -> bool {
        self.photon_searching || self.nlp_searching
    }

    /// Handle config updates.
    pub fn update_config(&mut self, config: &Config) {
        // Restart Photon geocoder on URL change.
        if config.search.photon_url != self.photon_url {
            // Drop old router first, to improve log order.
            self.photon_query_tx = None;

            self.photon_url = config.search.photon_url.clone();
            self.photon_query_tx = (!config.search.photon_url.is_empty()).then(|| {
                let (photon_query_tx, photon_query_rx) = mpsc::channel::<QueryEvent>();
                photon::Geocoder::spawn(
                    self.client.clone(),
                    config,
                    photon_query_rx,
                    self.result_tx.clone(),
                );
                photon_query_tx
            });
        }
    }

    /// Submit any type of query to all geocoders.
    fn query(&mut self, query: QueryEvent) {
        self.last_query = query.id();
        self.photon_searching = true;
        self.nlp_searching = true;
        self.results.clear();

        if let Some(query_tx) = &self.photon_query_tx {
            let _ = query_tx.send(query.clone());
        }
        let _ = self.nlp_query_tx.send(query);
    }
}

/// Geocoder query types.
#[derive(Clone)]
pub enum QueryEvent {
    Search(SearchQuery),
    Reverse(ReverseQuery),
}

impl QueryEvent {
    fn id(&self) -> QueryId {
        match self {
            Self::Search(search_query) => search_query.id,
            Self::Reverse(reverse_query) => reverse_query.id,
        }
    }
}

/// Geocoding search query.
#[derive(Clone)]
pub struct SearchQuery {
    id: QueryId,
    text: String,
    reference_point: Option<GeoPoint>,
    reference_zoom: Option<u8>,
}

impl SearchQuery {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            id: QueryId::new(),
            text: query.into(),
            reference_point: Default::default(),
            reference_zoom: Default::default(),
        }
    }

    /// Set the query's search reference.
    pub fn set_reference(&mut self, point: GeoPoint, zoom: u8) {
        self.reference_point = Some(point);
        self.reference_zoom = Some(zoom);
    }

    /// Get query's reference point in NLP's [`SearchReference`] format.
    fn reference_nlp(&self) -> Option<SearchReference> {
        let point = self.reference_point?;
        let mut reference = SearchReference::new(point.lat, point.lon);
        if let Some(zoom) = self.reference_zoom {
            reference.set_zoom(zoom);
        }
        Some(reference)
    }
}

/// Reverse geocoding query.
#[derive(Clone)]
pub struct ReverseQuery {
    id: QueryId,
    point: GeoPoint,
    zoom: u8,
}

impl ReverseQuery {
    pub fn new(point: GeoPoint, zoom: u8) -> Self {
        Self { point, zoom, id: QueryId::new() }
    }
}

/// Search query update event.
pub enum QueryResultEvent {
    /// New query results available.
    Results(Vec<QueryResult>),
    /// Photon search is done, no more results will be delivered.
    PhotonDone,
    /// Geocoder NLP search is done, no more results will be delivered.
    NlpDone,
}

/// Geocoding search result.
#[derive(Debug)]
pub struct QueryResult {
    pub point: GeoPoint,
    // Distance to the reference in meters.
    pub distance: Option<u32>,

    pub title: String,

    pub address: String,

    pub entity_type: &'static str,

    pub rank: QueryResultRank,
}

/// Geocoder-specific search result rank.
#[derive(Copy, Clone, Debug)]
pub enum QueryResultRank {
    /// Geocoder NLP result rank, lower is better.
    Nlp(f64),
    /// Photon result rank, lower is better.
    Photon(usize),
}
