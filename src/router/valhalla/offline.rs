//! Offline Valhalla router.

use std::sync::{Arc, mpsc};

use calloop::channel;
use tracing::{error, info};
use valhalla::proto::Options;
use valhalla::{Actor, Config, Response};

use crate::Error;
use crate::region::Regions;
use crate::router::valhalla::RouteResponse;
use crate::router::{RoutingQuery, RoutingUpdate};
use crate::ui::view::search::QueryId;

/// Valhalla configuration file.
const VALHALLA_CONFIG: &str = include_str!("config.json");

/// Valhalla API routing engine.
pub struct Router {
    query_rx: mpsc::Receiver<RoutingQuery>,
    result_tx: channel::Sender<(QueryId, RoutingUpdate)>,
    actor: Actor,
}

impl Router {
    /// Spawn Valhalla API router in a tokio worker thread.
    pub fn spawn(
        regions: Arc<Regions>,
        query_rx: mpsc::Receiver<RoutingQuery>,
        result_tx: channel::Sender<(QueryId, RoutingUpdate)>,
    ) -> Result<(), Error> {
        // Replace variables in Valhalla config.
        let tiles_path = regions.valhalla_tiles_path();
        let tiles_path = tiles_path.to_str().ok_or(Error::MissingCacheDir)?;
        let config = VALHALLA_CONFIG.replace("{TILE_DIR}", tiles_path);

        // Start Valhalla behemoth.
        let config = Config::from_json(&config)?;
        let actor = Actor::new(&config)?;

        tokio::spawn(async {
            let mut valhalla = Self { result_tx, query_rx, actor };
            valhalla.listen().await;
        });

        Ok(())
    }

    /// Listen for new routing queries.
    async fn listen(&mut self) {
        info!("Starting Valhalla Offline router");

        while let Ok(query) = self.query_rx.recv() {
            if let Err(err) = self.route(query).await {
                error!("Valhalla Offline routing failed: {err}");
            }

            // Mark this query as done, regardless of success.
            let _ = self.result_tx.send((query.id, RoutingUpdate::ValhallaOfflineDone));
        }

        info!("Shutting down Valhalla Offline router");
    }

    /// Process a routing query.
    async fn route(&mut self, query: RoutingQuery) -> Result<(), Error> {
        let request = Options {
            costing_type: query.mode as i32,
            locations: vec![query.origin.into(), query.target.into()],
            ..Default::default()
        };

        let route: RouteResponse = match self.actor.route(&request)? {
            Response::Json(json) => serde_json::from_str(&json)?,
            _ => return Err(Error::ValhallaInvalidResponseType),
        };

        route.submit(query.id, &self.result_tx, "Offline")
    }
}
