//! Protobuf mapbox vector tile deserialization.
//!
//! See <https://github.com/mapbox/vector-tile-spec/tree/master/2.1>.
//! See <https://shortbread-tiles.org/schema/1.0>.

use prost::{Enumeration, Message};

/// Vector tile data.
#[derive(Clone, PartialEq, Message)]
pub struct Tile {
    #[prost(message, repeated, tag = "3")]
    pub layers: Vec<Layer>,
}

/// Tile layer.
///
/// See <https://github.com/mapbox/vector-tile-spec/tree/master/2.1#41-layers>.
#[derive(Clone, PartialEq, Message)]
pub struct Layer {
    /// Vector tile specification version used by this layer.
    #[prost(uint32, required, tag = "15", default = "1")]
    pub version: u32,
    /// Unique layer identifier.
    #[prost(string, required, tag = "1")]
    pub name: String,
    /// The features in this tile.
    #[prost(message, repeated, tag = "2")]
    pub features: Vec<Feature>,
    /// Tag keys used by the layer's features.
    #[prost(string, repeated, tag = "3")]
    pub keys: Vec<String>,
    /// Tag values used by the layer's features.
    #[prost(message, repeated, tag = "4")]
    pub values: Vec<Value>,
    /// Inclusive width and height of the layer's coordinate system.
    #[prost(uint32, tag = "5", default = "4096")]
    pub extent: u32,
}

/// Layer features.
///
/// See <https://github.com/mapbox/vector-tile-spec/tree/master/2.1#42-features>.
#[derive(Clone, PartialEq, Eq, Hash, Message)]
pub struct Feature {
    /// Unique feature identifier.
    #[prost(uint64, optional, tag = "1", default = "0")]
    pub id: Option<u64>,
    /// Feature tags are consecutive pairs of keys and values indexing into
    /// [`Layer::keys`] and [`Layer::values`].
    ///
    /// See <https://github.com/mapbox/vector-tile-spec/tree/master/2.1#44-feature-attributes>.
    #[prost(uint32, repeated, tag = "2")]
    pub tags: Vec<u32>,
    /// The type of geometry stored in this feature.
    #[prost(enumeration = "GeomType", optional, tag = "3", default = "Unknown")]
    pub r#type: Option<i32>,
    /// Contains a stream of commands and parameters.
    ///
    /// See <https://github.com/mapbox/vector-tile-spec/tree/master/2.1#43-geometry-encoding>.
    #[prost(uint32, repeated, tag = "4")]
    pub geometry: Vec<u32>,
}

/// Types of geometry for a feature.
///
/// See <https://github.com/mapbox/vector-tile-spec/tree/master/2.1#434-geometry-types>.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Enumeration)]
#[repr(i32)]
pub enum GeomType {
    Unknown = 0,
    Point = 1,
    Linestring = 2,
    Polygon = 3,
}

/// Feature tag's value.
///
/// Exactly one of these values must be present in a valid message.
///
/// See <https://github.com/mapbox/vector-tile-spec/tree/master/2.1#41-layers>.
#[derive(Clone, PartialEq, Message)]
pub struct Value {
    #[prost(string, optional, tag = "1")]
    pub string_value: Option<String>,
    #[prost(float, optional, tag = "2")]
    pub float_value: Option<f32>,
    #[prost(double, optional, tag = "3")]
    pub double_value: Option<f64>,
    #[prost(int64, optional, tag = "4")]
    pub int_value: Option<i64>,
    #[prost(uint64, optional, tag = "5")]
    pub uint_value: Option<u64>,
    #[prost(sint64, optional, tag = "6")]
    pub sint_value: Option<i64>,
    #[prost(bool, optional, tag = "7")]
    pub bool_value: Option<bool>,
}
