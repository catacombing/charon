//! Online Valhalla router.

use std::sync::{Arc, mpsc};

use calloop::channel;
use reqwest::Client;
use serde::{Deserialize, Deserializer, Serialize};
use tracing::{debug, error, info};

use crate::config::Config;
use crate::geometry::GeoPoint;
use crate::router::{Route, RoutingQuery, RoutingUpdate, Segment};
use crate::ui::view::search::QueryId;
use crate::{Error, router};

/// Valhalla polyline precision.
const POLYLINE_PRECISION: f64 = 1E6;

/// Valhalla API routing engine.
pub struct Router {
    query_rx: mpsc::Receiver<RoutingQuery>,
    result_tx: channel::Sender<(QueryId, RoutingUpdate)>,
    url: Arc<String>,
    client: Client,
}

impl Router {
    /// Spawn Valhalla API router in a tokio worker thread.
    pub fn spawn(
        client: Client,
        config: &Config,
        query_rx: mpsc::Receiver<RoutingQuery>,
        result_tx: channel::Sender<(QueryId, RoutingUpdate)>,
    ) {
        let url = config.search.valhalla_url.clone();
        tokio::spawn(async {
            let mut valhalla = Self { result_tx, query_rx, client, url };
            valhalla.listen().await;
        });
    }

    /// Listen for new routing queries.
    async fn listen(&mut self) {
        info!("Starting Valhalla API router ({})", self.url);

        while let Ok(query) = self.query_rx.recv() {
            if let Err(err) = self.route(query).await {
                error!("Valhalla API routing failed: {err}");
            }

            // Mark this query as done, regardless of success.
            let _ = self.result_tx.send((query.id, RoutingUpdate::ValhallaApiDone));
        }

        info!("Shutting down Valhalla API router ({})", self.url);
    }

    /// Process a routing query.
    async fn route(&mut self, query: RoutingQuery) -> Result<(), Error> {
        // Convert query to Valhalla routing request format.
        let locations = vec![query.origin, query.target];
        let request = RouteRequest { locations, costing: Costing::Auto };
        let data = serde_json::to_string(&request)?;

        // Get routing results from Valhalla.
        let url = format!("{}/route?json={}", self.url, data);
        let response = self.client.get(&url).send().await?.error_for_status()?;

        let route: RouteResponse = response.json().await?;

        // Skip sending results if routing failed.
        if route.trip.status != 0 {
            debug!("Valhalla API routing failed: {}", route.trip.status_message);
            return Ok(());
        }

        // Transform and submit routing result.
        let mut response_route = Route {
            time: route.trip.summary.time.round() as u64,
            _length: route.trip.summary.length,
            segments: Vec::new(),
        };
        for leg in route.trip.legs {
            for maneuver in leg.maneuvers {
                if let Some(segment) = maneuver.segment(&leg.shape) {
                    response_route.segments.push(segment);
                }
            }
        }
        let _ = self.result_tx.send((query.id, RoutingUpdate::Results(response_route)));

        Ok(())
    }
}

/// Valhalla route API request body.
#[derive(Serialize)]
struct RouteRequest {
    locations: Vec<GeoPoint>,
    costing: Costing,
}

/// Valhalla costing models.
#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum Costing {
    Auto,
}

/// Valhalla route API response body.
#[derive(Deserialize)]
struct RouteResponse {
    trip: Trip,
}

/// Routed Valhalla trip.
#[derive(Deserialize)]
struct Trip {
    legs: Vec<Leg>,
    summary: Summary,
    status: i32,
    status_message: String,
}

/// Leg in a Valhalla trip.
#[derive(Deserialize)]
struct Leg {
    maneuvers: Vec<Maneuver>,
    #[serde(deserialize_with = "deserialize_shape")]
    shape: Vec<GeoPoint>,
}

/// Maneuver in a Valhalla leg.
#[derive(Deserialize)]
struct Maneuver {
    length: f64,
    /// Estimated travel time in seconds.
    time: f64,
    begin_shape_index: usize,
    end_shape_index: usize,
}

impl Maneuver {
    /// Convert this maneuver to a segment.
    fn segment(&self, shape: &[GeoPoint]) -> Option<Segment> {
        if self.begin_shape_index >= shape.len() || self.end_shape_index >= shape.len() {
            return None;
        }

        Some(Segment {
            points: shape[self.begin_shape_index..self.end_shape_index + 1].to_vec(),
            _time: self.time.round() as u64,
            _length: self.length,
        })
    }
}

/// Valhalla route (section) metadata.
#[derive(Deserialize)]
struct Summary {
    length: f64,
    /// Estimated travel time in seconds.
    time: f64,
}

/// Deserialize a Valhalla shape polyline.
fn deserialize_shape<'de, D>(deserializer: D) -> Result<Vec<GeoPoint>, D::Error>
where
    D: Deserializer<'de>,
{
    let text = String::deserialize(deserializer)?;
    Ok(router::decode_polyline(&text, POLYLINE_PRECISION))
}
