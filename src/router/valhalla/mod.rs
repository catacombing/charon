//! Valhalla routing engines.

use std::sync::Arc;

use calloop::channel::Sender;
use serde::{Deserialize, Deserializer};
use tracing::debug;

use crate::Error;
use crate::router::{self, GeoPoint, Route, RoutingQuery, RoutingUpdate, Segment};
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
        query: RoutingQuery,
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
            time: self.trip.summary.time.round() as u64,
            length: (self.trip.summary.length * 1_000.).round() as u32,
            segments: Vec::new(),
            mode: query.mode,
        };
        for leg in self.trip.legs {
            for maneuver in leg.maneuvers {
                if let Some(segment) = maneuver.segment(&leg.shape) {
                    response_route.segments.push(segment);
                }
            }
        }

        // Submit result to the collector.
        let _ = result_tx.send((query.id, RoutingUpdate::Route(response_route)));

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
    instruction: String,
    /// Estimated travel time in seconds.
    time: f64,
    begin_shape_index: usize,
    end_shape_index: usize,
}

impl Maneuver {
    /// Convert this maneuver to a segment.
    fn segment(mut self, shape: &[GeoPoint]) -> Option<Segment> {
        if self.begin_shape_index >= shape.len() || self.end_shape_index >= shape.len() {
            return None;
        }

        // Trim trailing full stop from Valhalla instructions, since it looks odd.
        if self.instruction.ends_with('.') {
            self.instruction.truncate(self.instruction.len() - 1);
        }

        Some(Segment {
            points: shape[self.begin_shape_index..self.end_shape_index + 1].to_vec(),
            instruction: Arc::new(self.instruction),
            time: self.time.round() as u64,
            length: (self.length * 1_000.).round() as u32,
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
