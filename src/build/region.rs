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

    geocoder_path: Option<String>,
    postal_path: Option<String>,

    // Complete size of this region and all of its children.
    storage_size: u64,
    #[serde(skip)]
    geocoder_size: u64,
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
            geocoder_path: Default::default(),
            geocoder_size: Default::default(),
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
                        geocoder_path: Default::default(),
                        geocoder_size: Default::default(),
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
    fn postprocess(&mut self, postal_global_size: u64) -> (HashSet<(String, u64)>, u64) {
        // Handle regions with postal/geocoder data available.
        if self.postal_size != 0 && self.geocoder_size != 0 {
            // Ensure regions are stored in reverse alphabetical order.
            self.regions.sort_unstable_by(|k1, _, k2, _| k2.cmp(k1));

            // Ensure children are updated, even if this region doesn't use that data.
            for region in self.regions.values_mut() {
                region.postprocess(postal_global_size);
            }

            // Update this region's storage size.
            self.storage_size = self.geocoder_size + self.postal_size + postal_global_size;

            // Return size of this region's postal and geocoder data.
            let mut postal_countries = HashSet::new();
            postal_countries.insert((self.postal_path.clone().unwrap(), self.postal_size));
            return (postal_countries, self.geocoder_size);
        }

        // Ensure regions are stored in reverse alphabetical order.
        self.regions.sort_unstable_by(|k1, _, k2, _| k2.cmp(k1));

        // Calculate geocoder and postal size from children.
        let mut postal_countries = HashSet::new();
        for region in self.regions.values_mut() {
            // Add geocoder size.
            let (countries, geocoder_size) = region.postprocess(postal_global_size);
            self.geocoder_size += geocoder_size;

            // Add postal size for each new postal country.
            for (country, country_size) in countries {
                if postal_countries.insert((country, country_size)) {
                    self.postal_size += country_size;
                }
            }
        }

        // Update this node's combined storage size.
        self.storage_size = self.geocoder_size + self.postal_size + postal_global_size;

        (postal_countries, self.geocoder_size)
    }
}
