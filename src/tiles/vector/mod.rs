//! Mapbox vector tile rendering.

use std::cmp::Ordering;
use std::mem;
use std::ops::{Deref, DerefMut};
use std::str::FromStr;

use skia_safe::{
    Canvas, Color4f, Font, Paint, PathBuilder, PathEffect, PathFillType, Rect, TextBlobBuilder,
};
use tracing::{debug, error};

use crate::config::Color;
use crate::geometry::{Point, Size};
use crate::tiles::vector::protobuf::{Feature, GeomType, Layer as ProtobufLayer, Tile};
use crate::ui::skia::RenderState;

// TODO: Do we want this public? If not we need to expose a way to decode tiles.
pub mod protobuf;

// TODO: Next up:
//  - Labels
//  - Fix road merges/diverges
//  - POIs

/// Dim factor for polygon/line outline colors.
const OUTLINE_DIM_FACTOR: f32 = 0.8;

/// Dim factor for tunnels.
const TUNNEL_DIM_FACTOR: f32 = 0.8;

/// Skia vector tile renderer.
pub struct SkiaRenderer {
    pub theme: Theme,

    geometry_type: Option<GeometryType>,
    path: PathBuilder,
    paint: Paint,
    extent: f32,
    font: Font,
}

impl SkiaRenderer {
    pub fn new(theme: Theme) -> Self {
        // Create path used for all rendering operations.
        //
        // `EvenOdd` fill type automatically takes care of the winding order of exterior
        // and interior polygons. Since no two interior polygons are allowed to overlap,
        // every overlapping polygon can be automatically treated as internal and
        // rendered as a "cutout".
        //
        // While this path is reused, no path is ever rendered twice, so setting the
        // `volatile` flag should improve performance.
        let mut path = PathBuilder::new();
        path.set_fill_type(PathFillType::EvenOdd);
        path.set_is_volatile(true);

        // Enable anti-aliasing for all drawing. While this has shown a severe
        // performance impact in other areas, it somehow seems to improve performance
        // here (???).
        let mut paint = Paint::default();
        paint.set_anti_alias(true);

        // TODO: Testing
        let font_mgr = skia_safe::FontMgr::new();
        let mut font_collection = skia_safe::textlayout::FontCollection::new();
        font_collection.set_default_font_manager(font_mgr.clone(), None);
        let typeface = font_collection.default_fallback().unwrap();
        let font = Font::from_typeface(typeface, 12.);

        Self {
            paint,
            theme,
            path,
            geometry_type: Default::default(),
            extent: Default::default(),
            font,
        }
    }

    /// Render the vector tile to the canvas.
    pub fn render<'a>(&mut self, canvas: &Canvas, point: Point<f32>, size: Size<f32>, tile: Tile) {
        let mut render_state = SkiaRendererState {
            canvas,
            point,
            size,
            geometry_type: &mut self.geometry_type,
            extent: &mut self.extent,
            paint: &mut self.paint,
            path: &mut self.path,
            theme: &self.theme,
            font: &self.font,
            layer: Layer::Ocean,
            tags: Default::default(),
        };
        render_state.render(tile);

        // Ensure the last path is rendered.
        render_state.draw_path();
    }
}

/// State used for drawing vector tiles with [`SkiaRenderer`].
struct SkiaRendererState<'a> {
    geometry_type: &'a mut Option<GeometryType>,
    path: &'a mut PathBuilder,
    paint: &'a mut Paint,
    extent: &'a mut f32,
    canvas: &'a Canvas,
    theme: &'a Theme,
    font: &'a Font,

    point: Point<f32>,
    size: Size<f32>,
    layer: Layer,
    tags: Tags,
}

impl<'a> SkiaRendererState<'a> {
    /// Draw the current path/polygon.
    fn draw_path(&mut self) {
        // Ignore empty paths.
        if self.path.is_empty() {
            return;
        }

        // Skip rendering for types without style.
        let path = self.path.detach();
        let mut style = match self.theme.style(self.layer, &self.tags) {
            Some(style) => style,
            None => return,
        };

        // TODO
        //  => Performance seems fine, text only takes ~40ms
        if self.layer == Layer::StreetLabels
            && let Some(name) = self.tags.name.as_ref()
        {
            let mut glyphs = vec![0; name.len()];
            let glyph_count = self.font.str_to_glyphs(name, &mut glyphs);
            glyphs.truncate(glyph_count);

            let mut points = vec![skia_safe::Point::default(); glyph_count];
            self.font.get_pos(&glyphs, &mut points, None);
            let mut widths = vec![0.; glyph_count];
            self.font.get_widths(&glyphs, &mut widths);

            let ascent = self.font.metrics().1.ascent;
            let descent = self.font.metrics().1.descent;
            let line_height = descent - ascent;

            let mut path_measure = skia_safe::PathMeasure::new(&path, false, None);
            let path_length = path_measure.length();
            let text_length: f32 = widths.iter().sum();
            let x_offset = (path_length - text_length) / 2.;
            if x_offset < 0. {
                // TODO: Ignore paths with not enough space?
                return;
            }

            // TODO: Always flip upright?
            let mut blob_builder = TextBlobBuilder::new();
            let (blob_glyphs, blob_rsx) = blob_builder.alloc_run_rsxform(self.font, glyphs.len());
            for (i, (glyph, &(mut point))) in glyphs.iter().zip(&points).enumerate() {
                // Center text vertically.
                point.y += (descent - ascent) / 2. - descent;

                let glyph_start = x_offset + point.x;
                let (mut path_point, tan) = path_measure.pos_tan(glyph_start).unwrap();

                path_point.x -= point.y * tan.y;
                path_point.y += point.y * tan.x;

                blob_glyphs[i] = *glyph;
                blob_rsx[i].scos = tan.x;
                blob_rsx[i].ssin = tan.y;
                blob_rsx[i].tx = path_point.x;
                blob_rsx[i].ty = path_point.y;
            }

            self.paint.set_stroke(false);
            self.paint.set_color4f(Color4f::new(1., 0., 1., 1.), None);
            let blob = blob_builder.make().unwrap();
            self.canvas.draw_text_blob(&blob, (0., 0.), self.paint);

            return;
        }

        // Dim colors for underground features.
        if self.tags.tunnel {
            style.color.r = (style.color.r as f32 * TUNNEL_DIM_FACTOR).round() as u8;
            style.color.g = (style.color.g as f32 * TUNNEL_DIM_FACTOR).round() as u8;
            style.color.b = (style.color.b as f32 * TUNNEL_DIM_FACTOR).round() as u8;
        }

        // TODO: Try out Path::simplify?

        // TODO: set_is_volatile seems to help a little? ~240 -> ~215
        //  - Doesn't matter if it is before or after second draw_path
        //  - Probably should put it before then.
        // TODO: Path effect changes don't seem to affect performance
        // TODO: Color doesn't really seem to affect performance
        // TODO: Stroke changes affect performance ~240 -> 425
        //  => Store stroked/unstroked separately?
        //  - Baseline ~700ms
        //  - Drawing just once without effects ~250ms
        //  - Drawing just once with effects is ~270ms
        //  - Drawing just polygons twice with effects is ~650ms
        //  - Drawing just lines twice with effects is ~300ms
        //  - Drawing just polygons twice with effects but without paint changes is
        //    ~330ms
        //  - Drawing just polygons twice with effects and color changes is ~330ms
        //  - Drawing just polygons twice and toggling only stroke on/off is ~600ms
        //  - Drawing just polygons twice with stroke on for both is ~~820ms
        //  - Baseline without evenodd is ~700ms
        //  - Only stroking polygons vs baseline stroke + fill is 700ms -> ~615ms
        //  - Baseline with polygon paths drawn individually is ~3s!
        //  - No poly border with polygons individually is ~900ms
        match self.geometry_type {
            // Draw polygon paths.
            //
            // This draws all polygons of a single layer. Outlines are not rendered since
            // the performance impact of stroking polygons is too big.
            Some(GeometryType::Polygon) => {
                self.paint.set_color4f(Color4f::from(style.color), None);
                self.paint.set_stroke(false);
                self.canvas.draw_path(&path, self.paint);
            },
            Some(GeometryType::Linestring) => {
                match style.dash {
                    // Apply path effect for dashed lines.
                    Some((on, off)) => {
                        self.paint.set_path_effect(PathEffect::dash(&[on, off], 0.));
                    },
                    None => {
                        // Clear previous path effects.
                        self.paint.set_path_effect(None);

                        // Draw dimmed outline for girthy, non-dashed lines.
                        //
                        // We draw outlines only for lines (streets), since they're more important
                        // for visual clarity compared to buildings, while also impacting
                        // performance significantly less (around -5%).
                        if style.stroke_width > 4. {
                            let mut dim_color = Color4f::from(style.color);
                            dim_color.r *= OUTLINE_DIM_FACTOR;
                            dim_color.g *= OUTLINE_DIM_FACTOR;
                            dim_color.b *= OUTLINE_DIM_FACTOR;
                            self.paint.set_color4f(dim_color, None);
                            self.paint.set_stroke_width(style.stroke_width + 2.);
                            self.paint.set_stroke(true);
                            self.canvas.draw_path(&path, &self.paint);
                        }
                    },
                }

                self.paint.set_color4f(Color4f::from(style.color), None);
                self.paint.set_stroke_width(style.stroke_width);
                self.paint.set_stroke(true);
                self.canvas.draw_path(&path, self.paint);
            },
            _ => (),
        }
    }
}

impl<'a> RenderTile for SkiaRendererState<'a> {
    fn layer_changed(&mut self, layer: Layer, extent: u32) {
        // Finish the previous path by drawing it to the canvas.
        //
        // XXX: Rendering relies on `self.layer`, so we must render here befor updating
        // these fields.
        self.draw_path();

        *self.extent = extent as f32;
        self.layer = layer;
    }

    fn feature_changed(&mut self, geometry_type: GeometryType, tags: Tags) {
        // Finish the previous path by drawing it to the canvas.
        //
        // XXX: Rendering relies on `self.geometry_type` and `self.tags`, so we must
        // render here befor updating these fields.
        self.draw_path();

        *self.geometry_type = Some(geometry_type);
        self.tags = tags;
    }

    fn move_to(&mut self, x: i32, y: i32) {
        if !matches!(self.geometry_type, Some(GeometryType::Polygon | GeometryType::Linestring)) {
            return;
        }

        let x = self.point.x + x as f32 / *self.extent * self.size.width;
        let y = self.point.y + y as f32 / *self.extent * self.size.height;
        self.path.move_to(Point::new(x, y));
    }

    fn line_to(&mut self, x: i32, y: i32) {
        if !matches!(self.geometry_type, Some(GeometryType::Polygon | GeometryType::Linestring)) {
            return;
        }

        let x = self.point.x + x as f32 / *self.extent * self.size.width;
        let y = self.point.y + y as f32 / *self.extent * self.size.height;
        self.path.line_to(Point::new(x, y));
    }

    fn close_path(&mut self) {
        if !matches!(self.geometry_type, Some(GeometryType::Polygon | GeometryType::Linestring)) {
            return;
        }

        // XXX: While our path is complete, we cannot draw it immediately since we need
        // to account for left-winding cutouts. Drawing each geometry individually is
        // also *extremely* slow.
        self.path.close();
    }
}

/// Trait for implementing a renderer for tile vector data.
pub trait RenderTile {
    /// Render a vector tile.
    fn render(&mut self, mut tile: Tile) {
        // Order layers to match render order.
        tile.layers.sort_unstable_by_key(|layer| {
            Layer::from_str(&layer.name).map_or(u16::MAX, |layer| layer as u16)
        });

        for mut layer in tile.layers {
            // Notify render implementation about the new layer.
            match Layer::from_str(&layer.name) {
                Ok(name) => self.layer_changed(name, layer.extent),
                // Stop once we've reached the unknown layers.
                Err(_) => break,
            }

            // Get tags for each feature and filter out unused feature kinds.
            let features = mem::take(&mut layer.features);
            let mut features: Vec<_> = features
                .into_iter()
                .filter_map(|feature| TaggedFeature::new(&layer, feature))
                .collect();

            // Sort features by render order.
            features.sort_unstable();

            for mut feature in features {
                // Ignore unknown geometry types.
                let geometry_type = match GeometryType::try_from(feature.r#type()) {
                    Ok(geometry_type) => geometry_type,
                    Err(_) => continue,
                };

                // Notify render implementation about the new feature.
                let geometry = mem::take(&mut feature.geometry);
                self.feature_changed(geometry_type, feature.tags);

                // The origin point always starts at (0, 0) and is reset for each feature.
                let mut x = 0;
                let mut y = 0;

                // Dispatch render commands in absolute coordinates.
                for command in GeometryIter::new(&geometry) {
                    match command {
                        Command::MoveTo(add_x, add_y) => {
                            x += add_x;
                            y += add_y;
                            self.move_to(x, y);
                        },
                        Command::LineTo(add_x, add_y) => {
                            x += add_x;
                            y += add_y;
                            self.line_to(x, y);
                        },
                        Command::ClosePath => self.close_path(),
                    }
                }
            }
        }
    }

    /// Indicate that the renderer switched to a new layer.
    ///
    /// The extent is the width and height of the coordinate system used for
    /// rendering. A value of `4096` means the point (4096, 4096) is the
    /// bottom-right corner of the tile.
    fn layer_changed(&mut self, _layer: Layer, _extent: u32) {}

    /// Indicate that the renderer switched to a new feature.
    fn feature_changed(&mut self, _geometry_type: GeometryType, _tags: Tags) {}

    /// Move the cursor to the absolute coordinate `(x, y`).
    fn move_to(&mut self, x: i32, y: i32);

    /// Draw a line from the current cursor to the absolute coordinate `(x, y`).
    fn line_to(&mut self, x: i32, y: i32);

    /// Close the path of the current polygon.
    ///
    /// Closing the path of a polygon connects the current cursor's point to the
    /// polygon's starting point. [`Self::move_to`] indicates the starting point
    /// while drawing polygons.
    ///
    /// Closing a path **must not** move the cursor. The cursor position will
    /// never be identical to the starting point.
    fn close_path(&mut self);
}

// TODO: Clean this up a bit, move to config.
//
/// Vector tile theme.
pub struct Theme {
    pub land: Color,
    pub water: Color,
    pub border: Color,
    pub street: Color,
    pub street_highway: Color,
    pub street_primary: Color,
    pub street_secondary: Color,
    pub street_footway: Color,
    pub street_plaza: Color,
    pub street_rail: Color,
    pub nature_forest: Color,
    pub nature_grass: Color,
    pub nature_scrub: Color,
    pub nature_park: Color,
    pub building: Color,
    pub text: Color,
}

impl Theme {
    /// Get the render style for a feature.
    fn style(&self, layer: Layer, tags: &Tags) -> Option<Style> {
        let style = match layer {
            // TODO
            Layer::StreetLabels => match tags.name {
                Some(_) => self.text.into(),
                None => return None,
            },
            Layer::Land => match tags.kind {
                Kind::Scrub
                | Kind::Heath
                | Kind::Swamp
                | Kind::Bog
                | Kind::StringBog
                | Kind::Farmland
                | Kind::Allotments
                | Kind::GreenhouseHorticulture => self.nature_scrub.into(),
                Kind::Grassland
                | Kind::Grass
                | Kind::Garden
                | Kind::VillageGreen
                | Kind::Meadow
                | Kind::WetMeadow
                | Kind::Marsh
                | Kind::GolfCourse => self.nature_grass.into(),
                Kind::Park
                | Kind::Playground
                | Kind::RecreationGround
                | Kind::MiniatureGolf
                | Kind::Cemetery
                | Kind::GraveYard => self.nature_park.into(),
                Kind::Wood | Kind::Forest | Kind::Orchard | Kind::Vineyard | Kind::PlantNursery => {
                    self.nature_forest.into()
                },
                _ => return None,
            },
            Layer::PierLines | Layer::PierPolygons => self.land.into(),
            Layer::Ocean | Layer::WaterPolygons | Layer::WaterLines => self.water.into(),
            Layer::Boundaries => self.border.into(),
            Layer::Buildings | Layer::Bridges => self.building.into(),
            // TODO: Sizes need to be scaled by tile z index.
            Layer::Streets
            | Layer::StreetPolygons
            | Layer::Aerialways
            | Layer::Ferries
            | Layer::PublicTransport => {
                let mut dash = None;
                let (color, stroke_width) = match tags.kind {
                    Kind::Motorway | Kind::MotorwayLink => (self.street_highway, 14.),
                    Kind::Trunk | Kind::TrunkLink => (self.street_highway, 12.),
                    Kind::Primary | Kind::PrimaryLink => (self.street_primary, 10.),
                    Kind::Secondary | Kind::SecondaryLink => (self.street_secondary, 8.),
                    Kind::Tertiary | Kind::TertiaryLink => (self.street, 6.),
                    Kind::Unclassified
                    | Kind::Residential
                    | Kind::LivingStreet
                    | Kind::Service
                    | Kind::Road => (self.street, 4.),
                    Kind::Pedestrian => (self.street_plaza, 4.),
                    Kind::Path
                    | Kind::Track
                    | Kind::Bridleway
                    | Kind::Cycleway
                    | Kind::Footway
                    | Kind::Sidewalk
                    | Kind::Crossing
                    | Kind::TrafficIsland
                    | Kind::Steps => {
                        dash = Some((4., 6.));
                        (self.street_footway, 1.)
                    },
                    Kind::Rail => {
                        dash = Some((6., 6.));
                        (self.street_rail, 3.)
                    },
                    Kind::LightRail | Kind::Funicular | Kind::Ferry => {
                        dash = Some((6., 6.));
                        (self.street_rail, 2.)
                    },
                    Kind::Tram | Kind::Monorail | Kind::Subway => (self.street_rail, 2.),
                    kind => {
                        debug!("unknown street kind: {kind:?}");
                        return None;
                    },
                };

                Style { stroke_width, dash, color }
            },
        };
        Some(style)
    }
}

/// Style used for rendering features.
struct Style {
    color: Color,
    stroke_width: f32,
    dash: Option<(f32, f32)>,
}

impl From<Color> for Style {
    fn from(color: Color) -> Self {
        Self { color, stroke_width: 1., dash: Default::default() }
    }
}

/// Iterator over commands in a feature's geometry.
struct GeometryIter<'a> {
    geometry: &'a [u32],
    command: Option<(Command, u32)>,
    index: usize,
}

impl<'a> GeometryIter<'a> {
    fn new(geometry: &'a [u32]) -> Self {
        Self { geometry, command: Default::default(), index: Default::default() }
    }
}

impl<'a> Iterator for GeometryIter<'a> {
    type Item = Command;

    fn next(&mut self) -> Option<Self::Item> {
        let len = self.geometry.len();
        while self.index < len {
            match &mut self.command {
                // Parse the next command integer.
                None => {
                    // Read next value from geometry.
                    let command_int = self.geometry[self.index];
                    let command_count = command_int >> 3;
                    self.index += 1;

                    // Parse command integer.
                    match command_int & 0x7 {
                        1 => self.command = Some((Command::MoveTo(0, 0), command_count * 2)),
                        2 => self.command = Some((Command::LineTo(0, 0), command_count * 2)),
                        // ClosePath has no parameters, so we stop parsing.
                        7 if command_count == 1 => return Some(Command::ClosePath),
                        7 => {
                            self.command = Some((Command::ClosePath, command_count - 1));
                            return Some(Command::ClosePath);
                        },
                        // Stop if the geometry list is malformed.
                        command_id => {
                            error!("Invalid geometry command id: {command_id}");
                            self.index = len;
                            return None;
                        },
                    }
                },

                // Reset command after all commands of one type are dispatched.
                Some((_, 0)) => self.command = None,

                // Return all remaining `ClosePath` commands.
                Some((Command::ClosePath, count)) => {
                    *count -= 1;

                    return Some(Command::ClosePath);
                },

                // Handle parameters of `MoveTo`/`LineTo` commands.
                Some((command, count)) => {
                    *count -= 1;

                    // Parse parameter value.
                    let parameter = self.geometry[self.index] as i32;
                    let value = (parameter >> 1) ^ (-(parameter & 1));
                    self.index += 1;

                    let (x, y) = match command {
                        Command::MoveTo(x, y) => (x, y),
                        Command::LineTo(x, y) => (x, y),
                        _ => unreachable!(),
                    };

                    if *count % 2 == 0 {
                        *y = value;

                        return Some(*command);
                    } else {
                        *x = value;
                    }
                },
            }
        }

        None
    }
}

/// Geometry drawing command.
#[derive(PartialEq, Eq, Copy, Clone, Debug)]
enum Command {
    MoveTo(i32, i32),
    LineTo(i32, i32),
    ClosePath,
}

/// Vector tile layers
#[derive(PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Debug)]
pub enum Layer {
    // XXX: The order of the elements in this enum is used for rendering, with the
    // first element being the bottommost and the last element being the topmost layer.

    // Background areas.
    Land,
    Ocean,
    WaterPolygons,
    WaterLines,
    PierPolygons,
    PierLines,
    Bridges,
    // We currently don't use any sites.
    // This would include areas like schools/hospitals/parking.
    // Sites,
    Buildings,

    // Pathways.
    Ferries,
    StreetPolygons,
    Streets,
    Aerialways,
    PublicTransport,
    Boundaries,
    //

    // TODO
    // Pois,

    // TODO
    // Labels.
    // WaterPolygonsLabels,
    // WaterLinesLabels,
    // StreetsPolygonsLabels,
    // StreetLabelsPoints,
    StreetLabels,
    // Addresses,
    // PlaceLabels,
    // BoundaryLabels,
}

impl FromStr for Layer {
    type Err = ();

    fn from_str(layer: &str) -> Result<Self, Self::Err> {
        match layer {
            "land" => Ok(Self::Land),
            "ocean" => Ok(Self::Ocean),
            "water_polygons" => Ok(Self::WaterPolygons),
            "water_lines" => Ok(Self::WaterLines),
            "pier_lines" => Ok(Self::PierLines),
            "pier_polygons" => Ok(Self::PierPolygons),
            "boundaries" => Ok(Self::Boundaries),
            "buildings" => Ok(Self::Buildings),
            "streets" => Ok(Self::Streets),
            "street_polygons" => Ok(Self::StreetPolygons),
            "bridges" => Ok(Self::Bridges),
            "aerialways" => Ok(Self::Aerialways),
            "ferries" => Ok(Self::Ferries),
            "public_transport" => Ok(Self::PublicTransport),
            // "sites" => Ok(Self::Sites),
            // "pois" => Ok(Self::Pois),
            // "addresses" => Ok(Self::Addresses),
            // "boundary_labels" => Ok(Self::BoundaryLabels),
            // "place_labels" => Ok(Self::PlaceLabels),
            "street_labels" => Ok(Self::StreetLabels),
            // "street_labels_points" => Ok(Self::StreetLabelsPoints),
            // "streets_polygons_labels" => Ok(Self::StreetsPolygonsLabels),
            // "water_lines_labels" => Ok(Self::WaterLinesLabels),
            // "water_polygons_labels" => Ok(Self::WaterPolygonsLabels),
            _ => Err(()),
        }
    }
}

/// Types of renderable geometry.
#[derive(Hash, PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Debug)]
pub enum GeometryType {
    Point,
    Linestring,
    Polygon,
}

impl TryFrom<GeomType> for GeometryType {
    type Error = ();

    fn try_from(geometry_type: GeomType) -> Result<Self, Self::Error> {
        match geometry_type {
            GeomType::Unknown => Err(()),
            GeomType::Point => Ok(GeometryType::Point),
            GeomType::Linestring => Ok(GeometryType::Linestring),
            GeomType::Polygon => Ok(GeometryType::Polygon),
        }
    }
}

/// Feature with cached tags.
#[derive(PartialEq, Eq)]
pub struct TaggedFeature {
    feature: Feature,
    tags: Tags,
}

impl TaggedFeature {
    fn new(layer: &ProtobufLayer, feature: Feature) -> Option<Self> {
        // Get tags for this feature from its layer.
        let mut tags = Tags::default();
        for chunk in feature.tags.chunks_exact(2) {
            let key = &layer.keys[chunk[0] as usize];
            match key.as_str() {
                "kind" => {
                    let value = &layer.values[chunk[1] as usize];
                    tags.kind = value.string_value.as_deref().map_or(Kind::None, Kind::from);
                },
                "tunnel" => {
                    let value = &layer.values[chunk[1] as usize];
                    tags.tunnel = value.bool_value.unwrap_or(false);
                },
                "name" => {
                    let value = &layer.values[chunk[1] as usize];
                    tags.name = value.string_value.clone();
                },
                _ => (),
            }
        }

        if matches!(tags.kind, Kind::Unknown) { None } else { Some(Self { feature, tags }) }
    }
}

impl PartialOrd for TaggedFeature {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TaggedFeature {
    fn cmp(&self, other: &Self) -> Ordering {
        // Render tunneled features below all others.
        if self.tags.tunnel && !other.tags.tunnel {
            return Ordering::Less;
        } else if !self.tags.tunnel && other.tags.tunnel {
            return Ordering::Greater;
        }

        // With all else being equal, determine render order based on feature kind.
        self.tags.kind.cmp(&other.tags.kind)
    }
}

impl Deref for TaggedFeature {
    type Target = Feature;

    fn deref(&self) -> &Self::Target {
        &self.feature
    }
}

impl DerefMut for TaggedFeature {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.feature
    }
}

/// Feature tags.
#[derive(PartialEq, Eq, Clone, Default, Debug)]
pub struct Tags {
    name: Option<String>,
    tunnel: bool,
    kind: Kind,
}

/// Kind feature tag variants.
#[derive(PartialEq, Eq, PartialOrd, Ord, Copy, Clone, Default, Debug)]
pub enum Kind {
    // XXX: The order of these variants defines the order in which features are rendered.

    // "Areas" used for backgrounds.
    Sand,
    Dune,
    Beach,
    Scrub,
    Heath,
    Swamp,
    Bog,
    StringBog,
    Farmland,
    Allotments,
    GreenhouseHorticulture,
    Grassland,
    Grass,
    Garden,
    VillageGreen,
    Meadow,
    WetMeadow,
    Marsh,
    GolfCourse,
    Park,
    Playground,
    RecreationGround,
    MiniatureGolf,
    Cemetery,
    GraveYard,
    Wood,
    Forest,
    Orchard,
    Vineyard,
    PlantNursery,
    Water,
    River,
    Canal,
    Ditch,
    Stream,
    Basin,
    Dock,
    Bridge,
    Pier,
    Breakwater,
    Gryone,
    Pedestrian,

    // Car roads in ascending size.
    Taxiway,
    Runway,
    Road,
    Service,
    LivingStreet,
    // XXX: This is both a land and a street tag. If it is ever rendered as a land, we
    // need to special-case this for feature sorting or it will overlay all parks etc.
    Residential,
    Unclassified,
    TertiaryLink,
    Tertiary,
    SecondaryLink,
    Secondary,
    PrimaryLink,
    Primary,
    TrunkLink,
    Trunk,
    MotorwayLink,
    Motorway,

    // Pedestrian roads in descending size.
    Path,
    Track,
    Bridleway,
    Cycleway,
    Footway,
    Sidewalk,
    Crossing,
    TrafficIsland,
    Steps,

    // Rail in ascending size.
    Ferry,
    Subway,
    Tram,
    Funicular,
    LightRail,
    Rail,
    Monorail,

    #[default]
    None,

    // Unused and/or unknown features.
    Unknown,
}

impl From<&str> for Kind {
    fn from(s: &str) -> Self {
        match s {
            "allotments" => Self::Allotments,
            "basin" => Self::Basin,
            "beach" => Self::Beach,
            "bog" => Self::Bog,
            "breakwater" => Self::Breakwater,
            "bridge" => Self::Bridge,
            "bridleway" => Self::Bridleway,
            "canal" => Self::Canal,
            "cemetery" => Self::Cemetery,
            "crossing" => Self::Crossing,
            "cycleway" => Self::Cycleway,
            "ditch" => Self::Ditch,
            "dock" => Self::Dock,
            "dune" => Self::Dune,
            "farmland" => Self::Farmland,
            "ferry" => Self::Ferry,
            "footway" => Self::Footway,
            "forest" => Self::Forest,
            "funicular" => Self::Funicular,
            "garden" => Self::Garden,
            "golf_course" => Self::GolfCourse,
            "grassland" => Self::Grassland,
            "grass" => Self::Grass,
            "grave_yard" => Self::GraveYard,
            "greenhouse_horticulture" => Self::GreenhouseHorticulture,
            "gryone" => Self::Gryone,
            "heath" => Self::Heath,
            "light_rail" => Self::LightRail,
            "living_street" => Self::LivingStreet,
            "marsh" => Self::Marsh,
            "meadow" => Self::Meadow,
            "miniature_golf" => Self::MiniatureGolf,
            "monorail" => Self::Monorail,
            "motorway_link" => Self::MotorwayLink,
            "motorway" => Self::Motorway,
            "orchard" => Self::Orchard,
            "park" => Self::Park,
            "path" => Self::Path,
            "pedestrian" => Self::Pedestrian,
            "pier" => Self::Pier,
            "plant_nursery" => Self::PlantNursery,
            "playground" => Self::Playground,
            "primary_link" => Self::PrimaryLink,
            "primary" => Self::Primary,
            "rail" => Self::Rail,
            "recreation_ground" => Self::RecreationGround,
            "residential" => Self::Residential,
            "river" => Self::River,
            "road" => Self::Road,
            "runway" => Self::Runway,
            "sand" => Self::Sand,
            "scrub" => Self::Scrub,
            "secondary_link" => Self::SecondaryLink,
            "secondary" => Self::Secondary,
            "service" => Self::Service,
            "sidewalk" => Self::Sidewalk,
            "steps" => Self::Steps,
            "stream" => Self::Stream,
            "string_bog" => Self::StringBog,
            "subway" => Self::Subway,
            "swamp" => Self::Swamp,
            "taxiway" => Self::Taxiway,
            "tertiary_link" => Self::TertiaryLink,
            "tertiary" => Self::Tertiary,
            "track" => Self::Track,
            "traffic_island" => Self::TrafficIsland,
            "tram" => Self::Tram,
            "trunk_link" => Self::TrunkLink,
            "trunk" => Self::Trunk,
            "unclassified" => Self::Unclassified,
            "village_green" => Self::VillageGreen,
            "vineyard" => Self::Vineyard,
            "water" => Self::Water,
            "wet_meadow" => Self::WetMeadow,
            "wood" => Self::Wood,

            _ => {
                debug!("ignoring unused feature of kind: {s:?}");
                Self::Unknown
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use prost::Message;
    use skia_safe::png_encoder::{self, Options as PngOptions};
    use skia_safe::surface::surfaces;

    use super::*;

    #[test]
    fn empty_geometry_iter() {
        let geometry = [];
        let mut iter = GeometryIter::new(&geometry);

        assert_eq!(iter.next(), None);
    }

    #[test]
    fn single_geometry_iter() {
        let geometry = [9, 50, 34];
        let mut iter = GeometryIter::new(&geometry);

        assert_eq!(iter.next(), Some(Command::MoveTo(25, 17)));
        assert_eq!(iter.next(), None);

        let geometry = [10, 50, 34];
        let mut iter = GeometryIter::new(&geometry);

        assert_eq!(iter.next(), Some(Command::LineTo(25, 17)));
        assert_eq!(iter.next(), None);

        let geometry = [15];
        let mut iter = GeometryIter::new(&geometry);

        assert_eq!(iter.next(), Some(Command::ClosePath));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn multi_geometry_iter() {
        let geometry = [17, 10, 14, 3, 9];
        let mut iter = GeometryIter::new(&geometry);

        assert_eq!(iter.next(), Some(Command::MoveTo(5, 7)));
        assert_eq!(iter.next(), Some(Command::MoveTo(-2, -5)));
        assert_eq!(iter.next(), None);

        let geometry = [9, 4, 4, 18, 0, 16, 16, 0];
        let mut iter = GeometryIter::new(&geometry);

        assert_eq!(iter.next(), Some(Command::MoveTo(2, 2)));
        assert_eq!(iter.next(), Some(Command::LineTo(0, 8)));
        assert_eq!(iter.next(), Some(Command::LineTo(8, 0)));
        assert_eq!(iter.next(), None);

        let geometry = [9, 4, 4, 18, 0, 16, 16, 0, 9, 17, 17, 10, 4, 8];
        let mut iter = GeometryIter::new(&geometry);

        assert_eq!(iter.next(), Some(Command::MoveTo(2, 2)));
        assert_eq!(iter.next(), Some(Command::LineTo(0, 8)));
        assert_eq!(iter.next(), Some(Command::LineTo(8, 0)));
        assert_eq!(iter.next(), Some(Command::MoveTo(-9, -9)));
        assert_eq!(iter.next(), Some(Command::LineTo(2, 4)));
        assert_eq!(iter.next(), None);

        let geometry = [9, 6, 12, 18, 10, 12, 24, 44, 15];
        let mut iter = GeometryIter::new(&geometry);

        assert_eq!(iter.next(), Some(Command::MoveTo(3, 6)));
        assert_eq!(iter.next(), Some(Command::LineTo(5, 6)));
        assert_eq!(iter.next(), Some(Command::LineTo(12, 22)));
        assert_eq!(iter.next(), Some(Command::ClosePath));
        assert_eq!(iter.next(), None);

        let geometry = [
            9, 0, 0, 26, 20, 0, 0, 20, 19, 0, 15, 9, 22, 2, 26, 18, 0, 0, 18, 17, 0, 15, 9, 4, 13,
            26, 0, 8, 8, 0, 0, 7, 15,
        ];
        let mut iter = GeometryIter::new(&geometry);

        assert_eq!(iter.next(), Some(Command::MoveTo(0, 0)));
        assert_eq!(iter.next(), Some(Command::LineTo(10, 0)));
        assert_eq!(iter.next(), Some(Command::LineTo(0, 10)));
        assert_eq!(iter.next(), Some(Command::LineTo(-10, 0)));
        assert_eq!(iter.next(), Some(Command::ClosePath));
        assert_eq!(iter.next(), Some(Command::MoveTo(11, 1)));
        assert_eq!(iter.next(), Some(Command::LineTo(9, 0)));
        assert_eq!(iter.next(), Some(Command::LineTo(0, 9)));
        assert_eq!(iter.next(), Some(Command::LineTo(-9, 0)));
        assert_eq!(iter.next(), Some(Command::ClosePath));
        assert_eq!(iter.next(), Some(Command::MoveTo(2, -7)));
        assert_eq!(iter.next(), Some(Command::LineTo(0, 4)));
        assert_eq!(iter.next(), Some(Command::LineTo(4, 0)));
        assert_eq!(iter.next(), Some(Command::LineTo(0, -4)));
        assert_eq!(iter.next(), Some(Command::ClosePath));
        assert_eq!(iter.next(), None);
    }

    #[test]
    fn xxx() {
        let directives = std::env::var("RUST_LOG").unwrap_or("debug".into());
        let env_filter = tracing_subscriber::EnvFilter::builder().parse_lossy(directives);
        tracing_subscriber::FmtSubscriber::builder()
            .with_env_filter(env_filter)
            .with_line_number(true)
            .init();

        // TODO: Actually store in this repo.
        const MVT: &[u8] = include_bytes!("/home/undeadleech/vector_tile_tests/14_8508_5489.mvt");

        let theme = Theme {
            land: Color::new(242, 239, 233),
            water: Color::new(170, 211, 223),
            border: Color::new(191, 169, 186),
            street: Color::new(255, 255, 255),
            street_highway: Color::new(233, 144, 160),
            street_primary: Color::new(253, 215, 161),
            street_secondary: Color::new(246, 250, 187),
            street_footway: Color::new(250, 128, 114),
            street_plaza: Color::new(221, 221, 232),
            street_rail: Color::new(112, 112, 112),
            nature_forest: Color::new(173, 209, 158),
            nature_grass: Color::new(205, 235, 176),
            nature_scrub: Color::new(200, 215, 171),
            nature_park: Color::new(200, 250, 204),
            building: Color::new(217, 208, 201),
            text: Color::new(0, 0, 0),
        };

        let size = Size::new(1024, 1024);
        let mut surface = surfaces::raster_n32_premul(size).unwrap();
        surface.canvas().clear(Color4f::from(theme.land));
        let tile = Tile::decode(MVT).unwrap();

        let start = std::time::Instant::now();
        let mut renderer = SkiaRenderer::new(theme);
        renderer.render(surface.canvas(), Point::new(0., 0.), size.into(), tile);

        let start = std::time::Instant::now();
        let image = surface.image_snapshot();
        let start = std::time::Instant::now();
        let png = png_encoder::encode_image(None, &image, &PngOptions::default()).unwrap();

        // TODO: Replace with assert at some point.
        fs::write("/tmp/map.png", png.as_bytes()).unwrap();

        assert!(false);
    }
}
