//! Geocoding abstraction layer.

use std::cmp::Ordering;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, mpsc};

use calloop::channel::Event;
use calloop::{LoopHandle, channel};
use geocoder_nlp::SearchReference;
use reqwest::Client;

use crate::config::Config;
use crate::geometry::GeoPoint;
use crate::region::Regions;
use crate::ui::view::View;
use crate::ui::view::search::SearchView;
use crate::{Error, State};

mod nlp;
mod nominatim;

/// Multi-provider geocoder.
pub struct Geocoder {
    nominatim_query_tx: Option<mpsc::Sender<Query>>,
    nlp_query_tx: mpsc::Sender<Query>,

    result_tx: channel::Sender<(QueryId, QueryResultEvent)>,
    nominatim_url: Arc<String>,
    client: Client,

    results: Vec<QueryResult>,
    last_query: QueryId,
    nominatim_searching: bool,
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
            let search_view: &mut SearchView = state.window.views.get_mut(View::Search).unwrap();
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
                        (QueryResultRank::Nominatim(a), QueryResultRank::Nominatim(b)) => b.cmp(&a),
                        (QueryResultRank::Nominatim(_), QueryResultRank::Nlp(_)) => Ordering::Less,
                        (QueryResultRank::Nlp(a), QueryResultRank::Nlp(b)) => a.total_cmp(&b),
                        (QueryResultRank::Nlp(_), QueryResultRank::Nominatim(_)) => {
                            Ordering::Greater
                        },
                    });
                },
                // Mark current Nominatim search as done.
                QueryResultEvent::NominatimDone => geocoder.nominatim_searching = false,
                // Mark current Geocoder NLP search as done.
                QueryResultEvent::NlpDone => geocoder.nlp_searching = false,
            }

            search_view.update_results();
            state.window.unstall();
        })?;

        // Spawn Geocoder NLP thread.
        let (nlp_query_tx, nlp_query_rx) = mpsc::channel::<Query>();
        nlp::Geocoder::spawn(regions, nlp_query_rx, result_tx.clone())?;

        // Spawn Nominatim geocoder.
        let nominatim_query_tx = if config.search.nominatim_url.is_empty() {
            None
        } else {
            let (nominatim_query_tx, nominatim_query_rx) = mpsc::channel::<Query>();
            nominatim::Geocoder::spawn(
                client.clone(),
                config,
                nominatim_query_rx,
                result_tx.clone(),
            );
            Some(nominatim_query_tx)
        };

        Ok(Self {
            nominatim_query_tx,
            nlp_query_tx,
            result_tx,
            client,
            nominatim_url: config.search.nominatim_url.clone(),
            last_query: QueryId::new(),
            nominatim_searching: Default::default(),
            nlp_searching: Default::default(),
            results: Default::default(),
        })
    }

    /// Submit a new query.
    pub fn query(&mut self, query: Query) {
        self.last_query = query.id;
        self.nominatim_searching = true;
        self.nlp_searching = true;
        self.results.clear();

        if let Some(query_tx) = &self.nominatim_query_tx {
            let _ = query_tx.send(query.clone());
        }
        let _ = self.nlp_query_tx.send(query);
    }

    /// Get current query results.
    pub fn results(&self) -> &[QueryResult] {
        &self.results
    }

    /// Check if search is finished.
    pub fn searching(&self) -> bool {
        self.nominatim_searching || self.nlp_searching
    }

    /// Clear current search query and results.
    pub fn clear_results(&mut self) {
        self.last_query = QueryId::new();
        self.nominatim_searching = false;
        self.nlp_searching = false;
        self.results.clear();
    }

    /// Handle config updates.
    pub fn update_config(&mut self, config: &Config) {
        // Restart Nominatim geocoder on URL change.
        if config.search.nominatim_url != self.nominatim_url {
            self.nominatim_url = config.search.nominatim_url.clone();
            self.nominatim_query_tx = if config.search.nominatim_url.is_empty() {
                None
            } else {
                let (nominatim_query_tx, nominatim_query_rx) = mpsc::channel::<Query>();
                nominatim::Geocoder::spawn(
                    self.client.clone(),
                    config,
                    nominatim_query_rx,
                    self.result_tx.clone(),
                );
                Some(nominatim_query_tx)
            };
        }
    }
}

/// Geocoding search query.
#[derive(Clone)]
pub struct Query {
    id: QueryId,
    text: String,
    reference_point: Option<GeoPoint>,
    reference_zoom: Option<u8>,
}

impl Query {
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

/// Search query update event.
pub enum QueryResultEvent {
    /// New query results available.
    Results(Vec<QueryResult>),
    /// Nominatim search is done, no more results will be delivered.
    NominatimDone,
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
    /// Nominatim result rank, higher is better.
    Nominatim(u32),
}

/// Unique ID of a search query.
#[derive(PartialEq, Eq, Copy, Clone)]
pub struct QueryId(u64);

impl QueryId {
    fn new() -> Self {
        static NEXT_QUERY_ID: AtomicU64 = AtomicU64::new(0);
        Self(NEXT_QUERY_ID.fetch_add(1, AtomicOrdering::Relaxed))
    }
}
