//! iio-sensor-proxy DBus interface.

use futures_lite::stream::StreamExt;
use tokio::time::{self, Duration, Interval, MissedTickBehavior};
use tracing::{error, info};
use zbus::proxy::PropertyStream;
use zbus::{Connection, proxy};

use crate::Error;

/// Interval between sensor updates.
const INTERVAL: Duration = Duration::from_millis(500);

/// iio-sensor-proxy DBus compass source.
pub struct IioCompassSource {
    heading_stream: PropertyStream<'static, f64>,
    compass: CompassProxy<'static>,
    interval: Interval,
    claimed: bool,
    heading: f64,
}

impl IioCompassSource {
    /// Create new compass source.
    ///
    /// The compass source will start out as paused by default, see
    /// [`Self::resume`] to start receiving compass heading updates.
    pub async fn new(connection: &Connection) -> Result<Option<Self>, Error> {
        let compass = CompassProxy::new(connection).await?;

        // Check whether this device has a compass available.
        if compass.has_compass().await != Ok(true) {
            info!("No compass available through iio-sensor-proxy");
            return Ok(None);
        }

        // Get stream for heading changes.
        let heading_stream = compass.receive_compass_heading_changed().await;

        // Configure interval to fire at most once when any amount of ticks have passed.
        let mut interval = time::interval(INTERVAL);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        info!("Started iio-sensor-proxy compass");

        Ok(Some(Self {
            heading_stream,
            compass,
            interval: time::interval(INTERVAL),
            claimed: Default::default(),
            heading: Default::default(),
        }))
    }

    /// Process the next iio-sensor-proxy compass update.
    pub async fn listen(&mut self) -> f64 {
        // Avoid excessive compass updates.
        //
        // By default the iio-sensor-proxy sends out updates at a very high frequency
        // (every 100-200ms). Since rotation updates at such a high frequency aren't
        // beneficial, we artificially ignore updates to improve battery usage.
        self.interval.tick().await;

        if let Some(property_change) = self.heading_stream.next().await {
            match property_change.get().await {
                Ok(heading) => self.heading = heading,
                Err(err) => {
                    error!("Failed to read heading property: {err}");
                    self.heading = 0.;
                },
            }
        }

        self.heading
    }

    /// Pause compass updates.
    pub async fn pause(&mut self) {
        // Skip state change when already in the desired state.
        if !self.claimed {
            return;
        }
        self.claimed = false;

        info!("Pausing iio-sensor-proxy compass");

        if let Err(err) = self.compass.release_compass().await {
            error!("Failed to release iio-sensor-proxy compass claim: {err}");
        }
    }

    /// Resume compass updates.
    pub async fn resume(&mut self) {
        // Skip state change when already in the desired state.
        if self.claimed {
            return;
        }
        self.claimed = true;

        info!("Resuming iio-sensor-proxy compass");

        if let Err(err) = self.compass.claim_compass().await {
            error!("Failed to acquire iio-sensor-proxy compass claim: {err}");
        }
    }
}

#[proxy(
    interface = "net.hadess.SensorProxy",
    default_service = "net.hadess.SensorProxy",
    default_path = "/net/hadess/SensorProxy"
)]
pub trait SensorProxy {
    /// ClaimAccelerometer method
    fn claim_accelerometer(&self) -> zbus::Result<()>;

    /// ClaimLight method
    fn claim_light(&self) -> zbus::Result<()>;

    /// ClaimProximity method
    fn claim_proximity(&self) -> zbus::Result<()>;

    /// ReleaseAccelerometer method
    fn release_accelerometer(&self) -> zbus::Result<()>;

    /// ReleaseLight method
    fn release_light(&self) -> zbus::Result<()>;

    /// ReleaseProximity method
    fn release_proximity(&self) -> zbus::Result<()>;

    /// AccelerometerOrientation property
    #[zbus(property)]
    fn accelerometer_orientation(&self) -> zbus::Result<String>;

    /// AccelerometerTilt property
    #[zbus(property)]
    fn accelerometer_tilt(&self) -> zbus::Result<String>;

    /// HasAccelerometer property
    #[zbus(property)]
    fn has_accelerometer(&self) -> zbus::Result<bool>;

    /// HasAmbientLight property
    #[zbus(property)]
    fn has_ambient_light(&self) -> zbus::Result<bool>;

    /// HasProximity property
    #[zbus(property)]
    fn has_proximity(&self) -> zbus::Result<bool>;

    /// LightLevel property
    #[zbus(property)]
    fn light_level(&self) -> zbus::Result<f64>;

    /// LightLevelUnit property
    #[zbus(property)]
    fn light_level_unit(&self) -> zbus::Result<String>;

    /// ProximityNear property
    #[zbus(property)]
    fn proximity_near(&self) -> zbus::Result<bool>;
}

#[proxy(
    interface = "net.hadess.SensorProxy.Compass",
    default_service = "net.hadess.SensorProxy",
    default_path = "/net/hadess/SensorProxy/Compass"
)]
pub trait Compass {
    /// ClaimCompass method
    fn claim_compass(&self) -> zbus::Result<()>;

    /// ReleaseCompass method
    fn release_compass(&self) -> zbus::Result<()>;

    /// CompassHeading property
    #[zbus(property)]
    fn compass_heading(&self) -> zbus::Result<f64>;

    /// HasCompass property
    #[zbus(property)]
    fn has_compass(&self) -> zbus::Result<bool>;
}
