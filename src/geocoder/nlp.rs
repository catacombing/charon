//! Geocoding using geocoder-nlp.

use std::path::Path;
use std::sync::{Arc, mpsc};
use std::thread::Builder as ThreadBuilder;

use calloop::channel;
use geocoder_nlp::Geocoder as GeocoderNlp;
use tracing::{error, info, warn};

use crate::Error;
use crate::geocoder::{Query, QueryId, QueryResult, QueryResultEvent, QueryResultRank};
use crate::geometry::GeoPoint;
use crate::region::Regions;

/// Geocoder NLP orchestrator.
pub struct Geocoder {
    geocoder: Option<GeocoderNlp>,

    regions: Arc<Regions>,

    query_rx: mpsc::Receiver<Query>,
    result_tx: channel::Sender<(QueryId, QueryResultEvent)>,
}

impl Geocoder {
    /// Spawn Geocoder NLP in a new background thread.
    pub fn spawn(
        regions: Arc<Regions>,
        query_rx: mpsc::Receiver<Query>,
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
        info!("Starting Geocoder NLP");

        let postal_global_path = self.regions.postal_global_path();

        while let Ok(query) = self.query_rx.recv() {
            self.query(&postal_global_path, query);
        }

        info!("Shutting down Geocoder NLP");
    }

    /// Process a geocoding query.
    fn query(&mut self, postal_global_path: &Path, query: Query) {
        let entity_types = crate::entity_type::entity_types();

        self.regions.world().for_installed(&mut |region| {
            // Get region-specific geocoding data paths.
            let postal_country_path = match self.regions.postal_country_root(region) {
                Some(postal_country_path) => postal_country_path,
                None => {
                    warn!("Installed country has no postal data: {}", region.name);
                    return;
                },
            };
            let geocoder_path = match self.regions.geocoder_path(region) {
                Some(geocoder_path) => geocoder_path,
                None => {
                    warn!("Installed country has no geocoder data: {}", region.name);
                    return;
                },
            };

            // Dynamically initialize the geocoder on first access.
            let geocoder = match &mut self.geocoder {
                Some(geocoder) => {
                    if let Err(err) = geocoder.set_geocoder_path(&geocoder_path) {
                        error!("Failed to update geocoder path for {}: {err}", region.name);
                        return;
                    }
                    geocoder.set_postal_country_path(&postal_country_path);
                    geocoder
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
                    self.geocoder.insert(geocoder_nlp)
                },
            };

            // Search this region for a result.
            let mut results = match geocoder.search(&query.text, query.reference_nlp()) {
                Ok(results) => results,
                // Since only one region might be broken, we don't return `false` here.
                Err(err) => {
                    error!("Failed geocoder-nlp search: {err}");
                    return;
                },
            };

            // Process results and send them to the collector.
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

                let distance = query.reference_point.map(|_| result.distance().round() as u32);
                let point = GeoPoint::new(result.latitude(), result.longitude());
                let rank = QueryResultRank::Nlp(result.search_rank());
                let postal_code = match result.postal_code().trim() {
                    "" => None,
                    postal_code => Some(postal_code.to_string()),
                };

                query_results.push(QueryResult {
                    entity_type,
                    postal_code,
                    distance,
                    point,
                    rank,
                    address: result.address().to_string(),
                    title: result.title().to_string(),
                });
            }

            let event = QueryResultEvent::Results(query_results);
            let _ = self.result_tx.send((query.id, event));
        });

        // Mark this query as done.
        let _ = self.result_tx.send((query.id, QueryResultEvent::Done));
    }
}
