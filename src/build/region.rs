//! Geographic region stats.

use std::collections::{HashMap, HashSet};
use std::mem;

use indexmap::IndexMap;
use serde::Serialize;

use crate::TILE_URL_BASE;
use crate::modrana::Countries;

/// Fixed mapping between region paths and unique IDs.
const REGION_IDS: &str = include_str!("./region_ids.json");

/// Data for a geographic region.
#[derive(Serialize, Debug)]
pub struct Region {
    id: u32,
    name: String,
    regions: IndexMap<String, Region>,

    valhalla_packages: Vec<String>,
    geocoder_path: Option<String>,
    postal_path: Option<String>,
    tiles_url: Option<String>,

    // Complete size of this region and all of its children.
    storage_size: u64,
    tiles_size: u64,
    #[serde(skip)]
    geocoder_size: u64,
    #[serde(skip)]
    valhalla_size: u64,
    #[serde(skip)]
    postal_size: u64,
}

impl Region {
    /// Get the root region of the world.
    pub fn world(modrana: &mut Countries, tile_sizes: &HashMap<String, u64>) -> Self {
        let postal_global_size =
            str::parse::<u64>(&modrana.postal_global.postal_global.size).unwrap();

        let region_ids: HashMap<String, u32> = serde_json::from_str(REGION_IDS).unwrap();

        let mut world = Region {
            name: "World".into(),
            id: 0,
            valhalla_packages: Default::default(),
            geocoder_path: Default::default(),
            geocoder_size: Default::default(),
            valhalla_size: Default::default(),
            storage_size: Default::default(),
            postal_path: Default::default(),
            postal_size: Default::default(),
            tiles_size: Default::default(),
            tiles_url: Default::default(),
            regions: Default::default(),
        };

        // Convert flat modrana data map to the region tree.
        for (id, modrana_region) in mem::take(&mut modrana.regions) {
            let mut region = &mut world;

            // Russia is the only region which has a mismatch between name and id segments,
            // so we just strip the `Europe/` to make it match.
            let mut name = modrana_region.name.as_str();
            if name.starts_with("Europe/Russian Federation") {
                name = name.strip_prefix("Europe/").unwrap();
            }

            for (relative_id, name) in id.split('/').zip(name.split('/')) {
                // Attempt to shorten Polish region names in a reasonable way, since they're
                // exceptionally long and contain multiple formats.
                let name = name.find('(').map(|i| &name[..i]).unwrap_or(name);

                region = region.regions.entry(relative_id.into()).or_insert_with(|| {
                    // Get region's numerical ID from our static map.
                    let absolute_id = &id[..id.find(relative_id).unwrap() + relative_id.len()];
                    let id = *region_ids
                        .get(absolute_id)
                        .unwrap_or_else(|| panic!("missing region id for {relative_id:?}"));

                    Region {
                        id,
                        name: name.into(),
                        valhalla_packages: Default::default(),
                        geocoder_path: Default::default(),
                        geocoder_size: Default::default(),
                        valhalla_size: Default::default(),
                        storage_size: Default::default(),
                        postal_path: Default::default(),
                        postal_size: Default::default(),
                        tiles_size: Default::default(),
                        tiles_url: Default::default(),
                        regions: Default::default(),
                    }
                });
            }

            // Set geocoder URL for this region.
            let geocoder_size = str::parse(&modrana_region.geocoder_nlp.size).unwrap();
            region.geocoder_path = Some(modrana_region.geocoder_nlp.path);
            region.geocoder_size = geocoder_size;

            // Set valhalla packages for this region.
            let valhalla_size = str::parse(&modrana_region.valhalla.size).unwrap();
            region.valhalla_packages = modrana_region.valhalla.packages;
            region.valhalla_size = valhalla_size;

            // Set libpostal URL for this region's language.
            let postal_size = str::parse(&modrana_region.postal_country.size).unwrap();
            region.postal_path = Some(modrana_region.postal_country.path);
            region.postal_size = postal_size;

            // Set tile data for this region.
            if let Some(tile_size) = tile_sizes.get(&id) {
                region.tiles_url = Some(format!("{TILE_URL_BASE}/{id}/tiles.tar.gz"));
                region.tiles_size += tile_size;
            }
        }

        // Recursively update storage size and sort regions.
        world.postprocess(postal_global_size);

        world
    }

    /// Process region list into more optimal storage format.
    #[allow(clippy::type_complexity)]
    fn postprocess(
        &mut self,
        postal_global_size: u64,
    ) -> (HashSet<(String, u64)>, HashSet<(String, u64)>, u64, u64) {
        // Ensure regions are stored in reverse alphabetical order.
        self.regions.sort_unstable_by(|k1, _, k2, _| k2.cmp(k1));

        let has_valhalla = self.valhalla_size != 0;
        let has_geocoder = self.geocoder_size != 0;
        let has_postal = self.postal_size != 0;
        let has_tiles = self.tiles_url.is_some();

        let mut valhalla_packages = HashSet::new();
        let mut postal_countries = HashSet::new();

        // Calculate geocoder and postal size from children.
        for region in self.regions.values_mut() {
            // Get subregion sizes.
            let (countries, packages, tile_size, geocoder_size) =
                region.postprocess(postal_global_size);

            if !has_tiles {
                self.tiles_size += tile_size;
            }

            if !has_geocoder {
                self.geocoder_size += geocoder_size;
            }

            // Add postal size for each new postal country.
            if !has_postal {
                for (country, country_size) in countries {
                    if postal_countries.insert((country, country_size)) {
                        self.postal_size += country_size;
                    }
                }
            }

            // Add valhalla size for each new package.
            if !has_valhalla {
                for (package, package_size) in packages {
                    if valhalla_packages.insert((package, package_size)) {
                        self.valhalla_size += package_size;
                    }
                }
            }
        }

        // Get valhalla package sizes.
        //
        // Since we only have the combined size, we approximate by dividing it evenly.
        if has_valhalla {
            let avg_size = self.valhalla_size / self.valhalla_packages.len() as u64;
            valhalla_packages =
                self.valhalla_packages.clone().into_iter().map(|p| (p, avg_size)).collect();
        }

        // Return size of this region's postal, valhalla, tile, and geocoder data.
        if has_postal {
            postal_countries.insert((self.postal_path.clone().unwrap(), self.postal_size));
        }

        // Update this node's combined storage size.
        self.storage_size = self.geocoder_size
            + self.valhalla_size
            + self.postal_size
            + self.tiles_size
            + postal_global_size;

        (postal_countries, valhalla_packages, self.tiles_size, self.geocoder_size)
    }
}
