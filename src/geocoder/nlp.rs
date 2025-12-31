//! Offline geocoding using geocoder-nlp.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, mpsc};
use std::thread::Builder as ThreadBuilder;

use calloop::channel;
use geocoder_nlp::{Geocoder as GeocoderNlp, SearchIter};
use tracing::{error, info, warn};

use crate::geocoder::{
    QueryEvent, QueryResult, QueryResultEvent, QueryResultRank, ReverseQuery, SearchQuery,
};
use crate::geometry::{self, GeoPoint};
use crate::region::{Region, Regions};
use crate::ui::view::search::QueryId;
use crate::{Error, entity_type};

/// Search radius in pixels for reverse geocoding.
const SEARCH_RADIUS: f64 = 50.;

/// Maximum search radius in meters.
///
/// This is required since Geocoder NLP will just search through EVERY available
/// entry otherwise, which tends to be pathological beyond certain sizes.
const MAX_SEARCH_RADIUS: f64 = 1_000.;

/// Geocoder NLP orchestrator.
pub struct Geocoder {
    geocoder: Option<GeocoderNlp>,

    regions: Arc<Regions>,

    query_rx: mpsc::Receiver<QueryEvent>,
    result_tx: channel::Sender<(QueryId, QueryResultEvent)>,
}

impl Geocoder {
    /// Spawn Geocoder NLP in a new background thread.
    pub fn spawn(
        regions: Arc<Regions>,
        query_rx: mpsc::Receiver<QueryEvent>,
        result_tx: channel::Sender<(QueryId, QueryResultEvent)>,
    ) -> Result<(), Error> {
        ThreadBuilder::new().name("geocoder-nlp".into()).spawn(move || {
            let mut geocoder = Self { result_tx, query_rx, regions, geocoder: Default::default() };
            geocoder.listen();
        })?;
        Ok(())
    }

    /// Listen for new search queries.
    fn listen(&mut self) {
        info!("Starting NLP geocoder");

        let postal_global_path = self.regions.postal_global_path();
        let entity_types = entity_type::entity_types();

        while let Ok(query) = self.query_rx.recv() {
            let id = query.id();
            match query {
                QueryEvent::Search(search_query) => {
                    self.search(&postal_global_path, entity_types, search_query);
                },
                QueryEvent::Reverse(reverse_query) => {
                    self.reverse(&postal_global_path, entity_types, reverse_query);
                },
            }

            // Mark this query as done.
            let _ = self.result_tx.send((id, QueryResultEvent::NlpDone));
        }

        info!("Shutting down NLP geocoder");
    }

    /// Process a geocoding search query.
    fn search(
        &mut self,
        postal_global_path: &Path,
        entity_types: &HashMap<&str, &'static str>,
        query: SearchQuery,
    ) {
        self.regions.world().for_installed(&mut |region| {
            Self::init_geocoder(&mut self.geocoder, &self.regions, region, postal_global_path);
            let geocoder = match &mut self.geocoder {
                Some(geocoder) => geocoder,
                None => return,
            };

            // Search this region for a result.
            let results = match geocoder.search(&query.text, query.reference_nlp()) {
                Ok(results) => results,
                // Since only one region might be broken, we don't return `false` here.
                Err(err) => {
                    error!("Failed geocoder-nlp search: {err}");
                    return;
                },
            };

            // Process results and send them to the collector.
            let query_results = Self::map_results(entity_types, query.reference_point, results);
            let event = QueryResultEvent::Results(query_results);
            let _ = self.result_tx.send((query.id, event));
        });
    }

    /// Process a reverse geocoding query.
    fn reverse(
        &mut self,
        postal_global_path: &Path,
        entity_types: &HashMap<&str, &'static str>,
        query: ReverseQuery,
    ) {
        self.regions.world().for_installed(&mut |region| {
            Self::init_geocoder(&mut self.geocoder, &self.regions, region, postal_global_path);
            let geocoder = match &mut self.geocoder {
                Some(geocoder) => geocoder,
                None => return,
            };

            // Convert search radius in pixels to search radius in meters.
            let pixel_size = geometry::pixel_size(query.point.lat, query.zoom);
            let search_radius = (SEARCH_RADIUS * pixel_size).min(MAX_SEARCH_RADIUS);

            // Search this region for a result.
            let results = match geocoder.reverse(query.point.lat, query.point.lon, search_radius) {
                Ok(results) => results,
                // Since only one region might be broken, we don't return `false` here.
                Err(err) => {
                    error!("Failed geocoder-nlp reverse search: {err}");
                    return;
                },
            };

            // Process results and send them to the collector.
            let query_results = Self::map_results(entity_types, Some(query.point), results);
            let event = QueryResultEvent::Results(query_results);
            let _ = self.result_tx.send((query.id, event));
        });
    }

    /// Map Geocoder NLP result to our expected format.
    fn map_results(
        entity_types: &HashMap<&str, &'static str>,
        reference_point: Option<GeoPoint>,
        mut results: SearchIter,
    ) -> Vec<QueryResult> {
        let mut query_results = Vec::new();
        while let Some(result) = results.next() {
            // Filter out unknown entity types.
            //
            // Unknown entities generally refer to old data like
            // `emergency_fire_detection_system`, which have been removed from OSM. Since
            // these are likely irrelevant, we remove them from the result.
            let entity_type = match entity_types.get(&*result.entity_type()).map(|et| &**et) {
                Some(entity_type) => entity_type,
                None => continue,
            };

            let distance = reference_point.map(|_| result.distance().round() as u32);
            let point = GeoPoint::new(result.latitude(), result.longitude());
            let rank = QueryResultRank::Nlp(result.search_rank());
            let address = match result.postal_code().trim() {
                "" => result.address().to_string(),
                postal_code => format!("{}, {}", postal_code, result.address()),
            };

            query_results.push(QueryResult {
                entity_type,
                distance,
                address,
                point,
                rank,
                title: result.title().to_string(),
            });
        }
        query_results
    }

    /// Dynamically initialize geocoder for a region.
    fn init_geocoder(
        geocoder: &mut Option<GeocoderNlp>,
        regions: &Regions,
        region: &Region,
        postal_global_path: &Path,
    ) {
        // Get region-specific geocoding data paths.
        let postal_country_path = match regions.postal_country_root(region) {
            Some(postal_country_path) => postal_country_path,
            None => {
                warn!("Installed country has no postal data: {}", region.name);
                return;
            },
        };
        let geocoder_path = match regions.geocoder_path(region) {
            Some(geocoder_path) => geocoder_path,
            None => {
                warn!("Installed country has no geocoder data: {}", region.name);
                return;
            },
        };

        // Initialize or update the geocoder.
        match geocoder {
            Some(geocoder) => {
                if let Err(err) = geocoder.set_geocoder_path(&geocoder_path) {
                    error!("Failed to update geocoder path for {}: {err}", region.name);
                    return;
                }
                geocoder.set_postal_country_path(&postal_country_path);
            },
            None => {
                let geocoder_nlp = match GeocoderNlp::new(
                    postal_global_path,
                    &postal_country_path,
                    &geocoder_path,
                ) {
                    Ok(geocoder) => geocoder,
                    Err(err) => {
                        error!("Failed to initialize geocoder for {}: {err}", region.name);
                        return;
                    },
                };
                *geocoder = Some(geocoder_nlp);
            },
        }
    }
}
