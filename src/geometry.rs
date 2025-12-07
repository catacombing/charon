//! Shared geometry types.

use std::f64::consts::PI;
use std::ops::{Add, AddAssign, Div, Mul, Sub, SubAssign};

use skia_safe::{ISize, Point as SkiaPoint};

use crate::tiles::{TILE_SIZE, TileIndex};

/// 2D object position.
#[derive(PartialEq, Eq, Copy, Clone, Default, Debug)]
pub struct Point<T = i32> {
    pub x: T,
    pub y: T,
}

impl<T> Point<T> {
    pub fn new(x: T, y: T) -> Self {
        Self { x, y }
    }
}

impl<T> From<(T, T)> for Point<T> {
    fn from((x, y): (T, T)) -> Self {
        Self { x, y }
    }
}

impl From<Point<f32>> for Point {
    fn from(point: Point<f32>) -> Self {
        Self { x: point.x.round() as i32, y: point.y.round() as i32 }
    }
}

impl From<Point<f64>> for Point<f32> {
    fn from(point: Point<f64>) -> Self {
        Self::new(point.x as f32, point.y as f32)
    }
}

impl From<Point> for Point<f32> {
    fn from(point: Point) -> Self {
        Self::new(point.x as f32, point.y as f32)
    }
}

impl From<Point> for Point<f64> {
    fn from(point: Point) -> Self {
        Self::new(point.x as f64, point.y as f64)
    }
}

impl From<Point> for SkiaPoint {
    fn from(point: Point) -> Self {
        Self::new(point.x as f32, point.y as f32)
    }
}

impl From<Point<f64>> for SkiaPoint {
    fn from(point: Point<f64>) -> Self {
        Self::new(point.x as f32, point.y as f32)
    }
}

impl From<Point<f32>> for SkiaPoint {
    fn from(point: Point<f32>) -> Self {
        Self::new(point.x, point.y)
    }
}

impl<T: Add<Output = T>> Add<Point<T>> for Point<T> {
    type Output = Self;

    fn add(mut self, other: Point<T>) -> Self {
        self.x = self.x + other.x;
        self.y = self.y + other.y;
        self
    }
}

impl<T: AddAssign> AddAssign<Point<T>> for Point<T> {
    fn add_assign(&mut self, other: Point<T>) {
        self.x += other.x;
        self.y += other.y;
    }
}

impl<T: Sub<Output = T>> Sub<Point<T>> for Point<T> {
    type Output = Self;

    fn sub(mut self, other: Point<T>) -> Self {
        self.x = self.x - other.x;
        self.y = self.y - other.y;
        self
    }
}

impl<T: SubAssign> SubAssign<Point<T>> for Point<T> {
    fn sub_assign(&mut self, other: Point<T>) {
        self.x -= other.x;
        self.y -= other.y;
    }
}

impl Div<f64> for Point {
    type Output = Point;

    fn div(mut self, scale: f64) -> Self {
        self.x = (self.x as f64 / scale).round() as i32;
        self.y = (self.y as f64 / scale).round() as i32;
        self
    }
}

impl Mul<f64> for Point<f64> {
    type Output = Point<f64>;

    fn mul(mut self, scale: f64) -> Self {
        self.x *= scale;
        self.y *= scale;
        self
    }
}

impl Div<f64> for Point<f64> {
    type Output = Point<f64>;

    fn div(mut self, scale: f64) -> Self {
        self.x /= scale;
        self.y /= scale;
        self
    }
}

/// 2D object size.
#[derive(Hash, PartialEq, Eq, Copy, Clone, Default, Debug)]
pub struct Size<T = u32> {
    pub width: T,
    pub height: T,
}

impl<T> Size<T> {
    pub fn new(width: T, height: T) -> Self {
        Self { width, height }
    }
}

impl<T> From<(T, T)> for Size<T> {
    fn from((width, height): (T, T)) -> Self {
        Self { width, height }
    }
}

impl From<Size<f32>> for Size {
    fn from(size: Size<f32>) -> Self {
        Self { width: size.width.round() as u32, height: size.height.round() as u32 }
    }
}

impl From<Size> for Size<i32> {
    fn from(size: Size) -> Self {
        Self { width: size.width as i32, height: size.height as i32 }
    }
}

impl From<Size> for Size<f32> {
    fn from(size: Size) -> Self {
        Self { width: size.width as f32, height: size.height as f32 }
    }
}

impl From<Size> for Size<f64> {
    fn from(size: Size) -> Self {
        Self { width: size.width as f64, height: size.height as f64 }
    }
}

impl From<Size> for ISize {
    fn from(size: Size) -> Self {
        ISize::new(size.width as i32, size.height as i32)
    }
}

impl Mul<f64> for Size {
    type Output = Self;

    fn mul(mut self, scale: f64) -> Self {
        self.width = (self.width as f64 * scale).round() as u32;
        self.height = (self.height as f64 * scale).round() as u32;
        self
    }
}

impl Div<f64> for Size {
    type Output = Self;

    fn div(mut self, scale: f64) -> Self {
        self.width = (self.width as f64 / scale).round() as u32;
        self.height = (self.height as f64 / scale).round() as u32;
        self
    }
}

impl<T: Sub<Output = T>> Sub<Size<T>> for Size<T> {
    type Output = Self;

    fn sub(mut self, other: Self) -> Self {
        self.width = self.width - other.width;
        self.height = self.height - other.height;
        self
    }
}

/// Point in geographical space.
#[derive(PartialEq, Default, Copy, Clone, Debug)]
pub struct GeoPoint {
    pub lat: f64,
    pub lon: f64,
}

impl GeoPoint {
    pub fn new(lat: f64, long: f64) -> Self {
        Self { lon: long, lat }
    }

    /// Get a geographic point from tile index and offset.
    pub fn from_tile(tile: TileIndex, offset: Point) -> Self {
        let x_fract = offset.x as f64 / TILE_SIZE as f64;
        let x = (tile.x as f64 + x_fract) / 2f64.powi(tile.z as i32);
        let y_fract = offset.y as f64 / TILE_SIZE as f64;
        let y = (tile.y as f64 + y_fract) / 2f64.powi(tile.z as i32);

        let lon_rad = (x * 2. - 1.) * PI;
        let lat_mercator = -(y * 2. - 1.) * PI;
        let lat_rad = 2. * lat_mercator.exp().atan() - PI / 2.;

        let lon = lon_rad.to_degrees();
        let lat = lat_rad.to_degrees();

        Self { lat, lon }
    }

    /// Convert this point to a position within a specific tile.
    pub fn tile(&self, zoom: u8) -> (TileIndex, Point) {
        let tile_count = (1 << zoom) as f64;

        // Get the tile's X index and offset.
        let x = tile_count * (self.lon + 180.) / 360.;
        let tile_x = x.floor() as u32;
        let x_offset = (x.fract() * TILE_SIZE as f64).floor() as i32;

        // Get the tile's Y index and offset.
        let lat_rad = self.lat.to_radians();
        let y = tile_count * (1. - (lat_rad.tan() + (1. / lat_rad.cos())).ln() / PI) / 2.;
        let tile_y = y.floor() as u32;
        let y_offset = (y.fract() * TILE_SIZE as f64).floor() as i32;

        let index = TileIndex::new(tile_x, tile_y, zoom);
        let offset = Point::new(x_offset, y_offset);

        (index, offset)
    }
}

/// Check if a rectangle contains a point.
pub fn rect_contains<T>(rect_point: Point<T>, rect_size: Size<T>, point: Point<T>) -> bool
where
    T: PartialOrd + Add<Output = T>,
{
    point.x >= rect_point.x
        && point.y >= rect_point.y
        && point.x < rect_point.x + rect_size.width
        && point.y < rect_point.y + rect_size.height
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_to_tile() {
        let (tile, point) = GeoPoint::new(51.157800, 6.865500).tile(14);
        assert_eq!(tile, TileIndex::new(8504, 5473, 14));
        assert_eq!(point, Point::new(116, 144));

        let (tile, point) = GeoPoint::new(51.16552, 6.8555).tile(14);
        assert_eq!(tile, TileIndex::new(8504, 5473, 14));
        assert_eq!(point, Point::new(0, 0));

        let (tile, point) = GeoPoint::new(51.15867, 6.8665).tile(14);
        assert_eq!(tile, TileIndex::new(8504, 5473, 14));
        assert_eq!(point, Point::new(128, 128));
    }

    #[test]
    fn tile_to_point() {
        let tile = TileIndex::new(8504, 5473, 14);
        let offset = Point::new(116, 144);
        let point = GeoPoint::from_tile(tile, offset);
        assert_eq!(point, GeoPoint::new(51.157815575327035, 6.865425109863281));

        let tile = TileIndex::new(8504, 5473, 14);
        let offset = Point::new(0, 0);
        let point = GeoPoint::from_tile(tile, offset);
        assert_eq!(point, GeoPoint::new(51.16556659836182, 6.85546875));

        let tile = TileIndex::new(8504, 5473, 14);
        let offset = Point::new(128, 128);
        let point = GeoPoint::from_tile(tile, offset);
        assert_eq!(point, GeoPoint::new(51.15867686442365, 6.866455078125));
    }
}
