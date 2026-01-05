//! Geographic region stats.

use std::collections::HashSet;
use std::mem;

use indexmap::IndexMap;
use serde::Serialize;

use crate::modrana::Countries;

/// Data for a geographic region.
#[derive(Serialize, Debug)]
pub struct Region {
    name: String,
    regions: IndexMap<String, Region>,

    valhalla_packages: Vec<String>,
    geocoder_path: Option<String>,
    postal_path: Option<String>,

    // Complete size of this region and all of its children.
    storage_size: u64,
    #[serde(skip)]
    geocoder_size: u64,
    #[serde(skip)]
    valhalla_size: u64,
    #[serde(skip)]
    postal_size: u64,
}

impl Region {
    /// Get the root region of the world.
    pub fn world(modrana: &mut Countries) -> Self {
        let postal_global_size =
            str::parse::<u64>(&modrana.postal_global.postal_global.size).unwrap();

        let mut region = Region {
            name: "World".into(),
            valhalla_packages: Default::default(),
            geocoder_path: Default::default(),
            geocoder_size: Default::default(),
            valhalla_size: Default::default(),
            storage_size: Default::default(),
            postal_path: Default::default(),
            postal_size: Default::default(),
            regions: Default::default(),
        };

        // Convert flat modrana data map to the region tree.
        for (id, modrana_region) in mem::take(&mut modrana.regions) {
            let mut geocoder_region = &mut region;

            // Russia is the only region which has a mismatch between name and id segments,
            // so we just strip the `Europe/` to make it match.
            let mut name = modrana_region.name.as_str();
            if name.starts_with("Europe/Russian Federation") {
                name = name.strip_prefix("Europe/").unwrap();
            }

            for (id, name) in id.split('/').zip(name.split('/')) {
                // Attempt to shorten Polish region names in a reasonable way, since they're
                // exceptionally long and contain multiple formats.
                let name = name.find('(').map(|i| &name[..i]).unwrap_or(name);

                geocoder_region =
                    geocoder_region.regions.entry(id.into()).or_insert_with(|| Region {
                        name: name.into(),
                        valhalla_packages: Default::default(),
                        geocoder_path: Default::default(),
                        geocoder_size: Default::default(),
                        valhalla_size: Default::default(),
                        storage_size: Default::default(),
                        postal_path: Default::default(),
                        postal_size: Default::default(),
                        regions: Default::default(),
                    });
            }

            // Set geocoder URL for this region.
            let geocoder_size = str::parse(&modrana_region.geocoder_nlp.size).unwrap();
            geocoder_region.geocoder_path = Some(modrana_region.geocoder_nlp.path);
            geocoder_region.geocoder_size = geocoder_size;

            // Set valhalla packages for this region.
            let valhalla_size = str::parse(&modrana_region.valhalla.size).unwrap();
            geocoder_region.valhalla_packages = modrana_region.valhalla.packages;
            geocoder_region.valhalla_size = valhalla_size;

            // Set libpostal URL for this region's language.
            let postal_size = str::parse(&modrana_region.postal_country.size).unwrap();
            geocoder_region.postal_path = Some(modrana_region.postal_country.path);
            geocoder_region.postal_size = postal_size;
        }

        // Recursively update storage size and sort regions.
        region.postprocess(postal_global_size);

        region
    }

    /// Process region list into more optimal storage format.
    #[allow(clippy::type_complexity)]
    fn postprocess(
        &mut self,
        postal_global_size: u64,
    ) -> (HashSet<(String, u64)>, HashSet<(String, u64)>, u64) {
        // Handle regions with postal/geocoder/valhalla data available.
        if self.postal_size != 0 && self.geocoder_size != 0 && self.valhalla_size != 0 {
            // Ensure regions are stored in reverse alphabetical order.
            self.regions.sort_unstable_by(|k1, _, k2, _| k2.cmp(k1));

            // Ensure children are updated, even if this region doesn't use that data.
            for region in self.regions.values_mut() {
                region.postprocess(postal_global_size);
            }

            // Update this region's storage size.
            self.storage_size =
                self.geocoder_size + self.valhalla_size + self.postal_size + postal_global_size;

            // Get valhalla package sizes.
            // Since we only get the combined size, we approximate things by dividing it
            // evenly.
            let avg_size = self.valhalla_size / self.valhalla_packages.len() as u64;
            let valhalla_packages =
                self.valhalla_packages.clone().into_iter().map(|p| (p, avg_size)).collect();

            // Return size of this region's postal, valhalla, and geocoder data.
            let mut postal_countries = HashSet::new();
            postal_countries.insert((self.postal_path.clone().unwrap(), self.postal_size));
            return (postal_countries, valhalla_packages, self.geocoder_size);
        }

        // Ensure regions are stored in reverse alphabetical order.
        self.regions.sort_unstable_by(|k1, _, k2, _| k2.cmp(k1));

        // Calculate geocoder and postal size from children.
        let mut valhalla_packages = HashSet::new();
        let mut postal_countries = HashSet::new();
        for region in self.regions.values_mut() {
            // Add geocoder size.
            let (countries, packages, geocoder_size) = region.postprocess(postal_global_size);
            self.geocoder_size += geocoder_size;

            // Add postal size for each new postal country.
            for (country, country_size) in countries {
                if postal_countries.insert((country, country_size)) {
                    self.postal_size += country_size;
                }
            }

            // Add valhalla size for each new package.
            for (package, package_size) in packages {
                if valhalla_packages.insert((package, package_size)) {
                    self.valhalla_size += package_size;
                }
            }
        }

        // Update this node's combined storage size.
        self.storage_size =
            self.geocoder_size + self.valhalla_size + self.postal_size + postal_global_size;

        (postal_countries, valhalla_packages, self.geocoder_size)
    }
}
