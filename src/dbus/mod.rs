//! DBus interfaces.

use std::future;

use calloop::channel::Sender;
use tracing::warn;
use zbus::Connection;

use crate::Error;
use crate::dbus::iio_sensor_proxy::IioCompassSource;
use crate::dbus::modem_manager::ModemGpsSource;
use crate::geometry::GeoPoint;

mod iio_sensor_proxy;
pub mod modem_manager;

/// Listen for DBus updates.
pub async fn dbus_listen(tx: Sender<(Option<GeoPoint>, Option<f64>)>) -> Result<(), Error> {
    let connection = Connection::system().await?;

    // Create modem GPS listener.
    let mut gps_source = ModemGpsSource::new(&connection).await?;

    // Create iio-sensor-proxy compass listener.
    let mut compass_source = IioCompassSource::new(&connection)
        .await
        .inspect_err(|err| warn!("Failed to initialize iio-sensor-proxy compass source: {err}"))
        .ok()
        .flatten();

    let mut location = gps_source.location().await;
    let mut heading = None;

    loop {
        // Publish current location.
        if tx.send((location, heading)).is_err() {
            // If the channel was closed, we terminate.
            return Ok(());
        }

        let compass_future = async {
            match (&mut compass_source, location) {
                // Enable compass if we have a GPS location fix.
                (Some(compass_source), Some(_)) => {
                    compass_source.resume().await;
                    compass_source.listen().await
                },
                // Disable compass if we don't have a GPS location fix.
                (Some(compass_source), None) => {
                    compass_source.pause().await;
                    heading = None;
                    future::pending().await
                },
                (None, _) => future::pending().await,
            }
        };

        tokio::select! {
            _ = gps_source.listen(&connection) => location = gps_source.location().await,
            new_heading = compass_future => heading = Some(new_heading),
        }
    }
}
