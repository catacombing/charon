//! GeoJSON parser.
//!
//! See <https://datatracker.ietf.org/doc/html/rfc7946>.

// Partial spec implementation doesn't make sense. Since nothing else is in this
// module anyway, we can just ignore unused warnings.
#![allow(unused)]

use serde::Deserialize;

/// GeoJSON root object.
#[derive(Deserialize)]
#[serde(tag = "type")]
pub enum GeoJson<P> {
    FeatureCollection(FeatureCollection<P>),
    Feature(Feature<P>),
    #[serde(untagged)]
    Geometry(Geometry),
}

/// GeoJSON feature collection.
#[derive(Deserialize)]
pub struct FeatureCollection<P> {
    pub features: Vec<Feature<P>>,
    #[serde(default)]
    pub bbox: Vec<f64>,
}

/// GeoJSON feature.
#[derive(Deserialize)]
pub struct Feature<P> {
    pub id: Option<FeatureId>,
    pub geometry: Option<Geometry>,
    pub properties: Option<P>,
    #[serde(default)]
    pub bbox: Vec<f64>,
}

/// GeoJSON feature ID.
#[derive(Deserialize)]
#[serde(untagged)]
pub enum FeatureId {
    String(String),
    Integer(i64),
    Float(f64),
}

/// GeoJSON geometry.
#[derive(Deserialize)]
#[serde(tag = "type")]
pub enum Geometry {
    GeometryCollection(GeometryCollection),
    Point(Coordinate1),
    MultiPoint(Coordinate2),
    LineString(Coordinate2),
    MultiLineString(Coordinate3),
    Polygon(Coordinate3),
    MultiPolygon(Coordinate4),
}

/// GeoJSON geometry collection.
#[derive(Deserialize)]
pub struct GeometryCollection {
    pub geometries: Vec<Geometry>,
    #[serde(default)]
    pub bbox: Vec<f64>,
}

/// GeoJSON coordinate point.
#[derive(Deserialize)]
pub struct Coordinate1 {
    pub coordinates: Vec<f64>,
    #[serde(default)]
    pub bbox: Vec<f64>,
}

/// GeoJSON list of coordinate points.
#[derive(Deserialize)]
pub struct Coordinate2 {
    pub coordinates: Vec<Vec<f64>>,
    #[serde(default)]
    pub bbox: Vec<f64>,
}

/// GeoJSON list of lists of coordinate points.
#[derive(Deserialize)]
pub struct Coordinate3 {
    pub coordinates: Vec<Vec<Vec<f64>>>,
    #[serde(default)]
    pub bbox: Vec<f64>,
}

/// GeoJSON list of lists of lists of coordinate points.
#[derive(Deserialize)]
pub struct Coordinate4 {
    pub coordinates: Vec<Vec<Vec<Vec<f64>>>>,
    #[serde(default)]
    pub bbox: Vec<f64>,
}
