//! Modrana download management.

use std::collections::HashMap;
use std::process::Command;

use serde::Deserialize;

/// URL for the modrana countries index.
const MODRANA_URL: &str = "https://data.modrana.org/osm_scout_server/countries_provided.json";

/// Format of the modrana.org countries_provided.json.
#[derive(Deserialize, Debug)]
pub struct Countries {
    // Parse these fields first, since they're not valid regions.
    #[serde(rename = "mapboxgl/global")]
    _mapboxgl_global: serde_json::Value,
    #[serde(rename = "mapboxgl/glyphs")]
    _mapboxgl_glyphs: serde_json::Value,
    #[serde(rename = "mapnik/global")]
    _mapnik_global: serde_json::Value,

    #[serde(rename = "postal/global")]
    pub postal_global: PostalGlobal,
    pub url: Urls,

    #[serde(flatten)]
    pub regions: HashMap<String, Region>,
}

impl Countries {
    pub fn new() -> Self {
        // Get modrana.org data for geocoder_nlp.
        //
        // We use `curl` here instead of reqwest since the latter causes some
        // cross-compilation build issues.
        let output = Command::new("curl").arg(MODRANA_URL).output().unwrap();
        if !output.status.success() {
            panic!("modrana.org data download failed");
        }

        // Parse stdout as json response.
        let response = str::from_utf8(&output.stdout).unwrap();
        serde_json::from_str(response).expect("failed to parse modrana response")
    }
}

/// Base URLs for the modrana data files.
#[derive(Deserialize, Debug)]
pub struct Urls {
    pub postal_country: String,
    pub postal_global: String,
    pub geocoder_nlp: String,
    pub base: String,
}

/// Global postal data path and stats.
#[derive(Deserialize, Debug)]
pub struct PostalGlobal {
    pub postal_global: PostalGlobalData,
}

/// Global postal data path and stats.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct PostalGlobalData {
    pub path: String,
    pub size: String,
}

/// Modrana region data.
#[derive(Deserialize, Debug)]
pub struct Region {
    pub geocoder_nlp: GeocoderRegion,
    pub postal_country: PostalRegion,
    pub name: String,
}

/// Geocoder path and stats for a region.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct GeocoderRegion {
    pub path: String,
    pub size: String,
}

/// Postal path and stats for a region.
#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct PostalRegion {
    pub path: String,
    pub size: String,
}
