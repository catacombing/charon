//! Route planning abstraction layer.

use std::sync::{Arc, mpsc};

use calloop::channel::Event;
use calloop::{LoopHandle, channel};
use reqwest::Client;

use crate::config::Config;
use crate::geometry::GeoPoint;
use crate::ui::view::View;
use crate::ui::view::search::QueryId;
use crate::{Error, State};

mod valhalla_api;

/// Multi-provider router
pub struct Router {
    valhalla_api_query_tx: Option<mpsc::Sender<RoutingQuery>>,

    result_tx: channel::Sender<(QueryId, RoutingUpdate)>,
    valhalla_url: Arc<String>,
    client: Client,

    results: Vec<Route>,
    last_query: QueryId,
    valhalla_api_routing: bool,
}

impl Router {
    pub fn new(
        event_loop: LoopHandle<'static, State>,
        config: &Config,
        client: Client,
    ) -> Result<Self, Error> {
        let (result_tx, result_rx) = channel::channel();

        // Handle new routing results.
        event_loop.insert_source(result_rx, |event, _, state| {
            let router = state.window.views.search().router_mut();

            let query_event = match event {
                // Ignore events for old queries.
                Event::Msg((id, _)) if id != router.last_query => return,
                Event::Msg((_, query_event)) => query_event,
                Event::Closed => return,
            };

            match query_event {
                // Update routing results.
                RoutingUpdate::Results(route) => router.results.push(route),
                // Mark current Valhalla API routing as done.
                RoutingUpdate::ValhallaApiDone => router.valhalla_api_routing = false,
            }

            // Once all routers are done, close the search to show the route.
            if !router.routing() {
                if router.results.is_empty() {
                    state.window.views.search().set_error("No Route Found");
                    state.window.unstall();
                } else {
                    // Extract shortest route from all results.
                    router.results.sort_unstable_by(|a, b| a.time.cmp(&b.time));
                    let shortest_route = router.results.swap_remove(0);
                    router.results.clear();

                    state.window.views.map().set_route(shortest_route);

                    state.window.set_view(View::Map);
                }
            }
        })?;

        // Spawn Valhalla API routing engine.
        let valhalla_api_query_tx = (!config.search.valhalla_url.is_empty()).then(|| {
            let (query_tx, query_rx) = mpsc::channel::<RoutingQuery>();
            valhalla_api::Router::spawn(client.clone(), config, query_rx, result_tx.clone());
            query_tx
        });

        Ok(Self {
            valhalla_api_query_tx,
            result_tx,
            client,
            valhalla_url: config.search.valhalla_url.clone(),
            last_query: QueryId::new(),
            valhalla_api_routing: Default::default(),
            results: Default::default(),
        })
    }

    /// Submit a routing query to all engines.
    pub fn route(&mut self, query: RoutingQuery) {
        self.last_query = query.id;
        self.valhalla_api_routing = true;
        self.results.clear();

        if let Some(query_tx) = &self.valhalla_api_query_tx {
            let _ = query_tx.send(query);
        }
    }

    /// Check if routing is finished.
    pub fn routing(&self) -> bool {
        self.valhalla_api_routing
    }

    /// Handle config updates.
    pub fn update_config(&mut self, config: &Config) {
        // Restart Valhalla API routing engine on URL change.
        if config.search.valhalla_url != self.valhalla_url {
            self.valhalla_url = config.search.valhalla_url.clone();
            self.valhalla_api_query_tx = (!config.search.valhalla_url.is_empty()).then(|| {
                let (query_tx, query_rx) = mpsc::channel::<RoutingQuery>();
                valhalla_api::Router::spawn(
                    self.client.clone(),
                    config,
                    query_rx,
                    self.result_tx.clone(),
                );
                query_tx
            });
        }
    }
}

/// Routing query.
#[derive(Copy, Clone)]
pub struct RoutingQuery {
    id: QueryId,
    origin: GeoPoint,
    target: GeoPoint,
}

impl RoutingQuery {
    pub fn new(origin: GeoPoint, target: GeoPoint) -> Self {
        Self { origin, target, id: QueryId::new() }
    }
}

/// Routing query update event.
pub enum RoutingUpdate {
    /// New query results available.
    Results(Route),
    /// Valhalla online wrouting is done, no more results will be delivered.
    ValhallaApiDone,
}

/// Routing result.
#[derive(Debug)]
pub struct Route {
    /// Trip segments.
    pub segments: Vec<Segment>,
    /// Complete trip time in seconds.
    pub time: u64,
    /// Complete trip length in kilometers.
    pub _length: f64,
}

/// Subsection of a route.
#[derive(Debug)]
pub struct Segment {
    pub points: Vec<GeoPoint>,
    /// Complete trip time in seconds.
    pub _time: u64,
    /// Complete trip length in kilometers.
    pub _length: f64,
}

/// Decode a polyline string.
///
/// See <https://developers.google.com/maps/documentation/utilities/polylinealgorithm>.
/// See <https://valhalla.github.io/valhalla/decoding/>.
fn decode_polyline(polyline: &str, precision: f64) -> Vec<GeoPoint> {
    let mut shape = Vec::new();

    let mut chars = polyline.chars();
    let mut last_lat = 0;
    let mut last_lon = 0;

    // Get the next latitude/longitude tuple.
    let mut next_coordinates = || {
        last_lat = parse_polyline_coordinate(&mut chars, last_lat)?;
        last_lon = parse_polyline_coordinate(&mut chars, last_lon)?;
        Some((last_lat, last_lon))
    };

    while let Some((lat, lon)) = next_coordinates() {
        let point = GeoPoint::new(lat as f64 / precision, lon as f64 / precision);
        shape.push(point);
    }

    shape
}

/// Parse the next latitude or longitude in the polyline string.
fn parse_polyline_coordinate(mut chars: impl Iterator<Item = char>, previous: i32) -> Option<i32> {
    let mut byte = None;
    let mut result = 0;
    let mut shift = 0;

    while byte.is_none_or(|b| b >= 0x20) {
        let byte = *byte.insert(chars.next()? as i32 - 63);
        result |= (byte & 0x1F) << shift;
        shift += 5;
    }

    let value = if result & 1 != 0 { previous + !(result >> 1) } else { previous + (result >> 1) };

    Some(value)
}

#[test]
fn decode_polyline5() {
    let x = decode_polyline("_p~iF~ps|U_ulLnnqC_mqNvxq`@", 1E5);
    let decoded = vec![
        GeoPoint::new(38.5, -120.2),
        GeoPoint::new(40.7, -120.95),
        GeoPoint::new(43.252, -126.453),
    ];
    assert_eq!(x, decoded);
}

#[test]
fn decode_polyline6() {
    let x = decode_polyline("e~epoA|jfpOiDaK", 1E6);
    let decoded = vec![GeoPoint::new(42.225139, -8.670911), GeoPoint::new(42.225224, -8.670718)];
    assert_eq!(x, decoded);
}
