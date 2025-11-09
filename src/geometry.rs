//! Shared geometry types.

use std::ops::{Mul, Sub, SubAssign};

use skia_safe::{Point as SkiaPoint, Rect};

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

impl From<Point<f64>> for Point<f32> {
    fn from(point: Point<f64>) -> Self {
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

impl Mul<f64> for Point<f64> {
    type Output = Point<f64>;

    fn mul(mut self, scale: f64) -> Self {
        self.x *= scale;
        self.y *= scale;
        self
    }
}

/// 2D object size.
#[derive(PartialEq, Eq, Copy, Clone, Default, Debug)]
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

impl From<Size> for Size<f32> {
    fn from(size: Size) -> Self {
        Self { width: size.width as f32, height: size.height as f32 }
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

impl<T: Sub<Output = T>> Sub<Size<T>> for Size<T> {
    type Output = Self;

    fn sub(mut self, other: Self) -> Self {
        self.width = self.width - other.width;
        self.height = self.height - other.height;
        self
    }
}

/// Check if a rectangle contains a point.
pub fn rect_contains(rect: Rect, point: Point<f64>) -> bool {
    point.x >= rect.left as f64
        && point.y >= rect.top as f64
        && point.x < rect.right as f64
        && point.y < rect.bottom as f64
}
