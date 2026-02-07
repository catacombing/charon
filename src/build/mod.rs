use std::collections::HashMap;
use std::env;
use std::fs::{self, File};
use std::path::Path;
use std::process::Command;

use gl_generator::{Api, Fallbacks, GlobalGenerator, Profile, Registry};
use serde::Serialize;

use crate::modrana::Countries;
use crate::region::Region;

mod modrana;
mod region;

/// URL of the catacomb tile archive server.
pub const TILE_URL_BASE: &str = "https://catacombing.org/tiles";

fn main() {
    let out_dir = env::var("OUT_DIR").unwrap();
    let out_dir = Path::new(&out_dir);

    let mut file = File::create(out_dir.join("gl_bindings.rs")).unwrap();
    Registry::new(Api::Gles2, (2, 0), Profile::Core, Fallbacks::All, [])
        .write_bindings(GlobalGenerator, &mut file)
        .unwrap();

    let region_data = serde_json::to_string(&Regions::new()).unwrap();
    fs::write(out_dir.join("regions.json"), region_data).unwrap();
}

/// Region data parsed at compile time.
#[derive(Serialize, Debug)]
struct Regions {
    world_region: Region,

    postal_country_base: String,
    postal_global_base: String,
    valhalla_base: String,
    geocoder_base: String,
}

impl Regions {
    fn new() -> Self {
        let mut modrana = Countries::new();
        let tile_sizes = tile_sizes();

        let world_region = Region::world(&mut modrana, &tile_sizes);

        let postal_country_base = format!("{}/{}", modrana.url.base, modrana.url.postal_country);
        let postal_global_base = format!(
            "{}/{}/{}",
            modrana.url.base, modrana.url.postal_global, modrana.postal_global.postal_global.path
        );
        let valhalla_base =
            format!("{}/{}/valhalla/packages", modrana.url.base, modrana.url.valhalla);
        let geocoder_base = format!("{}/{}", modrana.url.base, modrana.url.geocoder_nlp);

        Self { postal_country_base, postal_global_base, valhalla_base, geocoder_base, world_region }
    }
}

/// Load tile sizes from catacomb.org.
pub fn tile_sizes() -> HashMap<String, u64> {
    // We use `curl` here instead of reqwest since the latter causes some
    // cross-compilation build issues.
    let url = format!("{TILE_URL_BASE}/size");
    let output = Command::new("curl").arg(&url).output().unwrap();
    if !output.status.success() {
        panic!("catacombing.org tile index download failed");
    }

    // Parse stdout as json response.
    let response = str::from_utf8(&output.stdout).unwrap();
    serde_json::from_str(response).expect("failed to parse tile index")
}
