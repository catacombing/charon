//! Downloader for offline raster tiles.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::io::{Error as IoError, ErrorKind as IoErrorKind};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use async_compression::Level;
use async_compression::tokio::write::GzipEncoder;
use reqwest::Client;
use tempfile::NamedTempFile;
use tokio::fs::{self, File};
use tokio::time;
use tokio_tar::Builder as TarBuilder;
use tracing::{error, info};

use crate::Error;
use crate::geometry::GeoPoint;
use crate::tiles::TileIndex;

/// Pause duration between map tile downloads.
const REQUEST_INTERVAL: Duration = Duration::from_millis(25);

/// Maximum zoom level for offline tiles.
const MAX_ZOOM: u8 = 16;

/// Download all tiles for a region.
#[allow(unused)]
pub async fn download_region(
    tile_server: &str,
    cache_dir: &Path,
    target_dir: &Path,
    region: &str,
) -> Result<(), Error> {
    let client = crate::http_client()?;

    // Download .poly file from geofabrik.
    let url = format!("https://download.geofabrik.de/{region}.poly");
    let poly_str = client.get(&url).send().await?.error_for_status()?.text().await?;

    // Parse polygon file.
    let polygon = Polygon::from_str(&poly_str)?;

    // Ensure output directories exist.
    fs::create_dir_all(target_dir).await?;
    fs::create_dir_all(cache_dir).await?;

    // Create compressed output tar file archive.
    let filename = target_dir.join("tiles.tar.gz");
    let mut file = File::create(&filename).await?;
    let mut gz = GzipEncoder::with_quality(file, Level::Best);
    let mut tar = TarBuilder::new(gz);

    let mut total_size = 0;

    for z in 0..=MAX_ZOOM {
        // Skip downloading second highest tile level.
        //
        // This tile level is skipped, since it is the only level that significantly
        // impacts total tile count while also being an optional resolution step. For
        // rendering, this tile level can simply fall back to the next higher or lower
        // resolution.
        if z == MAX_ZOOM.saturating_sub(1) {
            continue;
        }

        for tile in PolygonTileIter::new(z, &polygon.points) {
            // Download file to cache dir if it is missing.
            let filename = format!("{}_{}_{}.png", tile.z, tile.x, tile.y);
            let cache_path = cache_dir.join(&filename);
            if !cache_path.exists()
                && let Err(err) = download_tile(&client, tile_server, &cache_path, tile).await
            {
                error!("Tile download failed: {err}");
            }

            // Get file size.
            let file_size = match fs::metadata(&cache_path).await {
                Ok(metadata) => metadata.size(),
                Err(err) => {
                    error!("Failed to read file metadata for {cache_path:?}: {err}");
                    continue;
                },
            };

            // Copy file to the archive.
            match tar.append_path_with_name(&cache_path, &filename).await {
                Ok(_) => info!("Successfully added tile {tile:?}"),
                Err(err) => error!("Failed to add tile {tile:?}: {err}"),
            }

            total_size += file_size;
        }
    }

    // Finish writing tar archive.
    tar.into_inner().await?;

    // Write uncompressed tar archive size.
    let size_path = target_dir.join("size");
    fs::write(size_path, total_size.to_string().as_bytes()).await?;

    Ok(())
}

/// Download a tile to the filesystem cache.
async fn download_tile(
    client: &Client,
    tile_server: &str,
    file_path: &Path,
    tile: TileIndex,
) -> Result<(), Error> {
    info!("Downloading tile {tile:?}");

    // Ensure we don't run into rate limiting.
    time::sleep(REQUEST_INTERVAL).await;

    // Download tile from server.
    let url = tile_server
        .replace("{x}", &tile.x.to_string())
        .replace("{y}", &tile.y.to_string())
        .replace("{z}", &tile.z.to_string());
    let response = client.get(&url).send().await?.error_for_status()?;
    let data = response.bytes().await?;

    // Atomically write tile image to the target directory.
    let target_dir = file_path.parent().ok_or(Error::UnexpectedRoot)?;
    let tempfile = NamedTempFile::new_in(target_dir)?;
    fs::write(&tempfile, &data).await?;
    tempfile.persist(file_path)?;

    Ok(())
}

/// Iterator over tile indices inside a polygon.
#[derive(Default)]
struct PolygonTileIter {
    tiles: Vec<(u32, u32, u32)>,
    zoom: u8,

    y_index: usize,
    x_index: u32,
}

impl PolygonTileIter {
    fn new(zoom: u8, polygon: &[GeoPoint]) -> Self {
        // Range of min..=max X coordinates for each tile Y coordinate.
        let mut tiles = HashMap::new();

        // Use a tile to update a row's min/max X coordinate.
        let mut insert_tile = |tile: TileIndex| match tiles.entry(tile.y) {
            Entry::Vacant(entry) => _ = entry.insert((tile.x, tile.x)),
            Entry::Occupied(mut entry) => {
                let (min, max) = entry.get_mut();
                *min = (*min).min(tile.x);
                *max = (*max).max(tile.x);
            },
        };

        // Tile index of the previously visited polygon.
        let mut last_tile = match polygon.first() {
            Some(first) => {
                let (tile, _) = first.tile(zoom);
                insert_tile(tile);
                tile
            },
            None => return Self::default(),
        };

        // Convert polygons to a map containing the minimum and maximum tile X
        // coordinate for each tile Y coordinate.
        //
        // This is done by repeating the following steps for each line in the polygon:
        //   1. Convert line's origin/target to the tile indices they are within
        //   2. Find tiles which could be intersected by any line between these tiles
        //   3. Push min/max X coordinate for each Y coordinate in this tile range
        //
        // While calculating just the tiles intersected by the line between origin and
        // target point would lead to fewer tiles, it is likely marginal and not worth
        // the extra complexity.
        for point in polygon.iter().skip(1) {
            let (tile, _) = point.tile(zoom);

            // Sort tiles by y index, to simplify math.
            let (min_tile, max_tile) =
                if tile.y > last_tile.y { (last_tile, tile) } else { (tile, last_tile) };

            // Short circuit if row did not change.
            let y_delta = max_tile.y - min_tile.y;
            if y_delta == 0 {
                insert_tile(tile);
                continue;
            }

            // Calculate increments between each tile row.
            let x_step = (max_tile.x as f64 - min_tile.x as f64) / y_delta as f64;
            let x_step_signum = x_step.signum();
            let (min_x, max_x) = if tile.x > last_tile.x {
                (last_tile.x as i32, tile.x as i32)
            } else {
                (tile.x as i32, last_tile.x as i32)
            };

            // Add tiles which are intersected by any line between this and the last tile.
            for i in 0..=y_delta {
                let y = min_tile.y + i;

                // Calculate minimum and maximum tile X index in this row.
                let min_delta_x = (x_step * (i as f64 - x_step_signum)).floor() as i32;
                let min_x = (min_tile.x as i32 + min_delta_x).max(min_x) as u32;
                let max_delta_x = (x_step * (i as f64 + x_step_signum)).ceil() as i32;
                let max_x = (min_tile.x as i32 + max_delta_x).min(max_x) as u32;

                insert_tile(TileIndex::new(min_x, y, zoom));
                insert_tile(TileIndex::new(max_x, y, zoom));
            }

            last_tile = tile;
        }

        let mut tiles: Vec<_> =
            tiles.into_iter().map(|(y, (min_x, max_x))| (y, min_x, max_x)).collect();
        tiles.sort_unstable();

        Self { tiles, zoom, y_index: 0, x_index: 0 }
    }
}

impl Iterator for PolygonTileIter {
    type Item = TileIndex;

    fn next(&mut self) -> Option<TileIndex> {
        let &(y, min_x, max_x) = self.tiles.get(self.y_index)?;
        let tile = TileIndex::new(min_x + self.x_index, y, self.zoom);

        if min_x + self.x_index >= max_x {
            self.y_index += 1;
            self.x_index = 0;
        } else {
            self.x_index += 1;
        }

        Some(tile)
    }
}

/// Polygon format used by geofabrik.
struct Polygon {
    points: Vec<GeoPoint>,
}

impl FromStr for Polygon {
    type Err = IoError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut points = Vec::new();
        for line in s.lines().skip(2) {
            let line = line.trim();
            if line == "END" {
                break;
            }

            let (lon, lat) = line.split_once("   ").ok_or_else(|| {
                IoError::new(
                    IoErrorKind::InvalidInput,
                    format!("invalid point in polygon: {line:?}"),
                )
            })?;
            let lon = f64::from_str(lon).map_err(|err| {
                IoError::new(IoErrorKind::InvalidInput, format!("invalid longitude {lon:?}: {err}"))
            })?;
            let lat = f64::from_str(lat).map_err(|err| {
                IoError::new(IoErrorKind::InvalidInput, format!("invalid latitude {lat:?}: {err}"))
            })?;

            points.push(GeoPoint::new(lat, lon));
        }

        Ok(Self { points })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polygon_to_tiles_square_poly_single_tile() {
        let polygon = [
            GeoPoint::new(50.9433676, 6.9443464),
            GeoPoint::new(50.9433135, 6.9528866),
            GeoPoint::new(50.9386353, 6.9529724),
            GeoPoint::new(50.938446, 6.9445181),
            // Close the loop.
            GeoPoint::new(50.9433676, 6.9443464),
        ];

        let mut iter = PolygonTileIter::new(15, &polygon);

        let index = TileIndex::new(17016, 10978, 15);
        assert_eq!(iter.next(), Some(index));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn polygon_to_tiles_square_poly_multi_tile() {
        let polygon = [
            GeoPoint::new(51.2172606, 6.7505407),
            GeoPoint::new(51.2169918, 6.7622137),
            GeoPoint::new(51.2102441, 6.7502832),
            GeoPoint::new(51.2102172, 6.7617846),
            // Close the loop.
            GeoPoint::new(51.2172606, 6.7505407),
        ];

        let mut iter = PolygonTileIter::new(15, &polygon);

        let index = TileIndex::new(16998, 10938, 15);
        assert_eq!(iter.next(), Some(index));
        let index = TileIndex::new(16999, 10938, 15);
        assert_eq!(iter.next(), Some(index));
        let index = TileIndex::new(16998, 10939, 15);
        assert_eq!(iter.next(), Some(index));
        let index = TileIndex::new(16999, 10939, 15);
        assert_eq!(iter.next(), Some(index));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn polygon_to_tiles_diamond_poly() {
        let polygon = [
            GeoPoint::new(51.2058348, 6.7619562),
            GeoPoint::new(51.2101904, 6.7540169),
            GeoPoint::new(51.2147068, 6.7619133),
            GeoPoint::new(51.2104592, 6.7686939),
            // Close the loop.
            GeoPoint::new(51.2058348, 6.7619562),
        ];

        let mut iter = PolygonTileIter::new(15, &polygon);

        let index = TileIndex::new(16998, 10938, 15);
        assert_eq!(iter.next(), Some(index));
        let index = TileIndex::new(16999, 10938, 15);
        assert_eq!(iter.next(), Some(index));
        let index = TileIndex::new(17000, 10938, 15);
        assert_eq!(iter.next(), Some(index));

        let index = TileIndex::new(16998, 10939, 15);
        assert_eq!(iter.next(), Some(index));
        let index = TileIndex::new(16999, 10939, 15);
        assert_eq!(iter.next(), Some(index));
        let index = TileIndex::new(17000, 10939, 15);
        assert_eq!(iter.next(), Some(index));

        let index = TileIndex::new(16998, 10940, 15);
        assert_eq!(iter.next(), Some(index));
        let index = TileIndex::new(16999, 10940, 15);
        assert_eq!(iter.next(), Some(index));
        let index = TileIndex::new(17000, 10940, 15);
        assert_eq!(iter.next(), Some(index));

        assert_eq!(iter.next(), None);
    }

    #[test]
    fn polygon_to_tiles_complex_poly() {
        let polygon = [
            GeoPoint::new(51.483521, -0.115056),
            GeoPoint::new(51.4707446, -0.1343679),
            GeoPoint::new(51.4627776, -0.1309347),
            GeoPoint::new(51.4646491, -0.0838566),
            GeoPoint::new(51.4685257, -0.0928688),
            GeoPoint::new(51.4818907, -0.0924826),
            GeoPoint::new(51.4861399, -0.0809813),
            // Close the loop.
            GeoPoint::new(51.483521, -0.115056),
        ];

        let mut iter = PolygonTileIter::new(15, &polygon);

        let index = TileIndex::new(16372, 10899, 15);
        assert_eq!(iter.next(), Some(index));
        let index = TileIndex::new(16373, 10899, 15);
        assert_eq!(iter.next(), Some(index));
        let index = TileIndex::new(16374, 10899, 15);
        assert_eq!(iter.next(), Some(index));
        let index = TileIndex::new(16375, 10899, 15);
        assert_eq!(iter.next(), Some(index));
        let index = TileIndex::new(16376, 10899, 15);
        assert_eq!(iter.next(), Some(index));

        let index = TileIndex::new(16371, 10900, 15);
        assert_eq!(iter.next(), Some(index));
        let index = TileIndex::new(16372, 10900, 15);
        assert_eq!(iter.next(), Some(index));
        let index = TileIndex::new(16373, 10900, 15);
        assert_eq!(iter.next(), Some(index));
        let index = TileIndex::new(16374, 10900, 15);
        assert_eq!(iter.next(), Some(index));
        let index = TileIndex::new(16375, 10900, 15);
        assert_eq!(iter.next(), Some(index));

        let index = TileIndex::new(16371, 10901, 15);
        assert_eq!(iter.next(), Some(index));
        let index = TileIndex::new(16372, 10901, 15);
        assert_eq!(iter.next(), Some(index));
        let index = TileIndex::new(16373, 10901, 15);
        assert_eq!(iter.next(), Some(index));
        let index = TileIndex::new(16374, 10901, 15);
        assert_eq!(iter.next(), Some(index));
        let index = TileIndex::new(16375, 10901, 15);
        assert_eq!(iter.next(), Some(index));

        let index = TileIndex::new(16371, 10902, 15);
        assert_eq!(iter.next(), Some(index));
        let index = TileIndex::new(16372, 10902, 15);
        assert_eq!(iter.next(), Some(index));
        let index = TileIndex::new(16373, 10902, 15);
        assert_eq!(iter.next(), Some(index));
        let index = TileIndex::new(16374, 10902, 15);
        assert_eq!(iter.next(), Some(index));
        let index = TileIndex::new(16375, 10902, 15);
        assert_eq!(iter.next(), Some(index));
        let index = TileIndex::new(16376, 10902, 15);
        assert_eq!(iter.next(), Some(index));

        assert_eq!(iter.next(), None);
    }

    #[test]
    fn deserialize_poly() {
        #[rustfmt::skip]
        let poly = r#"none
        1
           6.394689E+00   5.032397E+01
           6.402186E+00   5.032711E+01
           6.399327E+00   5.033692E+01
        END
        END"#;

        let polygon = Polygon::from_str(poly).unwrap();

        assert_eq!(polygon.points, vec![
            GeoPoint::new(50.32397, 6.394689),
            GeoPoint::new(50.32711, 6.402186),
            GeoPoint::new(50.33692, 6.399327),
        ]);
    }
}
