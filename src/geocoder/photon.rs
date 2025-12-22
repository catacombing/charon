//! Online geocoding using photon.

use std::collections::HashMap;
use std::sync::{Arc, mpsc};

use calloop::channel;
use reqwest::Client;
use serde::Deserialize;
use tracing::{error, info};

use crate::config::Config;
use crate::geocoder::geojson::{Feature, GeoJson, Geometry};
use crate::geocoder::{
    QueryEvent, QueryId, QueryResult, QueryResultEvent, QueryResultRank, ReverseQuery, SearchQuery,
};
use crate::geometry::GeoPoint;
use crate::{Error, entity_type};

/// Maximum results returned by one Photon query.
const MAX_RESULTS: u8 = 15;

/// Photon geocoder.
pub struct Geocoder {
    query_rx: mpsc::Receiver<QueryEvent>,
    result_tx: channel::Sender<(QueryId, QueryResultEvent)>,
    url: Arc<String>,
    client: Client,
}

impl Geocoder {
    /// Spawn Photon geocoder in a tokio worker thread.
    pub fn spawn(
        client: Client,
        config: &Config,
        query_rx: mpsc::Receiver<QueryEvent>,
        result_tx: channel::Sender<(QueryId, QueryResultEvent)>,
    ) {
        let url = config.search.photon_url.clone();
        tokio::spawn(async {
            let mut geocoder = Self { result_tx, query_rx, client, url };
            geocoder.listen().await;
        });
    }

    /// Listen for new search queries.
    async fn listen(&mut self) {
        info!("Starting Photon Geocoder ({})", self.url);

        let entity_types = entity_type::entity_types();

        while let Ok(query) = self.query_rx.recv() {
            let id = query.id();
            match query {
                QueryEvent::Search(search_query) => {
                    if let Err(err) = self.search(entity_types, search_query).await {
                        error!("Photon geocoding failed: {err}");
                    }
                },
                QueryEvent::Reverse(reverse_query) => {
                    if let Err(err) = self.reverse(entity_types, reverse_query).await {
                        error!("Photon reverse geocoding failed: {err}");
                    }
                },
            }

            // Mark this query as done, regardless of success.
            let _ = self.result_tx.send((id, QueryResultEvent::PhotonDone));
        }

        info!("Shutting down Photon Geocoder ({})", self.url);
    }

    /// Process a geocoding search query.
    async fn search(
        &mut self,
        entity_types: &HashMap<&str, &'static str>,
        query: SearchQuery,
    ) -> Result<(), Error> {
        // Get geocoding results from Photon.
        let url = format!("{}/api/?q={}&limit={}", self.url, query.text, MAX_RESULTS);
        let response = self.client.get(&url).send().await?.error_for_status()?;

        let geo_json: GeoJson<PhotonProperties> = response.json().await?;

        // Transform and submit query results.
        let query_results = Self::map_geo_json(entity_types, query.reference_point, geo_json);
        let event = QueryResultEvent::Results(query_results);
        let _ = self.result_tx.send((query.id, event));

        Ok(())
    }

    /// Process a reverse geocoding query.
    async fn reverse(
        &mut self,
        entity_types: &HashMap<&str, &'static str>,
        query: ReverseQuery,
    ) -> Result<(), Error> {
        // Get geocoding results from Photon.
        let url = format!(
            "{}/reverse?lat={}&lon={}&limit={}",
            self.url, query.point.lat, query.point.lon, MAX_RESULTS,
        );
        let response = self.client.get(&url).send().await?.error_for_status()?;

        let geo_json: GeoJson<PhotonProperties> = response.json().await?;

        // Transform and submit query results.
        let query_results = Self::map_geo_json(entity_types, Some(query.point), geo_json);
        let event = QueryResultEvent::Results(query_results);
        let _ = self.result_tx.send((query.id, event));

        Ok(())
    }

    /// Map a Photon GeoJSON response to a list of query results.
    fn map_geo_json(
        entity_types: &HashMap<&str, &'static str>,
        reference_point: Option<GeoPoint>,
        geo_json: GeoJson<PhotonProperties>,
    ) -> Vec<QueryResult> {
        match geo_json {
            GeoJson::FeatureCollection(feature_collection) => {
                let features = feature_collection.features.into_iter().enumerate();
                features
                    .filter_map(|(i, feature)| {
                        Self::map_feature(entity_types, reference_point, feature, i)
                    })
                    .collect()
            },
            GeoJson::Feature(feature) => {
                match Self::map_feature(entity_types, reference_point, feature, 0) {
                    Some(query_result) => vec![query_result],
                    None => Vec::new(),
                }
            },
            // Ignore geometries without any additional detail.
            GeoJson::Geometry(_) => Vec::new(),
        }
    }

    /// Try to map a Photon GeoJSON feature to a query result.
    fn map_feature(
        entity_types: &HashMap<&str, &'static str>,
        reference_point: Option<GeoPoint>,
        feature: Feature<PhotonProperties>,
        index: usize,
    ) -> Option<QueryResult> {
        let properties = feature.properties?;
        let address = properties.address();
        let title = properties.name?;

        // Filter out unknown entity types.
        //
        // Unknown entities generally refer to old data like
        // `emergency_fire_detection_system`, which have been removed from OSM. Since
        // these are likely irrelevant, we remove them from the result.
        let entity_type = format!("{}_{}", properties.osm_key, properties.osm_value);
        let entity_type = entity_types.get(&*entity_type).map(|et| &**et)?;

        // Map geometry; luckily Photon only uses points, which makes our life easier.
        let point = match feature.geometry? {
            Geometry::Point(point) if point.coordinates.len() == 2 => {
                GeoPoint::new(point.coordinates[1], point.coordinates[0])
            },
            _ => return None,
        };

        Some(QueryResult {
            entity_type,
            address,
            point,
            title,
            distance: reference_point.map(|p| p.distance(point)),
            rank: QueryResultRank::Photon(index),
        })
    }
}

/// Photon API GeoJSON properties.
#[derive(Deserialize)]
struct PhotonProperties {
    osm_key: String,
    osm_value: String,

    postcode: Option<String>,
    housenumber: Option<String>,
    street: Option<String>,
    district: Option<String>,
    city: Option<String>,
    state: Option<String>,
    country: Option<String>,

    name: Option<String>,
}

impl PhotonProperties {
    /// Assemble address from its parts.
    fn address(&self) -> String {
        let mut address = String::new();
        if let Some(postcode) = &self.postcode {
            address.push_str(postcode);
            address.push_str(", ");
        }
        if let Some(housenumber) = &self.housenumber {
            address.push_str(housenumber);
            address.push_str(", ");
        }
        if let Some(street) = &self.street {
            address.push_str(street);
            address.push_str(", ");
        }
        if let Some(district) = &self.district {
            address.push_str(district);
            address.push_str(", ");
        }
        if let Some(city) = &self.city {
            address.push_str(city);
            address.push_str(", ");
        }
        if let Some(state) = &self.state {
            address.push_str(state);
            address.push_str(", ");
        }
        if let Some(country) = &self.country {
            address.push_str(country);
            address.push_str(", ");
        }
        address
    }
}
