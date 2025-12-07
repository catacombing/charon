//! Geocoding abstraction layer.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};

use calloop::channel::Event;
use calloop::{LoopHandle, channel};
use geocoder_nlp::SearchReference;

use crate::geometry::GeoPoint;
use crate::region::Regions;
use crate::ui::view::View;
use crate::ui::view::search::SearchView;
use crate::{Error, State};

mod nlp;

/// Multi-provider geocoder.
pub struct Geocoder {
    nlp_query_tx: mpsc::Sender<Query>,

    results: Vec<QueryResult>,
    last_query: QueryId,
    searching: bool,
}

impl Geocoder {
    pub fn new(
        event_loop: LoopHandle<'static, State>,
        regions: Arc<Regions>,
    ) -> Result<Self, Error> {
        let (nlp_query_tx, nlp_query_rx) = mpsc::channel::<Query>();
        let (nlp_result_tx, nlp_result_rx) = channel::channel();

        // Handle new geocoding results.
        event_loop.insert_source(nlp_result_rx, |event, _, state| {
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
                        (QueryResultRank::Nlp(a), QueryResultRank::Nlp(b)) => a.total_cmp(&b),
                    });
                },
                // Mark current search as done.
                QueryResultEvent::Done => geocoder.searching = false,
            }

            search_view.update_results();
            state.window.unstall();
        })?;

        // Spawn Geocoder NLP thread.
        nlp::Geocoder::spawn(regions, nlp_query_rx, nlp_result_tx)?;

        Ok(Self {
            nlp_query_tx,
            last_query: QueryId::new(),
            searching: Default::default(),
            results: Default::default(),
        })
    }

    /// Submit a new query.
    pub fn query(&mut self, query: Query) {
        self.last_query = query.id;
        self.searching = true;
        self.results.clear();

        let _ = self.nlp_query_tx.send(query);
    }

    /// Get current query results.
    pub fn results(&self) -> &[QueryResult] {
        &self.results
    }

    /// Check if search is finished.
    pub fn searching(&self) -> bool {
        self.searching
    }

    /// Clear current search query and results.
    pub fn clear_results(&mut self) {
        self.last_query = QueryId::new();
        self.searching = false;
        self.results.clear();
    }
}

/// Geocoding search query.
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
    /// Search is done, no more results will be delivered.
    Done,
}

/// Geocoding search result.
#[derive(Debug)]
pub struct QueryResult {
    pub point: GeoPoint,
    // Distance to the reference in meters.
    pub distance: Option<u32>,

    pub title: String,

    pub address: String,
    pub postal_code: Option<String>,

    pub entity_type: &'static str,

    pub rank: QueryResultRank,
}

/// Geocoder-specific search result rank.
#[derive(Copy, Clone, Debug)]
pub enum QueryResultRank {
    Nlp(f64),
}

/// Unique ID of a search query.
#[derive(PartialEq, Eq, Copy, Clone)]
pub struct QueryId(u64);

impl QueryId {
    fn new() -> Self {
        static NEXT_QUERY_ID: AtomicU64 = AtomicU64::new(0);
        Self(NEXT_QUERY_ID.fetch_add(1, Ordering::Relaxed))
    }
}
