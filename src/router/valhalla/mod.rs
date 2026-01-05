//! Valhalla routing engines.

use calloop::channel::Sender;
use serde::{Deserialize, Deserializer};
use tracing::debug;

use crate::Error;
use crate::geometry::GeoPoint;
use crate::router::{self, Route, RoutingUpdate, Segment};
use crate::ui::view::search::QueryId;

pub mod offline;
pub mod online;

/// Valhalla polyline precision.
const POLYLINE_PRECISION: f64 = 1E6;

/// Valhalla route API response body.
#[derive(Deserialize)]
struct RouteResponse {
    trip: Trip,
}

impl RouteResponse {
    fn submit(
        self,
        id: QueryId,
        result_tx: &Sender<(QueryId, RoutingUpdate)>,
        router: &'static str,
    ) -> Result<(), Error> {
        // Skip sending results if routing failed.
        if self.trip.status != 0 {
            debug!("Valhalla {router} routing failed: {}", self.trip.status_message);
            return Ok(());
        }

        // Transform Valhalla response into Route.
        let mut response_route = Route {
            _time: self.trip.summary.time.round() as u64,
            _length: self.trip.summary.length,
            segments: Vec::new(),
        };
        for leg in self.trip.legs {
            for maneuver in leg.maneuvers {
                if let Some(segment) = maneuver.segment(&leg.shape) {
                    response_route.segments.push(segment);
                }
            }
        }

        // Submit result to the collector.
        let _ = result_tx.send((id, RoutingUpdate::Route(response_route)));

        Ok(())
    }
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
