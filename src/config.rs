//! Configuration options.

use std::fmt::{self, Display, Formatter};
use std::ops::Deref;
use std::sync::Arc;
use std::time::Duration;

use base64::prelude::*;
use calloop::LoopHandle;
use calloop::channel::{self, Event, Sender};
use configory::EventHandler;
use configory::docgen::{DocType, Docgen, Leaf};
use serde::de::Visitor;
use serde::{Deserialize, Deserializer};
use skia_safe::Color4f;
use tracing::{error, info};

use crate::State;

/// # Charon
///
/// ## Syntax
///
/// Charon's configuration file uses the TOML format. The format's specification
/// can be found at _https://toml.io/en/v1.0.0_.
///
/// ## Location
///
/// Charon doesn't create the configuration file for you, but it looks for one
/// at <br> `${XDG_CONFIG_HOME:-$HOME/.config}/charon/charon.toml`.
///
/// ## Fields
#[derive(Docgen, Deserialize, Default, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// This section documents the `[font]` table.
    pub font: Font,
    /// This section documents the `[color]` table.
    pub colors: Colors,
    /// This section documents the `[tiles]` table.
    pub tiles: Tiles,
    /// This section documents the `[search]` table.
    pub search: Search,
    /// This section documents the `[input]` table.
    pub input: Input,
}

/// Font configuration.
#[derive(Docgen, Deserialize, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct Font {
    /// Font family.
    pub family: Arc<String>,
    /// Font size.
    pub size: f32,
}

impl Default for Font {
    fn default() -> Self {
        Self { family: Arc::new(String::from("sans")), size: 18. }
    }
}

/// Color configuration.
#[derive(Docgen, Deserialize, Copy, Clone, Hash, PartialEq, Eq, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct Colors {
    /// Primary foreground color.
    #[serde(alias = "fg")]
    pub foreground: Color,
    /// Primary background color.
    #[serde(alias = "bg")]
    pub background: Color,
    /// Primary accent color.
    #[serde(alias = "hl")]
    pub highlight: Color,

    /// Alternative foreground color.
    #[serde(alias = "alt_fg")]
    pub alt_foreground: Color,
    /// Alternative background color.
    #[serde(alias = "alt_bg")]
    pub alt_background: Color,
}

impl Default for Colors {
    fn default() -> Self {
        Self {
            foreground: Color::new(255, 255, 255),
            background: Color::new(24, 24, 24),
            highlight: Color::new(117, 42, 42),

            alt_foreground: Color::new(191, 191, 191),
            alt_background: Color::new(40, 40, 40),
        }
    }
}

/// Map tile configuration.
#[derive(Docgen, Deserialize, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct Tiles {
    /// Raster tile server.
    ///
    /// This should be your tile server's URL, using the variables `{x}` and
    /// `{y}` for the tile numbers and `{z}` for the zoom level.
    #[docgen(
        default = "https://tile.jawg.io/c09eed68-abaf-45b9-bed8-8bb2076013d7/{z}/{x}/{y}.png"
    )]
    pub server: Arc<String>,
    /// Maximum number of map tiles cached in memory.
    ///
    /// Tiles average ~100kB, which means 1_000 tiles will take around 100MB of
    /// RAM. A 720x1440p screen fits 18-28 tiles at a time.
    pub max_mem_tiles: usize,
    /// Maximum number of map tiles cached on disk.
    ///
    /// Tiles take on average ~20kB per tile, which means 50_000 tiles will take
    /// around 1GB of disk space.
    ///
    /// Tiles are cached at `${XDG_CACHE_HOME:-$HOME/.cache}/charon/tiles/`.
    pub max_fs_tiles: u32,
    /// Tileserver attribution message.
    pub attribution: Arc<String>,
}

impl Default for Tiles {
    fn default() -> Self {
        // Avoid exposting jawg token to crawlers.
        let url = "https://tile.jawg.io/c09eed68-abaf-45b9-bed8-8bb2076013d7/{z}/{x}/{y}.png";
        let token_bytes = BASE64_STANDARD.decode("P2FjY2Vzcy10b2tlbj1Ydk94aTMxakNtYlRBSDRUcW1zM3RXb\
            EJsUTNBQ1o5cWxTY0NnSkFzVkVLRUNMYk16S3BJeTdRaGtJU1NiWmNs").unwrap();
        let token = str::from_utf8(&token_bytes).unwrap();

        Self {
            server: Arc::new(format!("{url}{token}")),
            attribution: Arc::new(String::from("© JawgMaps © OpenStreetMap")),
            max_mem_tiles: 1_000,
            max_fs_tiles: 50_000,
        }
    }
}

/// Options related to geocoding.
#[derive(Docgen, Deserialize, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct Search {
    /// URL base of the Nominatim geocoding server.
    ///
    /// An empty URL will disable online geocoding.
    pub nominatim_url: Arc<String>,
}

impl Default for Search {
    fn default() -> Self {
        Self { nominatim_url: Arc::new("https://nominatim.openstreetmap.org".into()) }
    }
}

/// Input configuration.
#[derive(Docgen, Deserialize, PartialEq, Copy, Clone, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct Input {
    /// Milliseconds per velocity tick.
    pub velocity_interval: u16,
    /// Percentage of velocity retained each tick.
    pub velocity_friction: f64,

    /// Square of the maximum distance before touch input is considered a drag.
    pub max_tap_distance: f64,
    /// Maximum interval between taps to be considered a double/trible-tap.
    #[docgen(doc_type = "integer (milliseconds)", default = "300")]
    pub max_multi_tap: MillisDuration,
}

impl Default for Input {
    fn default() -> Self {
        Self {
            max_multi_tap: Duration::from_millis(300).into(),
            velocity_friction: 0.85,
            max_tap_distance: 400.,
            velocity_interval: 30,
        }
    }
}

/// RGB color.
#[derive(Copy, Clone, Hash, PartialEq, Eq, Debug)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

impl From<Color> for Color4f {
    fn from(color: Color) -> Self {
        Color4f {
            r: color.r as f32 / 255.,
            g: color.g as f32 / 255.,
            b: color.b as f32 / 255.,
            a: 1.,
        }
    }
}

impl Docgen for Color {
    fn doc_type() -> DocType {
        DocType::Leaf(Leaf::new("color"))
    }

    fn format(&self) -> String {
        format!("\"#{:0>2x}{:0>2x}{:0>2x}\"", self.r, self.g, self.b)
    }
}

/// Deserialize rgb color from a hex string.
impl<'de> Deserialize<'de> for Color {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ColorVisitor;

        impl Visitor<'_> for ColorVisitor {
            type Value = Color;

            fn expecting(&self, f: &mut Formatter<'_>) -> fmt::Result {
                f.write_str("hex color like #ff00ff")
            }

            fn visit_str<E>(self, value: &str) -> Result<Color, E>
            where
                E: serde::de::Error,
            {
                let channels = match value.strip_prefix('#') {
                    Some(channels) => channels,
                    None => {
                        return Err(E::custom(format!("color {value:?} is missing leading '#'")));
                    },
                };

                let digits = channels.len();
                if digits != 6 {
                    let msg = format!("color {value:?} has {digits} digits; expected 6");
                    return Err(E::custom(msg));
                }

                match u32::from_str_radix(channels, 16) {
                    Ok(mut color) => {
                        let b = (color & 0xFF) as u8;
                        color >>= 8;
                        let g = (color & 0xFF) as u8;
                        color >>= 8;
                        let r = color as u8;

                        Ok(Color::new(r, g, b))
                    },
                    Err(_) => Err(E::custom(format!("color {value:?} contains non-hex digits"))),
                }
            }
        }

        deserializer.deserialize_str(ColorVisitor)
    }
}

impl Display for Color {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), fmt::Error> {
        write!(f, "#{:0>2x}{:0>2x}{:0>2x}", self.r, self.g, self.b)
    }
}

/// Config wrapper for millisecond-precision durations.
#[derive(Copy, Clone, Hash, PartialEq, Eq, Debug)]
pub struct MillisDuration(Duration);

impl Deref for MillisDuration {
    type Target = Duration;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'de> Deserialize<'de> for MillisDuration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let ms = u64::deserialize(deserializer)?;
        Ok(Duration::from_millis(ms).into())
    }
}

impl From<Duration> for MillisDuration {
    fn from(duration: Duration) -> Self {
        Self(duration)
    }
}

impl Display for MillisDuration {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), fmt::Error> {
        write!(f, "{}", self.0.as_millis())
    }
}

/// Event handler for configuration manager updates.
pub struct ConfigEventHandler {
    tx: Sender<Config>,
}

impl ConfigEventHandler {
    pub fn new(event_loop: &LoopHandle<'static, State>) -> Self {
        // Create calloop channel to apply config updates.
        let (tx, rx) = channel::channel();
        let _ = event_loop
            .insert_source(rx, |event, _, state| {
                if let Event::Msg(config) = event {
                    state.window.update_config(config);
                }
            })
            .inspect_err(|err| error!("Failed to insert config source: {err}"));

        Self { tx }
    }

    /// Reload the configuration file.
    fn reload_config(&self, config: &configory::Config) {
        info!("Reloading configuration file");

        // Parse config or fall back to the default.
        let parsed = config
            .get::<&str, Config>(&[])
            .inspect_err(|err| error!("Config error: {err}"))
            .ok()
            .flatten()
            .unwrap_or_default();

        // Update the config.
        if let Err(err) = self.tx.send(parsed) {
            error!("Failed to send on config channel: {err}");
        }
    }
}

impl EventHandler for ConfigEventHandler {
    type MessageData = ();

    fn file_changed(&self, config: &configory::Config) {
        self.reload_config(config);
    }

    fn ipc_changed(&self, config: &configory::Config) {
        self.reload_config(config);
    }

    fn file_error(&self, _config: &configory::Config, err: configory::Error) {
        error!("Configuration file error: {err}");
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use configory::docgen::markdown::Markdown;

    use super::*;

    #[test]
    fn config_docs() {
        let mut formatter = Markdown::new();
        formatter.set_heading_size(3);
        let expected = formatter.format::<Config>();

        // Uncomment to update config documentation.
        // fs::write("./docs/config.md", &expected).unwrap();

        // Ensure documentation is up to date.
        let docs = fs::read_to_string("./docs/config.md").unwrap();
        assert_eq!(docs, expected);
    }
}
