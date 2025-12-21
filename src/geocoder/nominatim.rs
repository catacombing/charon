//! Online geocoding using nominatim.openstreetmap.org.

use std::collections::HashMap;
use std::sync::{Arc, mpsc};

use calloop::channel;
use reqwest::Client;
use serde::Deserialize;
use tracing::{error, info};

use crate::config::Config;
use crate::geocoder::{Query, QueryId, QueryResult, QueryResultEvent, QueryResultRank};
use crate::geometry::GeoPoint;
use crate::{Error, entity_type};

/// Geocoder NLP orchestrator.
pub struct Geocoder {
    query_rx: mpsc::Receiver<Query>,
    result_tx: channel::Sender<(QueryId, QueryResultEvent)>,
    url: Arc<String>,
    client: Client,
}

impl Geocoder {
    /// Spawn Geocoder NLP in a new background thread.
    pub fn spawn(
        client: Client,
        config: &Config,
        query_rx: mpsc::Receiver<Query>,
        result_tx: channel::Sender<(QueryId, QueryResultEvent)>,
    ) {
        let url = config.search.nominatim_url.clone();
        tokio::spawn(async {
            let mut geocoder = Self { result_tx, query_rx, client, url };
            geocoder.listen().await;
        });
    }

    /// Listen for new search queries.
    async fn listen(&mut self) {
        info!("Starting Nominatim Geocoder ({})", self.url);

        let entity_types = entity_type::entity_types();

        while let Ok(query) = self.query_rx.recv() {
            if let Err(err) = self.query(entity_types, query).await {
                error!("Nominatim geocoding failed: {err}");
            }
        }

        info!("Shutting down Nominatim Geocoder ({})", self.url);
    }

    /// Process a geocoding query.
    async fn query(
        &mut self,
        entity_types: &HashMap<&str, &'static str>,
        query: Query,
    ) -> Result<(), Error> {
        // Get geocoding results from Nominatim.
        let url = format!("{}/search?format=jsonv2&q={}", self.url, query.text);
        let response = self.client.get(&url).send().await?.error_for_status()?;

        let places: Vec<Place> = response.json().await?;

        // Transform and submit query results.
        let query_results = places
            .into_iter()
            .filter_map(|place| {
                // Filter out unknown entity types.
                //
                // Unknown entities generally refer to old data like
                // `emergency_fire_detection_system`, which have been removed from OSM. Since
                // these are likely irrelevant, we remove them from the result.
                let entity_type = format!("{}_{}", place.category, place.category_type);
                let entity_type = match entity_types.get(&*entity_type).map(|et| &**et) {
                    Some(entity_type) => entity_type,
                    None => return None,
                };

                let point = match (place.lat.parse::<f64>(), place.lon.parse::<f64>()) {
                    (Ok(lat), Ok(lon)) => GeoPoint::new(lat, lon),
                    _ => return None,
                };
                let rank = QueryResultRank::Nominatim(place.place_rank);
                let distance = query.reference_point.map(|p| p.distance(point));

                Some(QueryResult {
                    entity_type,
                    distance,
                    point,
                    rank,
                    address: place.address,
                    title: place.name,
                })
            })
            .collect();
        let event = QueryResultEvent::Results(query_results);
        let _ = self.result_tx.send((query.id, event));

        // Mark this query as done.
        let _ = self.result_tx.send((query.id, QueryResultEvent::NominatimDone));

        Ok(())
    }
}

/// Nominatim query response entity.
#[derive(Deserialize)]
struct Place {
    name: String,
    lat: String,
    lon: String,
    #[serde(rename = "display_name")]
    address: String,
    category: String,
    #[serde(rename = "type")]
    category_type: String,
    place_rank: u32,
}
