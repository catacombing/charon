//! Online Valhalla router.

use std::sync::{Arc, mpsc};

use calloop::channel;
use reqwest::Client;
use serde::Serialize;
use tracing::{error, info};

use crate::Error;
use crate::config::Config;
use crate::geometry::GeoPoint;
use crate::router::valhalla::RouteResponse;
use crate::router::{Mode, RoutingQuery, RoutingUpdate};
use crate::ui::view::search::QueryId;

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
        let request = RouteRequest { locations, costing: query.mode };
        let data = serde_json::to_string(&request)?;

        // Get routing results from Valhalla.
        let url = format!("{}/route?json={}", self.url, data);
        let response = self.client.get(&url).send().await?.error_for_status()?;

        let route: RouteResponse = response.json().await?;

        route.submit(query, &self.result_tx, "Online")
    }
}

/// Valhalla route API request body.
#[derive(Serialize)]
struct RouteRequest {
    locations: Vec<GeoPoint>,
    costing: Mode,
}
