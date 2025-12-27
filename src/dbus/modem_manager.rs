//! ModemManager DBus interface.

use std::collections::HashMap;
use std::future;
use std::time::Duration;

use calloop::channel::Sender;
use futures_lite::stream::StreamExt;
use tokio::task::JoinSet;
use tokio::time::{self, Interval};
use tracing::{error, info};
use zbus::fdo::ObjectManagerProxy;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Type, Value};
use zbus::{Connection, proxy};

use crate::Error;
use crate::geometry::GeoPoint;

/// Listen for GPS location updates
pub async fn gps_listen(tx: Sender<Option<GeoPoint>>) -> Result<(), Error> {
    let connection = Connection::system().await?;

    // Create object manager for modem changes.
    let object_manager = ObjectManagerProxy::builder(&connection)
        .destination("org.freedesktop.ModemManager1")?
        .path("/org/freedesktop/ModemManager1")?
        .build()
        .await?;

    // Fill list of active location proxies.
    let mut proxies = location_proxies(&connection, &object_manager).await;

    // Get stream for modem changes.
    let mut modem_added_stream = object_manager.receive_interfaces_added().await?;
    let mut modem_removed_stream = object_manager.receive_interfaces_removed().await?;

    // Get GPS refresh rate.
    let mut refresh_rate = gps_refresh_rate(&proxies).await;

    let log_refresh_rate = |refresh_rate: &Option<Interval>| match refresh_rate {
        Some(refresh_rate) => {
            let refresh_rate = refresh_rate.period().as_secs();
            info!("Updated modem GPS polling rate to {refresh_rate}s");
        },
        None => info!("Paused modem GPS polling; no GPS source present"),
    };
    info!("Started modem GPS pollin");
    log_refresh_rate(&refresh_rate);

    // Transmit initial location.
    let _ = tx.send(location(&proxies).await);

    loop {
        let next_refresh = async {
            match &mut refresh_rate {
                Some(refresh_rate) => refresh_rate.tick().await,
                None => future::pending().await,
            }
        };

        tokio::select! {
            // Wait for next GPS refresh tick.
            _ = next_refresh => (),

            // Wait for GPS property changes.
            _ = properties_changed(&proxies) => {
                refresh_rate = gps_refresh_rate(&proxies).await;
                log_refresh_rate(&refresh_rate);
            },

            // Wait for new/removed modems.
            Some(_) = modem_added_stream.next() => {
                proxies = location_proxies(&connection, &object_manager).await;
                refresh_rate = gps_refresh_rate(&proxies).await;
                log_refresh_rate(&refresh_rate);
            },
            Some(_) = modem_removed_stream.next() => {
                proxies = location_proxies(&connection, &object_manager).await;
                refresh_rate = gps_refresh_rate(&proxies).await;
                log_refresh_rate(&refresh_rate);
            },

            else => continue,
        };

        // Publish new location event.
        if tx.send(location(&proxies).await).is_err() {
            // If the channel was closed, we terminate.
            return Ok(());
        }
    }
}

/// Get GPS location from a list of location proxies.
async fn location(proxies: &[LocationProxy<'_>]) -> Option<GeoPoint> {
    // Return data from first modem with raw GPS enabled.
    let gps_raw = ModemLocationSource::GpsRaw as u32;
    for proxy in proxies {
        // Log errors, to indicate missing polkit permissions.
        let locations = match proxy.get_location().await {
            Ok(locations) => locations,
            Err(err) => {
                error!("Failed to get modem location: {err}");
                continue;
            },
        };

        if let Some(location) = locations.get(&gps_raw)
            && let Value::Dict(dict) = &**location
            && let Ok(Some(lat)) = dict.get(&"latitude")
            && let Ok(Some(lon)) = dict.get(&"longitude")
        {
            return Some(GeoPoint::new(lat, lon));
        }
    }

    None
}

/// Get the minimum refresh interval from a list of location proxies.
///
/// This only checks location proxies with raw GPS enabled, since it is the only
/// supported location format.
async fn gps_refresh_rate(proxies: &[LocationProxy<'_>]) -> Option<Interval> {
    // Setup duration to never refresh my default.
    let mut min_secs = None;

    // Find shortest refresh interval from all proxies.
    let gps_raw = ModemLocationSource::GpsRaw as u32;
    for proxy in proxies {
        if proxy.enabled().await.is_ok_and(|enabled| enabled & gps_raw != 0)
            && let Ok(refresh_rate) = proxy.gps_refresh_rate().await
            && min_secs.is_none_or(|min| min >= refresh_rate)
        {
            min_secs = Some(refresh_rate);
        }
    }

    min_secs.map(|secs| time::interval(Duration::from_secs(secs as u64)))
}

/// Await GPS-related modem property changes.
async fn properties_changed(proxies: &[LocationProxy<'static>]) {
    // Avoid hot loop without modem GPS source.
    if proxies.is_empty() {
        future::pending::<()>().await;
        return;
    }

    let mut set = JoinSet::new();

    // Spawn a future for each relevant property of each proxy.
    //
    // The streams must be polled twice, since the first event will fire immediately
    // for the current state.
    for proxy in proxies.iter() {
        let mut refresh_rate_stream = proxy.receive_gps_refresh_rate_changed().await;
        set.spawn(async move {
            refresh_rate_stream.next().await;
            refresh_rate_stream.next().await;
        });

        let mut enabled_stream = proxy.receive_enabled_changed().await;
        set.spawn(async move {
            enabled_stream.next().await;
            enabled_stream.next().await;
        });
    }

    set.join_next().await;
}

/// Get the location proxies for all active modems.
async fn location_proxies(
    connection: &Connection,
    object_manager: &ObjectManagerProxy<'_>,
) -> Vec<LocationProxy<'static>> {
    let managed_objects = object_manager.get_managed_objects().await;

    let mut proxies = Vec::new();
    for (path, _) in managed_objects.into_iter().flatten() {
        if path.starts_with("/org/freedesktop/ModemManager1/Modem/")
            && let Ok(proxy) = location_from_path(connection, path).await
        {
            proxies.push(proxy);
        }
    }

    proxies
}

/// Try and convert a DBus device path to a location proxy.
async fn location_from_path(
    connection: &Connection,
    device_path: OwnedObjectPath,
) -> zbus::Result<LocationProxy<'static>> {
    LocationProxy::builder(connection).path(device_path)?.build().await
}

#[proxy(
    interface = "org.freedesktop.ModemManager1",
    default_service = "org.freedesktop.ModemManager1",
    default_path = "/org/freedesktop/ModemManager1"
)]
trait ModemManager1 {
    /// InhibitDevice method
    fn inhibit_device(&self, uid: &str, inhibit: bool) -> zbus::Result<()>;

    /// ReportKernelEvent method
    fn report_kernel_event(&self, properties: HashMap<&str, Value<'_>>) -> zbus::Result<()>;

    /// ScanDevices method
    fn scan_devices(&self) -> zbus::Result<()>;

    /// SetLogging method
    fn set_logging(&self, level: &str) -> zbus::Result<()>;

    /// Version property
    #[zbus(property)]
    fn version(&self) -> zbus::Result<String>;
}

#[proxy(
    interface = "org.freedesktop.ModemManager1.Modem.Location",
    default_service = "org.freedesktop.ModemManager1",
    default_path = "/org/freedesktop/ModemManager1/Modem/0"
)]
trait Location {
    /// GetLocation method
    fn get_location(&self) -> zbus::Result<HashMap<u32, OwnedValue>>;

    /// InjectAssistanceData method
    fn inject_assistance_data(&self, data: &[u8]) -> zbus::Result<()>;

    /// SetGpsRefreshRate method
    fn set_gps_refresh_rate(&self, rate: u32) -> zbus::Result<()>;

    /// SetSuplServer method
    fn set_supl_server(&self, supl: &str) -> zbus::Result<()>;

    /// Setup method
    fn setup(&self, sources: u32, signal_location: bool) -> zbus::Result<()>;

    /// AssistanceDataServers property
    #[zbus(property)]
    fn assistance_data_servers(&self) -> zbus::Result<Vec<String>>;

    /// Capabilities property
    #[zbus(property)]
    fn capabilities(&self) -> zbus::Result<u32>;

    /// Enabled property
    #[zbus(property)]
    fn enabled(&self) -> zbus::Result<u32>;

    /// GpsRefreshRate property
    #[zbus(property)]
    fn gps_refresh_rate(&self) -> zbus::Result<u32>;

    /// Location property
    #[zbus(property)]
    fn location(&self) -> zbus::Result<HashMap<u32, OwnedValue>>;

    /// SignalsLocation property
    #[zbus(property)]
    fn signals_location(&self) -> zbus::Result<bool>;

    /// SuplServer property
    #[zbus(property)]
    fn supl_server(&self) -> zbus::Result<String>;

    /// SupportedAssistanceData property
    #[zbus(property)]
    fn supported_assistance_data(&self) -> zbus::Result<u32>;
}

#[proxy(
    interface = "org.freedesktop.ModemManager1.Modem.Signal",
    default_service = "org.freedesktop.ModemManager1",
    default_path = "/org/freedesktop/ModemManager1/Modem/0"
)]
trait Signal {
    /// Setup method
    fn setup(&self, rate: u32) -> zbus::Result<()>;

    /// SetupThresholds method
    fn setup_thresholds(&self, settings: HashMap<&str, Value<'_>>) -> zbus::Result<()>;

    /// Cdma property
    #[zbus(property)]
    fn cdma(&self) -> zbus::Result<HashMap<String, OwnedValue>>;

    /// ErrorRateThreshold property
    #[zbus(property)]
    fn error_rate_threshold(&self) -> zbus::Result<bool>;

    /// Evdo property
    #[zbus(property)]
    fn evdo(&self) -> zbus::Result<HashMap<String, OwnedValue>>;

    /// Gsm property
    #[zbus(property)]
    fn gsm(&self) -> zbus::Result<HashMap<String, OwnedValue>>;

    /// Lte property
    #[zbus(property)]
    fn lte(&self) -> zbus::Result<HashMap<String, OwnedValue>>;

    /// Nr5g property
    #[zbus(property)]
    fn nr5g(&self) -> zbus::Result<HashMap<String, OwnedValue>>;

    /// Rate property
    #[zbus(property)]
    fn rate(&self) -> zbus::Result<u32>;

    /// RssiThreshold property
    #[zbus(property)]
    fn rssi_threshold(&self) -> zbus::Result<u32>;

    /// Umts property
    #[zbus(property)]
    fn umts(&self) -> zbus::Result<HashMap<String, OwnedValue>>;
}

#[proxy(
    interface = "org.freedesktop.ModemManager1.Modem.Modem3gpp.Ussd",
    default_service = "org.freedesktop.ModemManager1",
    default_path = "/org/freedesktop/ModemManager1/Modem/0"
)]
trait Ussd {
    /// Cancel method
    fn cancel(&self) -> zbus::Result<()>;

    /// Initiate method
    fn initiate(&self, command: &str) -> zbus::Result<String>;

    /// Respond method
    fn respond(&self, response: &str) -> zbus::Result<String>;

    /// NetworkNotification property
    #[zbus(property)]
    fn network_notification(&self) -> zbus::Result<String>;

    /// NetworkRequest property
    #[zbus(property)]
    fn network_request(&self) -> zbus::Result<String>;

    /// State property
    #[zbus(property)]
    fn state(&self) -> zbus::Result<u32>;
}

#[proxy(
    interface = "org.freedesktop.ModemManager1.Modem.Messaging",
    default_service = "org.freedesktop.ModemManager1",
    default_path = "/org/freedesktop/ModemManager1/Modem/0"
)]
trait Messaging {
    /// Create method
    fn create(&self, properties: HashMap<&str, Value<'_>>) -> zbus::Result<OwnedObjectPath>;

    /// Delete method
    fn delete(&self, path: &ObjectPath<'_>) -> zbus::Result<()>;

    /// List method
    fn list(&self) -> zbus::Result<Vec<OwnedObjectPath>>;

    /// Added signal
    #[zbus(signal)]
    fn added(&self, path: ObjectPath<'_>, received: bool) -> zbus::Result<()>;

    /// Deleted signal
    #[zbus(signal)]
    fn deleted(&self, path: ObjectPath<'_>) -> zbus::Result<()>;

    /// DefaultStorage property
    #[zbus(property)]
    fn default_storage(&self) -> zbus::Result<u32>;

    /// Messages property
    #[zbus(property)]
    fn messages(&self) -> zbus::Result<Vec<OwnedObjectPath>>;

    /// SupportedStorages property
    #[zbus(property)]
    fn supported_storages(&self) -> zbus::Result<Vec<u32>>;
}

#[proxy(
    interface = "org.freedesktop.ModemManager1.Modem",
    default_service = "org.freedesktop.ModemManager1",
    default_path = "/org/freedesktop/ModemManager1/Modem/0"
)]
trait Modem {
    /// Command method
    fn command(&self, cmd: &str, timeout: u32) -> zbus::Result<String>;

    /// CreateBearer method
    fn create_bearer(&self, properties: HashMap<&str, Value<'_>>) -> zbus::Result<OwnedObjectPath>;

    /// DeleteBearer method
    fn delete_bearer(&self, bearer: &ObjectPath<'_>) -> zbus::Result<()>;

    /// Enable method
    fn enable(&self, enable: bool) -> zbus::Result<()>;

    /// FactoryReset method
    fn factory_reset(&self, code: &str) -> zbus::Result<()>;

    /// GetCellInfo method
    fn get_cell_info(&self) -> zbus::Result<Vec<HashMap<String, OwnedValue>>>;

    /// ListBearers method
    fn list_bearers(&self) -> zbus::Result<Vec<OwnedObjectPath>>;

    /// Reset method
    fn reset(&self) -> zbus::Result<()>;

    /// SetCurrentBands method
    fn set_current_bands(&self, bands: &[u32]) -> zbus::Result<()>;

    /// SetCurrentCapabilities method
    fn set_current_capabilities(&self, capabilities: u32) -> zbus::Result<()>;

    /// SetCurrentModes method
    fn set_current_modes(&self, modes: &(u32, u32)) -> zbus::Result<()>;

    /// SetPowerState method
    fn set_power_state(&self, state: u32) -> zbus::Result<()>;

    /// SetPrimarySimSlot method
    fn set_primary_sim_slot(&self, sim_slot: u32) -> zbus::Result<()>;

    /// StateChanged signal
    #[zbus(signal)]
    fn state_changed(&self, old: i32, new: i32, reason: u32) -> zbus::Result<()>;

    /// AccessTechnologies property
    #[zbus(property)]
    fn access_technologies(&self) -> zbus::Result<u32>;

    /// Bearers property
    #[zbus(property)]
    fn bearers(&self) -> zbus::Result<Vec<OwnedObjectPath>>;

    /// CarrierConfiguration property
    #[zbus(property)]
    fn carrier_configuration(&self) -> zbus::Result<String>;

    /// CarrierConfigurationRevision property
    #[zbus(property)]
    fn carrier_configuration_revision(&self) -> zbus::Result<String>;

    /// CurrentBands property
    #[zbus(property)]
    fn current_bands(&self) -> zbus::Result<Vec<u32>>;

    /// CurrentCapabilities property
    #[zbus(property)]
    fn current_capabilities(&self) -> zbus::Result<u32>;

    /// CurrentModes property
    #[zbus(property)]
    fn current_modes(&self) -> zbus::Result<(u32, u32)>;

    /// Device property
    #[zbus(property)]
    fn device(&self) -> zbus::Result<String>;

    /// DeviceIdentifier property
    #[zbus(property)]
    fn device_identifier(&self) -> zbus::Result<String>;

    /// Drivers property
    #[zbus(property)]
    fn drivers(&self) -> zbus::Result<Vec<String>>;

    /// EquipmentIdentifier property
    #[zbus(property)]
    fn equipment_identifier(&self) -> zbus::Result<String>;

    /// HardwareRevision property
    #[zbus(property)]
    fn hardware_revision(&self) -> zbus::Result<String>;

    /// Manufacturer property
    #[zbus(property)]
    fn manufacturer(&self) -> zbus::Result<String>;

    /// MaxActiveBearers property
    #[zbus(property)]
    fn max_active_bearers(&self) -> zbus::Result<u32>;

    /// MaxActiveMultiplexedBearers property
    #[zbus(property)]
    fn max_active_multiplexed_bearers(&self) -> zbus::Result<u32>;

    /// MaxBearers property
    #[zbus(property)]
    fn max_bearers(&self) -> zbus::Result<u32>;

    /// Model property
    #[zbus(property)]
    fn model(&self) -> zbus::Result<String>;

    /// OwnNumbers property
    #[zbus(property)]
    fn own_numbers(&self) -> zbus::Result<Vec<String>>;

    /// Plugin property
    #[zbus(property)]
    fn plugin(&self) -> zbus::Result<String>;

    /// Ports property
    #[zbus(property)]
    fn ports(&self) -> zbus::Result<Vec<(String, u32)>>;

    /// PowerState property
    #[zbus(property)]
    fn power_state(&self) -> zbus::Result<u32>;

    /// PrimaryPort property
    #[zbus(property)]
    fn primary_port(&self) -> zbus::Result<String>;

    /// PrimarySimSlot property
    #[zbus(property)]
    fn primary_sim_slot(&self) -> zbus::Result<u32>;

    /// Revision property
    #[zbus(property)]
    fn revision(&self) -> zbus::Result<String>;

    /// SignalQuality property
    #[zbus(property)]
    fn signal_quality(&self) -> zbus::Result<(u32, bool)>;

    /// Sim property
    #[zbus(property)]
    fn sim(&self) -> zbus::Result<OwnedObjectPath>;

    /// SimSlots property
    #[zbus(property)]
    fn sim_slots(&self) -> zbus::Result<Vec<OwnedObjectPath>>;

    /// State property
    #[zbus(property, name = "State")]
    fn modem_state(&self) -> zbus::Result<u32>;

    /// StateFailedReason property
    #[zbus(property)]
    fn state_failed_reason(&self) -> zbus::Result<u32>;

    /// SupportedBands property
    #[zbus(property)]
    fn supported_bands(&self) -> zbus::Result<Vec<u32>>;

    /// SupportedCapabilities property
    #[zbus(property)]
    fn supported_capabilities(&self) -> zbus::Result<Vec<u32>>;

    /// SupportedIpFamilies property
    #[zbus(property)]
    fn supported_ip_families(&self) -> zbus::Result<u32>;

    /// SupportedModes property
    #[zbus(property)]
    fn supported_modes(&self) -> zbus::Result<Vec<(u32, u32)>>;

    /// UnlockRequired property
    #[zbus(property)]
    fn unlock_required(&self) -> zbus::Result<u32>;

    /// UnlockRetries property
    #[zbus(property)]
    fn unlock_retries(&self) -> zbus::Result<HashMap<u32, u32>>;
}

#[proxy(
    interface = "org.freedesktop.ModemManager1.Modem.Time",
    default_service = "org.freedesktop.ModemManager1",
    default_path = "/org/freedesktop/ModemManager1/Modem/0"
)]
trait Time {
    /// GetNetworkTime method
    fn get_network_time(&self) -> zbus::Result<String>;

    /// NetworkTimeChanged signal
    #[zbus(signal)]
    fn network_time_changed(&self, time: &str) -> zbus::Result<()>;

    /// NetworkTimezone property
    #[zbus(property)]
    fn network_timezone(&self) -> zbus::Result<HashMap<String, OwnedValue>>;
}

#[proxy(
    interface = "org.freedesktop.ModemManager1.Modem.Firmware",
    default_service = "org.freedesktop.ModemManager1",
    default_path = "/org/freedesktop/ModemManager1/Modem/0"
)]
trait Firmware {
    /// List method
    fn list(&self) -> zbus::Result<(String, Vec<HashMap<String, OwnedValue>>)>;

    /// Select method
    fn select(&self, uniqueid: &str) -> zbus::Result<()>;

    /// UpdateSettings property
    #[zbus(property)]
    fn update_settings(&self) -> zbus::Result<(u32, HashMap<String, OwnedValue>)>;
}

#[proxy(
    interface = "org.freedesktop.ModemManager1.Modem.Modem3gpp.ProfileManager",
    default_service = "org.freedesktop.ModemManager1",
    default_path = "/org/freedesktop/ModemManager1/Modem/0"
)]
trait ProfileManager {
    /// Delete method
    fn delete(&self, properties: HashMap<&str, Value<'_>>) -> zbus::Result<()>;

    /// List method
    fn list(&self) -> zbus::Result<Vec<HashMap<String, OwnedValue>>>;

    /// Set method
    fn set(
        &self,
        requested_properties: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<HashMap<String, OwnedValue>>;

    /// Updated signal
    #[zbus(signal)]
    fn updated(&self) -> zbus::Result<()>;

    /// IndexField property
    #[zbus(property)]
    fn index_field(&self) -> zbus::Result<String>;
}

#[proxy(
    interface = "org.freedesktop.ModemManager1.Modem.Sar",
    default_service = "org.freedesktop.ModemManager1",
    default_path = "/org/freedesktop/ModemManager1/Modem/0"
)]
trait Sar {
    /// Enable method
    fn enable(&self, enable: bool) -> zbus::Result<()>;

    /// SetPowerLevel method
    fn set_power_level(&self, level: u32) -> zbus::Result<()>;

    /// PowerLevel property
    #[zbus(property)]
    fn power_level(&self) -> zbus::Result<u32>;

    /// State property
    #[zbus(property)]
    fn state(&self) -> zbus::Result<bool>;
}

#[proxy(
    interface = "org.freedesktop.ModemManager1.Modem.Simple",
    default_service = "org.freedesktop.ModemManager1",
    default_path = "/org/freedesktop/ModemManager1/Modem/0"
)]
trait Simple {
    /// Connect method
    fn connect(&self, properties: HashMap<&str, Value<'_>>) -> zbus::Result<OwnedObjectPath>;

    /// Disconnect method
    fn disconnect(&self, bearer: &ObjectPath<'_>) -> zbus::Result<()>;

    /// GetStatus method
    fn get_status(&self) -> zbus::Result<HashMap<String, OwnedValue>>;
}

#[proxy(
    interface = "org.freedesktop.ModemManager1.Modem.Modem3gpp",
    default_service = "org.freedesktop.ModemManager1",
    default_path = "/org/freedesktop/ModemManager1/Modem/0"
)]
trait Modem3gpp {
    /// DisableFacilityLock method
    fn disable_facility_lock(&self, properties: &(u32, &str)) -> zbus::Result<()>;

    /// Register method
    fn register(&self, operator_id: &str) -> zbus::Result<()>;

    /// Scan method
    fn scan(&self) -> zbus::Result<Vec<HashMap<String, OwnedValue>>>;

    /// SetEpsUeModeOperation method
    fn set_eps_ue_mode_operation(&self, mode: u32) -> zbus::Result<()>;

    /// SetInitialEpsBearerSettings method
    fn set_initial_eps_bearer_settings(
        &self,
        settings: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<()>;

    /// SetNr5gRegistrationSettings method
    fn set_nr5g_registration_settings(
        &self,
        properties: HashMap<&str, Value<'_>>,
    ) -> zbus::Result<()>;

    /// SetPacketServiceState method
    fn set_packet_service_state(&self, state: u32) -> zbus::Result<()>;

    /// EnabledFacilityLocks property
    #[zbus(property)]
    fn enabled_facility_locks(&self) -> zbus::Result<u32>;

    /// EpsUeModeOperation property
    #[zbus(property)]
    fn eps_ue_mode_operation(&self) -> zbus::Result<u32>;

    /// Imei property
    #[zbus(property)]
    fn imei(&self) -> zbus::Result<String>;

    /// InitialEpsBearer property
    #[zbus(property)]
    fn initial_eps_bearer(&self) -> zbus::Result<OwnedObjectPath>;

    /// InitialEpsBearerSettings property
    #[zbus(property)]
    fn initial_eps_bearer_settings(&self) -> zbus::Result<HashMap<String, OwnedValue>>;

    /// Nr5gRegistrationSettings property
    #[zbus(property)]
    fn nr5g_registration_settings(&self) -> zbus::Result<HashMap<String, OwnedValue>>;

    /// OperatorCode property
    #[zbus(property)]
    fn operator_code(&self) -> zbus::Result<String>;

    /// OperatorName property
    #[zbus(property)]
    fn operator_name(&self) -> zbus::Result<String>;

    /// PacketServiceState property
    #[zbus(property)]
    fn packet_service_state(&self) -> zbus::Result<u32>;

    /// Pco property
    #[zbus(property)]
    fn pco(&self) -> zbus::Result<Vec<(u32, bool, Vec<u8>)>>;

    /// RegistrationState property
    #[zbus(property)]
    fn registration_state(&self) -> zbus::Result<u32>;

    /// SubscriptionState property
    #[zbus(property)]
    fn subscription_state(&self) -> zbus::Result<u32>;
}

#[proxy(
    interface = "org.freedesktop.ModemManager1.Modem.Voice",
    default_service = "org.freedesktop.ModemManager1",
    default_path = "/org/freedesktop/ModemManager1/Modem/0"
)]
trait Voice {
    /// CallWaitingQuery method
    fn call_waiting_query(&self) -> zbus::Result<bool>;

    /// CallWaitingSetup method
    fn call_waiting_setup(&self, enable: bool) -> zbus::Result<()>;

    /// CreateCall method
    fn create_call(&self, properties: HashMap<&str, Value<'_>>) -> zbus::Result<OwnedObjectPath>;

    /// DeleteCall method
    fn delete_call(&self, path: &ObjectPath<'_>) -> zbus::Result<()>;

    /// HangupAll method
    fn hangup_all(&self) -> zbus::Result<()>;

    /// HangupAndAccept method
    fn hangup_and_accept(&self) -> zbus::Result<()>;

    /// HoldAndAccept method
    fn hold_and_accept(&self) -> zbus::Result<()>;

    /// ListCalls method
    fn list_calls(&self) -> zbus::Result<Vec<OwnedObjectPath>>;

    /// Transfer method
    fn transfer(&self) -> zbus::Result<()>;

    /// CallAdded signal
    #[zbus(signal)]
    fn call_added(&self, path: ObjectPath<'_>) -> zbus::Result<()>;

    /// CallDeleted signal
    #[zbus(signal)]
    fn call_deleted(&self, path: ObjectPath<'_>) -> zbus::Result<()>;

    /// Calls property
    #[zbus(property)]
    fn calls(&self) -> zbus::Result<Vec<OwnedObjectPath>>;

    /// EmergencyOnly property
    #[zbus(property)]
    fn emergency_only(&self) -> zbus::Result<bool>;
}

// Sources of location information supported by the modem.
#[derive(Type, OwnedValue, PartialEq, Debug, PartialOrd)]
#[repr(u32)]
pub enum ModemLocationSource {
    None = 0,
    // Location Area Code and Cell ID.
    LacCi = 1 << 0,
    // GPS location given by predefined keys.
    GpsRaw = 1 << 1,
    // GPS location given as NMEA traces.
    GpsNmea = 1 << 2,
    // CDMA base station position.
    CdmaBs = 1 << 3,
    // No location given, just GPS module setup. Since 1.4.
    GpsUnmanaged = 1 << 4,
    // Mobile Station Assisted A-GPS location requested. In MSA A-GPS, the position fix is
    // computed by a server online. The modem must have a valid SIM card inserted and be enabled
    // for this mode to be allowed. Since 1.12.
    AgpsMsa = 1 << 5,
    // Mobile Station Based A-GPS location requested. In MSB A-GPS, the position fix is computed
    // by the modem, but it first gathers information from an online server to facilitate the
    // process (e.g. ephemeris). The modem must have a valid SIM card inserted and be enabled for
    // this mode to be allowed. Since 1.12.
    // AgpsMsb = 64,
    AgpsMsb = 1 << 6,
}
