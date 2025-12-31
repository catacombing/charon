//! Shared geometry types.

use std::f64::consts::PI;
use std::ops::{Add, AddAssign, Div, Mul, Sub, SubAssign};

use serde::Serialize;
use skia_safe::{ISize, Point as SkiaPoint};

use crate::tiles::{MAX_ZOOM, TILE_SIZE, TileIndex};

/// Earth's circumference at the equator in meters.
const EARTH_EQUATOR: f64 = 40_075_016.686;

/// Width and height of a single pixel in meters at zoom level 0.
const ZERO_PIXEL_SIZE: f64 = EARTH_EQUATOR / TILE_SIZE as f64;

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

impl Mul<f64> for Point {
    type Output = Point;

    fn mul(mut self, scale: f64) -> Self {
        self.x = (self.x as f64 * scale).round() as i32;
        self.y = (self.y as f64 * scale).round() as i32;
        self
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
#[derive(Serialize, PartialEq, Default, Copy, Clone, Debug)]
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

    /// Calculate distance in meters between two points.
    ///
    /// This uses the haversine formula, which is inaccurate due to assuming the
    /// earth is round, but it should suffice for our purposes.
    pub fn distance(&self, other: Self) -> u32 {
        const EARTH_RADIUS: f64 = 6_371_000.;

        let slat = self.lat.to_radians();
        let olat = other.lat.to_radians();
        let delta_lat = (self.lat - other.lat).to_radians();
        let delta_lon = (self.lon - other.lon).to_radians();

        let a = (delta_lat / 2.).sin().powi(2)
            + slat.cos() * olat.cos() * (delta_lon / 2.).sin().powi(2);
        let c = 2. * a.sqrt().atan2((1. - a).sqrt());
        (EARTH_RADIUS * c).round() as u32
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

/// Check if any point of a line lies within a rectangle.
pub fn rect_intersects_line(
    rect_point: Point<f64>,
    rect_size: Size<f64>,
    line_start: Point<f64>,
    line_end: Point<f64>,
) -> bool {
    let max_point = rect_point + Point::new(rect_size.width, rect_size.height);

    // Handle case where entire line lies within the rectangle.
    rect_contains(rect_point, rect_size, line_start)
        // Check all four sides for intersection with the line.
        || lines_intersect(rect_point, Point::new(max_point.x, rect_point.y), line_start, line_end)
        || lines_intersect(rect_point, Point::new(rect_point.x, max_point.y), line_start, line_end)
        || lines_intersect(Point::new(max_point.x, rect_point.y), max_point, line_start, line_end)
        || lines_intersect(Point::new(rect_point.x, max_point.y), max_point, line_start, line_end)
}

/// Check if two lines intersect
fn lines_intersect(
    a_start: Point<f64>,
    a_end: Point<f64>,
    b_start: Point<f64>,
    b_end: Point<f64>,
) -> bool {
    let a_width = a_end.x - a_start.x;
    let a_height = a_end.y - a_start.y;
    let b_width = b_end.x - b_start.x;
    let b_height = b_end.y - b_start.y;

    // Check if lines are parallel.
    let det = a_width * b_height - b_width * a_height;
    if det == 0. {
        // Check if parallel lines intersect.
        return line_contains(a_start, a_end, b_start)
            || line_contains(a_start, a_end, b_end)
            || line_contains(b_start, b_end, a_start);
    }

    // Calculate intersection.
    let idet = 1. / det;
    let s = (-a_height * (a_start.x - b_start.x) + a_width * (a_start.y - b_start.y)) * idet;
    let t = (b_width * (a_start.y - b_start.y) - b_height * (a_start.x - b_start.x)) * idet;

    (0. ..=1.).contains(&s) && (0. ..=1.).contains(&t)
}

/// Check if a line contains a point.
fn line_contains(line_start: Point<f64>, line_end: Point<f64>, point: Point<f64>) -> bool {
    point.x <= line_start.x.max(line_end.x)
        && point.x >= line_start.x.min(line_end.x)
        && point.y <= line_start.y.max(line_end.y)
        && point.y >= line_start.y.min(line_end.y)
}

/// Get pixel size in meters for a certain zoom level and latitude.
pub fn pixel_size(lat: f64, zoom: u8) -> f64 {
    ZERO_PIXEL_SIZE * lat.to_radians().cos() / 2f64.powi(zoom as i32)
}

/// Get zoom level at which a distance in meters fits into a fixed pixel size.
pub fn zoom_for_distance(lat: f64, meters: f64, pixels: f64) -> u8 {
    let zoom = (ZERO_PIXEL_SIZE * lat.to_radians().cos() * pixels / meters).ln() / 2f64.ln();
    zoom.clamp(0., MAX_ZOOM as f64).floor() as u8
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

    #[test]
    fn distance() {
        let a = GeoPoint::new(0., 0.);
        let b = GeoPoint::new(0., 0.);
        assert_eq!(a.distance(b), 0);

        let a = GeoPoint::new(48.8566, 2.3522);
        let b = GeoPoint::new(50.0647, 19.9450);
        assert_eq!(a.distance(b), 1_275_570);
    }

    #[test]
    fn meters_per_pixel() {
        for lat in 0..90 {
            let lat = lat as f64;
            assert_eq!(pixel_size(lat, 0), 156543.0339296875 * lat.to_radians().cos());
            assert_eq!(pixel_size(lat, 9), 305.7481131439209 * lat.to_radians().cos());
            assert_eq!(pixel_size(lat, 18), 0.5971642834842205 * lat.to_radians().cos());
        }
    }

    #[test]
    fn rect_intersection() {
        let rect_point = Point::new(0., 0.);
        let rect_size = Size::new(10., 10.);

        let line_start = Point::new(3., 3.);
        let line_end = Point::new(5., 5.);
        assert!(rect_intersects_line(rect_point, rect_size, line_start, line_end));

        let line_start = Point::new(0., 3.);
        let line_end = Point::new(0., 5.);
        assert!(rect_intersects_line(rect_point, rect_size, line_start, line_end));

        let line_start = Point::new(3., 10.);
        let line_end = Point::new(5., 10.);
        assert!(rect_intersects_line(rect_point, rect_size, line_start, line_end));

        let line_start = Point::new(-5., 3.);
        let line_end = Point::new(15., 3.);
        assert!(rect_intersects_line(rect_point, rect_size, line_start, line_end));

        let line_start = Point::new(3., -5.);
        let line_end = Point::new(3., 15.);
        assert!(rect_intersects_line(rect_point, rect_size, line_start, line_end));

        let line_start = Point::new(-2., 5.);
        let line_end = Point::new(5., 12.);
        assert!(rect_intersects_line(rect_point, rect_size, line_start, line_end));

        let line_start = Point::new(-2., -1.);
        let line_end = Point::new(12., -1.);
        assert!(!rect_intersects_line(rect_point, rect_size, line_start, line_end));

        let line_start = Point::new(12., -2.);
        let line_end = Point::new(12., 12.);
        assert!(!rect_intersects_line(rect_point, rect_size, line_start, line_end));

        let line_start = Point::new(-2., 1.);
        let line_end = Point::new(1., -2.);
        assert!(!rect_intersects_line(rect_point, rect_size, line_start, line_end));
    }

    #[test]
    fn required_zoom() {
        assert_eq!(zoom_for_distance(0., 0.6, 1.), 17);
        assert_eq!(zoom_for_distance(0., 152., 1.), 10);
        assert_eq!(zoom_for_distance(0., 152., 1.), 10);

        assert_eq!(zoom_for_distance(0., 119.44, 100.), 16);
        assert_eq!(zoom_for_distance(0., 119.43, 100.), 17);

        assert_eq!(zoom_for_distance(80., 53., 2.), 10);
    }
}
