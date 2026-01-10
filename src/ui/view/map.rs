//! Map rendering UI view.

use std::any::Any;
use std::collections::HashMap;
use std::mem;
use std::time::{Duration, Instant};

use calloop::channel::{self, Event};
use calloop::timer::{TimeoutAction, Timer};
use calloop::{LoopHandle, RegistrationToken};
use reqwest::Client;
use skia_safe::{
    Color4f, FilterMode, MipmapMode, Paint, PaintCap, PaintJoin, PathBuilder, Rect, SamplingOptions,
};
use tracing::error;

use crate::config::{Config, Input};
use crate::db::Db;
use crate::dbus::modem_manager;
use crate::geometry::{self, GeoPoint, Point, Size, rect_intersects_line};
use crate::router::Route;
use crate::tiles::{MAX_ZOOM, TILE_SIZE, TileIndex, TileIter, Tiles};
use crate::ui::skia::RenderState;
use crate::ui::view::search::RouteOrigin;
use crate::ui::view::{UiView, View};
use crate::ui::{Button, Svg, Velocity};
use crate::{Error, State};

/// Button width and height at scale 1.
const BUTTON_SIZE: u32 = 48;

/// Padding around the buttons at scale 1.
const BUTTON_PADDING: u32 = 16;

/// Border size around the buttons at scale 1.
const BUTTON_BORDER: f64 = 1.;

/// Border size around the locked GPS button at scale 1.
const LOCKED_GPS_BORDER: f64 = 4.;

/// Attribution label font size relative to the default.
const ATTRIBUTION_FONT_SIZE: f32 = 0.5;

/// POI/GPS indicator width/height at scale 1.
const INDICATOR_SIZE: f32 = 10.;

/// POI/GPS indicator border size at scale 1.
const INDICATOR_BORDER: f32 = 4.;

/// Time after losing GPS signal before GPS indicator is removed.
const GPS_TIMEOUT: Duration = Duration::from_secs(10);

/// Width of the route path at scale 1 and max zoom.
const ROUTE_WIDTH: f32 = 10.;

/// Square of the minimum physical distance between a route's path segments.
const ROUTE_RESOLUTION: f64 = 15.;

/// Percentage of route width used to center the map.
const ROUTE_ZOOM_PADDING: f64 = 1.1;

/// Maximum GPS distance to be considered ON the route.
const MAX_GPS_ROUTE_DISTANCE: u32 = 15;

/// Minimum distance before rerouting a GPS route.
const MIN_GPS_REROUTE_DISTANCE: u32 = 30;

/// Minimum duration between rerouting attempts.
const MIN_REROUTE_INTERVAL: Duration = Duration::from_secs(5);

/// Map rendering UI view.
pub struct MapView {
    pending_tiles: Vec<TileIndex>,
    tiles: Tiles,

    gps_timeout: Option<RegistrationToken>,
    gps: Option<RenderGeoPoint>,
    poi: Option<RenderGeoPoint>,
    route: Option<(Vec<RenderGeoPoint>, bool)>,
    last_reroute: Instant,
    rerouting: bool,

    cursor_tile: TileIndex,
    cursor_offset: Point,
    cursor_zoom: f64,
    gps_locked: bool,

    search_button: Button,
    gps_button: Button,
    route_paint: Paint,
    tile_paint: Paint,

    touch_state: TouchState,
    input_config: Input,

    event_loop: LoopHandle<'static, State>,

    size: Size,
    scale: f64,

    dirty: bool,
}

impl MapView {
    #[cfg_attr(feature = "profiling", profiling::function)]
    pub fn new(
        event_loop: LoopHandle<'static, State>,
        client: Client,
        db: Db,
        config: &Config,
        size: Size,
    ) -> Result<Self, Error> {
        // Initialize the tile cache.
        let (tile_tx, tile_rx) = channel::channel();
        event_loop.insert_source(tile_rx, |event, _, state| {
            let map_view = state.window.views.map();
            if let Event::Msg(tile_index) = event
                && map_view.pending_tiles.contains(&tile_index)
            {
                map_view.dirty = true;
                state.window.unstall();
            }
        })?;
        let tiles = Tiles::new(client, db, tile_tx, config)?;

        // Listen for new GPS location updates.
        Self::spawn_gps(&event_loop)?;

        // Set (0, 0) start location at a zoom level without empty space.
        let (cursor_tile, cursor_offset) = GeoPoint::new(0., 0.).tile(3);

        // Initialize UI elements.
        let point = Self::search_button_point(size, 1.);
        let size = Self::button_size(1.);
        let search_button = Button::new(point, size, Svg::Search);

        let point = Self::gps_button_point(size, 1.);
        let size = Self::button_size(1.);
        let gps_button = Button::new(point, size, Svg::Gps);

        let mut tile_paint = Paint::default();
        tile_paint.set_color4f(Color4f::from(config.colors.background), None);

        // XXX: We intentionally set anti-aliasing to FALSE, since it kills performance.
        // With 69 elements, `draw_path` time increased from ~0.3ms to 10+ms.
        let mut route_paint = Paint::default();
        route_paint.set_color4f(Color4f::from(config.colors.highlight), None);
        route_paint.set_stroke_join(PaintJoin::Bevel);
        route_paint.set_stroke_cap(PaintCap::Round);
        route_paint.set_stroke_width(ROUTE_WIDTH);
        route_paint.set_anti_alias(false);
        route_paint.set_stroke(true);

        Ok(Self {
            cursor_offset,
            search_button,
            cursor_tile,
            route_paint,
            event_loop,
            gps_button,
            tile_paint,
            tiles,
            size,
            last_reroute: Instant::now(),
            input_config: config.input,
            dirty: true,
            scale: 1.,
            pending_tiles: Default::default(),
            cursor_zoom: Default::default(),
            touch_state: Default::default(),
            gps_timeout: Default::default(),
            gps_locked: Default::default(),
            rerouting: Default::default(),
            route: Default::default(),
            gps: Default::default(),
            poi: Default::default(),
        })
    }

    /// Access the tile storage.
    pub fn tiles(&self) -> &Tiles {
        &self.tiles
    }

    /// Get the geographic reference point for geocoding.
    pub fn reference_point(&self) -> GeoPoint {
        match self.gps {
            Some(gps) => gps.point,
            None => GeoPoint::from_tile(self.cursor_tile, self.cursor_offset),
        }
    }

    /// Get the current tile zoom level.
    pub fn zoom(&self) -> u8 {
        self.cursor_tile.z
    }

    /// Go to a specific coordinate.
    pub fn goto(&mut self, point: GeoPoint, zoom: u8) {
        let (cursor_tile, cursor_offset) = point.tile(zoom);
        if self.cursor_tile != cursor_tile || self.cursor_offset != cursor_offset {
            self.cursor_tile = cursor_tile;
            self.cursor_offset = cursor_offset;
            self.gps_locked = false;
            self.dirty = true;
        }
    }

    /// Highlight a specific point on the map.
    pub fn set_poi(&mut self, point: Option<GeoPoint>) {
        let point = point.map(RenderGeoPoint::from);
        if self.poi == point {
            return;
        }

        // Clear route when a new POI is set.
        if point.is_some() {
            self.cancel_route();
        }

        self.dirty = true;
        self.poi = point;
    }

    /// Update the GPS indicator location.
    #[cfg_attr(feature = "profiling", profiling::function)]
    pub fn set_gps(&mut self, point: Option<GeoPoint>) {
        let point = point.map(RenderGeoPoint::from);
        match point {
            // Handle changed GPS positions.
            Some(point) if Some(point) != self.gps => {
                // Cancel pending GPS removal on update.
                if let Some(token) = self.gps_timeout.take() {
                    self.event_loop.remove(token);
                }

                // Jump to new GPS position if the view is locked to the GPS.
                if self.gps_locked {
                    self.goto(point.point, self.cursor_tile.z);
                    self.gps_locked = true;
                }

                // Update current route.
                if let Some((route, true)) = &mut self.route {
                    let (index, distance) = nearest_route_segment(route, point.point);

                    // Update the route to remove segments already traveled.
                    if distance <= MAX_GPS_ROUTE_DISTANCE && index > 0 {
                        *route = route.split_off(index);
                    }

                    // Reroute if GPS is way off course.
                    if !self.rerouting
                        && distance >= MIN_GPS_REROUTE_DISTANCE
                        && let Some(&target) = route.last()
                        && self.last_reroute.elapsed() >= MIN_REROUTE_INTERVAL
                    {
                        self.rerouting = true;
                        self.event_loop.insert_idle(move |state| {
                            state.window.views.search().route(RouteOrigin::Gps, target.point);
                        });
                    }
                }

                self.gps = Some(point);
                self.dirty = true;
            },
            // To avoid 'flickering' we only remove GPS after a period of inactivity.
            None if self.gps_timeout.is_none() => {
                let timer = Timer::from_duration(GPS_TIMEOUT);
                let token = self.event_loop.insert_source(timer, move |_, _, state| {
                    let map_view = state.window.views.map();
                    map_view.dirty |= map_view.gps.is_some();
                    map_view.gps_timeout = None;
                    map_view.gps_locked = false;
                    map_view.gps = None;

                    state.window.unstall();

                    TimeoutAction::Drop
                });
                self.gps_timeout = token
                    .inspect_err(|err| error!("Failed to stage GPS removal timeout: {err}"))
                    .ok();
            },
            _ => (),
        }
    }

    /// Update the active route.
    #[cfg_attr(feature = "profiling", profiling::function)]
    pub fn set_route(&mut self, route: Route, is_gps_route: bool) {
        let had_gps_route = self.route.as_ref().is_some_and(|(_, is_gps_route)| *is_gps_route);

        // Convert route from segments to renderable geographic points.
        let points = route
            .segments
            .into_iter()
            .flat_map(|segment| segment.points.into_iter().map(RenderGeoPoint::from))
            .collect();
        self.route = Some((points, is_gps_route));

        // Lock and center new GPS route, or show entire non-GPS route.
        if is_gps_route
            && !had_gps_route
            && let Some(gps) = self.gps
        {
            self.goto(gps.point, self.cursor_tile.z);
            self.gps_locked = true;
        } else if !is_gps_route {
            self.center_route();
        }

        self.reset_reroute_timeout();

        // Clear POIs, since they're either part of the route or a distraction.
        self.poi = None;

        self.dirty = true;
    }

    /// Clear rerouting timeout.
    pub fn reset_reroute_timeout(&mut self) {
        self.last_reroute = Instant::now();
        self.rerouting = false;
    }

    /// Clear the active route.
    pub fn cancel_route(&mut self) {
        self.dirty |= self.route.is_some();
        self.route = None;
    }

    /// Get current route's origin and target, if present.
    pub fn active_route(&self) -> Option<(RouteOrigin, GeoPoint)> {
        let (route, is_gps_route) = self.route.as_ref()?;
        if *is_gps_route {
            Some((RouteOrigin::Gps, route.last()?.point))
        } else {
            Some((route.first()?.point.into(), route.last()?.point))
        }
    }

    /// Touch long-press callback.
    pub fn trigger_long_press(&mut self, mut point: Point<f64>) {
        // Manually reset touch state, since touch release might be sent to search view.
        self.touch_state.slots.clear();
        self.touch_state.last_time = 0;

        // Convert point from screen origin to center origin.
        let size = self.size * self.scale;
        point.x -= size.width as f64 / 2.;
        point.y -= size.height as f64 / 2.;

        // Convert screen point to geographic point.
        let (tile_index, offset) = self.center_point_tile(point);
        let geo_point = GeoPoint::from_tile(tile_index, offset);

        // Clear POI marker.
        self.set_poi(None);

        // Submit query and open search view.
        self.event_loop.insert_idle(move |state| {
            state.window.views.search().reverse(geo_point, tile_index.z);
            state.window.set_view(View::Search);
        });
    }

    /// Move the map by a pixel delta.
    fn move_by(&mut self, delta: Point<f64>) {
        if delta == Point::default() {
            return;
        }

        let (tile, offset) = self.center_point_tile(delta * -1.);
        self.cursor_tile = tile;
        self.cursor_offset = offset;

        self.gps_locked = false;
        self.dirty = true;
    }

    /// Convert a point relative to the screen's center to a tile + offset.
    fn center_point_tile(&self, point: Point<f64>) -> (TileIndex, Point) {
        let mut tile = self.cursor_tile;
        let mut offset = self.cursor_offset;

        // Apply sub-tile scale, since the cursor is in tile coordinates.
        let scale = self.zoom_scale();
        let x = (point.x / scale).round() as i32;
        let y = (point.y / scale).round() as i32;

        let max_tile = (1 << tile.z) - 1;
        let offset_x = offset.x + x;
        let offset_y = offset.y + y;

        // Calculate tile index.
        let tile_x = tile.x as i32 + offset_x.div_euclid(TILE_SIZE);
        let tile_y = tile.y as i32 + offset_y.div_euclid(TILE_SIZE);
        tile.x = tile_x.clamp(0, max_tile) as u32;
        tile.y = tile_y.clamp(0, max_tile) as u32;

        // Calculate tile offset.
        let clamp_offset = |tile: i32, offset: i32| {
            if tile > max_tile {
                255
            } else if tile < 0 {
                0
            } else {
                offset.rem_euclid(TILE_SIZE)
            }
        };
        offset.x = clamp_offset(tile_x, offset_x);
        offset.y = clamp_offset(tile_y, offset_y);

        (tile, offset)
    }

    /// Zoom the map by a percentage.
    ///
    /// A value of `2.5` will increase the resolution of the current map by
    /// `2.5`, which will increase the tileset zoom level by 1.
    fn zoom_by(&mut self, zoom: f64) {
        let map_delta = (1. / zoom).log2() - self.cursor_zoom;
        let map_delta_trunc = map_delta.trunc() as i32;

        let size = self.size * self.scale;
        let center = Point::new(size.width as f64, size.height as f64) / 2.;

        // Always use screen center (GPS location) as zoom focus while locked.
        let zoom_focus = if self.gps_locked && self.gps.is_some() {
            center
        } else {
            self.touch_state.zoom_focus
        };

        // Calculate offset required to keep zoom focus stationary.
        //
        // Rounding and precision here means the focus point isn't kept precisely in the
        // same position, which is fine for our usecase.
        //
        // The rounding is required to avoid insignificant negative changes being
        // floored to integer cursor_offset changes and causing the map to move around
        // when moving the zoom points without changing their distance.
        let focus_delta = zoom_focus - center;
        let mut focus_offset = focus_delta / self.zoom_scale() * (zoom - 1.);
        focus_offset.x = focus_offset.x.round();
        focus_offset.y = focus_offset.y.round();

        // Convert position within tile back to fractional tile indices.
        let tile_x = self.cursor_tile.x as f64
            + (self.cursor_offset.x as f64 + focus_offset.x) / TILE_SIZE as f64;
        let tile_y = self.cursor_tile.y as f64
            + (self.cursor_offset.y as f64 + focus_offset.y) / TILE_SIZE as f64;
        let tile_z = self.cursor_tile.z as i32;

        // Calculate new fractional tile indices.
        let tile_delta = map_delta_trunc.clamp(-(MAX_ZOOM as i32 - tile_z), tile_z);
        let new_tile_x = tile_x * 2f64.powi(-tile_delta);
        let new_tile_y = tile_y * 2f64.powi(-tile_delta);

        // Convert fractional, to integer tile indices and offset.
        let x_offset = (new_tile_x.fract() * TILE_SIZE as f64).floor() as i32;
        let y_offset = (new_tile_y.fract() * TILE_SIZE as f64).floor() as i32;
        self.cursor_tile.x = new_tile_x.trunc() as u32;
        self.cursor_tile.y = new_tile_y.trunc() as u32;
        self.cursor_tile.z = (tile_z - tile_delta) as u8;
        self.cursor_offset = Point::new(x_offset, y_offset);

        // Clamp scale fraction to 199/49% when clamped.
        self.cursor_zoom = if map_delta_trunc != tile_delta {
            0.999f64.copysign(-map_delta_trunc as f64)
        } else {
            -map_delta.fract()
        };

        self.dirty = true;
    }

    /// Snap zoom to nearest integer tile scale.
    fn snap_zoom(&mut self) {
        if self.cursor_zoom == 0. {
            return;
        }

        if (self.cursor_zoom < -0.5 && self.cursor_tile.z > 0)
            || self.cursor_zoom >= 0.5 && self.cursor_tile.z < MAX_ZOOM
        {
            let zoom_signum = self.cursor_zoom.signum() as i32;

            let tile_x = self.cursor_tile.x as f64 + self.cursor_offset.x as f64 / TILE_SIZE as f64;
            let tile_x = tile_x * 2f64.powi(zoom_signum);
            let tile_y = self.cursor_tile.y as f64 + self.cursor_offset.y as f64 / TILE_SIZE as f64;
            let tile_y = tile_y * 2f64.powi(zoom_signum);

            self.cursor_tile.x = tile_x.floor() as u32;
            self.cursor_tile.y = tile_y.floor() as u32;
            self.cursor_tile.z = (self.cursor_tile.z as i32 + zoom_signum) as u8;

            self.cursor_offset.x = (tile_x.fract() * TILE_SIZE as f64).floor() as i32;
            self.cursor_offset.y = (tile_y.fract() * TILE_SIZE as f64).floor() as i32;
        }
        self.cursor_zoom = 0.;

        self.dirty = true;
    }

    /// Get the current sub-tile zoom level.
    ///
    /// A value of 1.5 means tiles should be rendered at 150% of their size.
    /// This value will always be between 0.5 and 2.0, never reaching either
    /// bound.
    fn zoom_scale(&self) -> f64 {
        (2f64.powf(self.cursor_zoom) * 100.).round() / 100.
    }

    /// Physical size of the UI buttons.
    fn button_size(scale: f64) -> Size {
        Size::new(BUTTON_SIZE, BUTTON_SIZE) * scale
    }

    /// Physical location of the search button.
    fn search_button_point(size: Size, scale: f64) -> Point {
        let padding = (BUTTON_PADDING as f64 * scale).round() as i32;
        let button_size = Self::button_size(scale);
        let physical_size = size * scale;

        let x = (physical_size.width - button_size.width) as i32 - padding;
        let y = (physical_size.height - button_size.height) as i32 - padding;

        Point::new(x, y)
    }

    /// Physical location of the GPS centering button.
    fn gps_button_point(size: Size, scale: f64) -> Point {
        let search_button_point = Self::search_button_point(size, scale);
        let padding = (BUTTON_PADDING as f64 * scale).round() as i32;
        let button_size = Self::button_size(scale);

        let mut point = search_button_point;
        point.x -= button_size.width as i32 + padding;

        point
    }

    /// Set tile index and offset to give an overview over the current route.
    #[cfg_attr(feature = "profiling", profiling::function)]
    fn center_route(&mut self) {
        // Get geographical start and end point of the route.
        //
        // While in theory the start and end might not be the furthest point apart from
        // each other, this should work in most scenarios and avoids having to
        // determine maximum bounds for all points in the route.
        let (start, end) =
            match self.route.as_ref().and_then(|(r, _)| Some((r.first()?, r.last()?))) {
                Some((start, end)) => (start.point, end.point),
                None => return,
            };

        // Calculate center point of the route.
        let center_lat = (start.lat + end.lat) / 2.;
        let center_lon = (start.lon + end.lon) / 2.;
        let center = GeoPoint::new(center_lat, center_lon);

        // Calculate maximum dimensions (in meters) of the route.
        //
        // We use the minimum latitude for width calculation since circumference gets
        // bigger when closer to the equator (lat 0), which gives us the maximum
        // required distance.
        let min_lat = start.lat.min(end.lat);
        let width = GeoPoint::new(min_lat, start.lon).distance(GeoPoint::new(min_lat, end.lon));
        let height = GeoPoint::new(start.lat, 0.).distance(GeoPoint::new(end.lat, 0.));

        // Add tolerance to ensure route doesn't 'bump' into screen borders.
        let width = width as f64 * ROUTE_ZOOM_PADDING;
        let height = height as f64 * ROUTE_ZOOM_PADDING;

        // Calculate required zoom level.
        //
        // We use the maximum latitude for zoom level since pixels are most stretched at
        // the poles (lat 90), so we need to zoom out more when closer to the poles.
        let max_lat = start.lat.max(end.lat);
        let size: Size<f64> = (self.size * self.scale).into();
        let width_zoom = geometry::zoom_for_distance(max_lat, width, size.width);
        let height_zoom = geometry::zoom_for_distance(max_lat, height, size.height);
        let zoom = width_zoom.min(height_zoom);

        self.goto(center, zoom);
    }

    /// Create the GPS location background task.
    fn spawn_gps(event_loop: &LoopHandle<'static, State>) -> Result<(), Error> {
        let (gps_tx, gps_rx) = channel::channel();

        // Listen for new GPS location updates in the background.
        tokio::spawn(async move {
            if let Err(err) = modem_manager::gps_listen(gps_tx).await {
                error!("DBus GPS error: {err}");
            }
        });

        // Forward new GPS locations to the maps view.
        event_loop.insert_source(gps_rx, |event, _, state| {
            let location = match event {
                Event::Msg(location) => location,
                Event::Closed => return,
            };

            state.window.views.search().set_gps(location);
            state.window.views.map().set_gps(location);

            state.window.unstall();
        })?;

        Ok(())
    }
}

impl UiView for MapView {
    #[cfg_attr(feature = "profiling", profiling::function)]
    fn draw<'a>(&mut self, config: &Config, mut render_state: RenderState<'a>) {
        let size = self.size * self.scale;

        // Apply pending velocities.
        if let Some(velocity_delta) = self.touch_state.move_velocity.apply(&self.input_config) {
            self.move_by(velocity_delta);
        }
        if let Some(velocity) = self.touch_state.zoom_velocity.apply(&self.input_config) {
            // Get zoom velocity as increasing distance, then swap numerator and denominator
            // if we're zooming out. This is done to avoid shrinking distances
            // eventually running past the zero point and causing issues.
            let distance = self.touch_state.zoom_velocity_distance + velocity.x;
            let zoom = if self.touch_state.velocity_zooming_in {
                distance / self.touch_state.zoom_velocity_distance
            } else {
                self.touch_state.zoom_velocity_distance / distance
            };

            // Stop velocity once zoom delta drops below 1%.
            if zoom.abs() < 0.01 {
                self.touch_state.zoom_velocity.stop();
            } else {
                self.zoom_by(zoom);
                self.touch_state.zoom_velocity_distance = distance;
            }
        }

        // Clear dirtiness flag.
        //
        // This is inentionally placed after functions like `move_by`, since these
        // modify dirtiness but do not require another redraw.
        self.dirty = false;

        // Reset tiles pending download.
        self.pending_tiles.clear();

        render_state.clear(config.colors.background);

        // Create iterator over visible tiles.
        let mut iter = TileIter::new(size, self.cursor_tile, self.cursor_offset, self.zoom_scale());
        let tile_size = iter.tile_size() as f32;

        // Render all visible tiles.
        for (index, point) in &mut iter {
            // Get image for this tile.
            let image = match self.tiles.get(index).image() {
                Some(image) => image,
                // If the image hasn't loaded yet, add it to the pending tiles.
                None => {
                    self.pending_tiles.push(index);
                    continue;
                },
            };

            #[cfg(feature = "profiling")]
            profiling::scope!("draw_tile_image");

            // Calculate tile's destination rectangle.
            let (x, y) = (point.x as f32, point.y as f32);
            let dst_rect = Rect::new(x, y, x + tile_size, y + tile_size);

            let sampling = SamplingOptions::new(FilterMode::Linear, MipmapMode::Linear);
            render_state.draw_image_rect_with_sampling_options(
                image,
                None,
                dst_rect,
                sampling,
                &self.tile_paint,
            );
        }

        // Render attribution message.
        if !config.tiles.attribution.is_empty() {
            let fg = config.colors.foreground;
            let mut builder = render_state.paragraph(fg, ATTRIBUTION_FONT_SIZE, None);
            builder.add_text(&*config.tiles.attribution);

            let mut paragraph = builder.build();
            paragraph.layout(size.width as f32);
            paragraph.paint(&render_state, Point::new(0., 0.));
        }

        let fill_size = INDICATOR_SIZE * self.scale as f32;
        let border_size = fill_size + INDICATOR_BORDER * self.scale as f32;

        // Draw POI rectangle.
        let poi_tile = self.poi.map(|mut poi| poi.tile(self.cursor_tile.z));
        let poi_point = poi_tile.and_then(|(tile, offset)| iter.screen_point(tile, offset));
        if let Some(point) = poi_point {
            // Draw circle border.
            self.tile_paint.set_color4f(Color4f::from(config.colors.background), None);
            let rect = Rect::new(
                point.x as f32 - border_size / 2.,
                point.y as f32 - border_size / 2.,
                point.x as f32 + border_size / 2.,
                point.y as f32 + border_size / 2.,
            );
            render_state.draw_rect(rect, &self.tile_paint);

            // Draw circle fill.
            self.tile_paint.set_color4f(Color4f::from(config.colors.highlight), None);
            let rect = Rect::new(
                point.x as f32 - fill_size / 2.,
                point.y as f32 - fill_size / 2.,
                point.x as f32 + fill_size / 2.,
                point.y as f32 + fill_size / 2.,
            );
            render_state.draw_rect(rect, &self.tile_paint);
        }

        // Draw GPS circle.
        let gps_tile = self.gps.map(|mut gps| gps.tile(self.cursor_tile.z));
        let gps_point = gps_tile.and_then(|(tile, offset)| iter.screen_point(tile, offset));
        if let Some(point) = gps_point {
            // Draw circle border.
            self.tile_paint.set_color4f(Color4f::from(config.colors.background), None);
            render_state.draw_circle(point, border_size / 2., &self.tile_paint);

            // Draw circle fill.
            self.tile_paint.set_color4f(Color4f::from(config.colors.highlight), None);
            render_state.draw_circle(point, fill_size / 2., &self.tile_paint);
        }

        // Draw current route segments.
        if let Some((route, _)) = &mut self.route {
            #[cfg(feature = "profiling")]
            profiling::scope!("draw_route");

            // Ensure route color is up to date.
            self.route_paint.set_color4f(Color4f::from(config.colors.highlight), None);

            let mut path = PathBuilder::new();
            let mut skipped = true;
            let mut omitted = 0;

            // Add path segments for all visible route sections.
            let size: Size<f64> = size.into();
            for i in 1..route.len() {
                let (start_tile, start_offset) = route[i - omitted - 1].tile(self.cursor_tile.z);
                let start_point = iter.tile_point(start_tile, start_offset).into();

                let (end_tile, end_offset) = route[i].tile(self.cursor_tile.z);
                let end_point = iter.tile_point(end_tile, end_offset).into();

                let delta: Point<f64> = start_point - end_point;

                if i + 1 < route.len() && delta.x.hypot(delta.y) < ROUTE_RESOLUTION {
                    // If a point is too close to the last one, omit it.
                    omitted += 1;
                } else if rect_intersects_line(Point::default(), size, start_point, end_point) {
                    // Draw visible route segments.
                    if mem::take(&mut skipped) {
                        path.move_to(start_point);
                    }
                    path.line_to(end_point);
                    omitted = 0;
                } else {
                    // Skip invisible segments, breaking the path.
                    skipped = true;
                    omitted = 0;
                }
            }

            // Draw the entire path.
            render_state.draw_path(&path.detach(), &self.route_paint);
        }

        // Draw buttons with a border to distinguish them from the map.

        let search_point: Point<f32> = Self::search_button_point(self.size, self.scale).into();
        let gps_point: Point<f32> = Self::gps_button_point(self.size, self.scale).into();
        let button_size: Size<f32> = Self::button_size(self.scale).into();
        let button_border = (BUTTON_BORDER * self.scale).round() as f32;

        let button_points: &mut [_] = match self.gps {
            Some(_) if self.gps_locked => {
                let gps_border = (LOCKED_GPS_BORDER * self.scale).round() as f32;
                let bg = config.colors.background;
                let search = (&mut self.search_button, search_point, button_border, bg);
                let gps = (&mut self.gps_button, gps_point, gps_border, config.colors.highlight);
                &mut [search, gps]
            },
            Some(_) => &mut [
                (&mut self.search_button, search_point, button_border, config.colors.background),
                (&mut self.gps_button, gps_point, button_border, config.colors.background),
            ],
            None => {
                let bg = config.colors.background;
                &mut [(&mut self.search_button, search_point, button_border, bg)]
            },
        };

        for (button, point, border_size, border_color) in button_points {
            let search_left = point.x - *border_size;
            let search_top = point.y - *border_size;
            let search_right = point.x + button_size.width + *border_size;
            let search_bottom = point.y + button_size.height + *border_size;
            let border_rect = Rect::new(search_left, search_top, search_right, search_bottom);

            self.tile_paint.set_color4f(Color4f::from(*border_color), None);
            render_state.draw_rect(border_rect, &self.tile_paint);

            button.draw(&mut render_state, config.colors.alt_background);
        }

        // If no downloads are pending, pre-download tiles just outside the viewport.
        #[cfg(feature = "profiling")]
        profiling::scope!("fetch_background_tiles");
        if self.pending_tiles.is_empty() {
            for index in iter.border_tiles() {
                self.tiles.preload(index);
            }
        }
    }

    fn dirty(&self) -> bool {
        self.dirty
            || self.touch_state.move_velocity.is_moving()
            || self.touch_state.zoom_velocity.is_moving()
    }

    #[cfg_attr(feature = "profiling", profiling::function)]
    fn set_size(&mut self, size: Size) {
        self.size = size;
        self.dirty = true;

        // Update UI elements.
        self.search_button.set_point(Self::search_button_point(size, self.scale));
        self.gps_button.set_point(Self::gps_button_point(size, self.scale));
    }

    #[cfg_attr(feature = "profiling", profiling::function)]
    fn set_scale_factor(&mut self, scale: f64) {
        self.scale = scale;
        self.dirty = true;

        // Update UI elements.
        self.search_button.set_point(Self::search_button_point(self.size, scale));
        self.search_button.set_size(Self::button_size(scale));
        self.gps_button.set_point(Self::gps_button_point(self.size, scale));
        self.gps_button.set_size(Self::button_size(scale));
        self.route_paint.set_stroke_width(ROUTE_WIDTH * scale as f32);
    }

    #[cfg_attr(feature = "profiling", profiling::function)]
    fn touch_down(&mut self, slot: i32, time: u32, point: Point<f64>) {
        let point = point * self.scale;

        // Cancel velocity/long-press if a new touch sequence starts.
        self.touch_state.clear_long_press(&self.event_loop);
        self.touch_state.move_velocity.stop();
        self.touch_state.zoom_velocity.stop();

        // Only allow at most 2 touch slots at a time.
        match self.touch_state.slots.len() {
            0 if self.search_button.contains(point) => {
                self.touch_state.action = TouchAction::Search;
            },
            0 if self.gps_button.contains(point) => {
                self.touch_state.action = TouchAction::Gps;
            },
            0 => {
                // Calculate delta to last tap.
                let elapsed =
                    self.touch_state.last_time + self.input_config.max_multi_tap.as_millis() as u32;
                let delta = self.touch_state.last_point - point;
                let distance = delta.x.powi(2) + delta.y.powi(2);

                let action = if elapsed >= time && distance <= self.input_config.max_tap_distance {
                    TouchAction::DoubleTap
                } else {
                    // Stage long-press only for initial tap action.
                    self.touch_state.stage_long_press(&self.event_loop, &self.input_config, point);

                    TouchAction::Tap
                };
                self.touch_state.action = action;

                // Update state for multi-tap detection.
                self.touch_state.last_time = time;
                self.touch_state.last_point = point;
            },
            1 => self.touch_state.action = TouchAction::Zoom,
            _ => return,
        }

        // Update active touch slot.
        let slot = self.touch_state.slots.entry(slot).or_default();
        slot.point = point;
        slot.start = point;
    }

    #[cfg_attr(feature = "profiling", profiling::function)]
    fn touch_motion(&mut self, id: i32, point: Point<f64>) {
        // Ignore unknown touch slots.
        let slot = match self.touch_state.slots.get_mut(&id) {
            Some(slot) => slot,
            None => return,
        };

        // Update touch point.
        let point = point * self.scale;
        let old_point = mem::replace(&mut slot.point, point);

        // Update the map position.
        match self.touch_state.action {
            TouchAction::Tap | TouchAction::DoubleTap | TouchAction::Drag => {
                // Ignore dragging until tap distance limit is exceeded.
                let max_tap_distance = self.input_config.max_tap_distance;
                let delta = slot.point - slot.start;
                if delta.x.powi(2) + delta.y.powi(2) <= max_tap_distance {
                    return;
                }
                self.touch_state.action = TouchAction::Drag;

                let delta = slot.point - old_point;
                self.touch_state.move_velocity.set(delta);
                self.move_by(delta);

                // Ensure no long press fires after transitioning to drag.
                self.touch_state.clear_long_press(&self.event_loop);
            },
            TouchAction::Zoom => {
                // Get opposing touch slot.
                let slot = *slot;
                let mut slots = self.touch_state.slots.iter();
                let slot2 = match slots.find(|(i, _)| **i != id) {
                    Some((_, slot2)) => slot2,
                    None => return,
                };

                // Ensure zoom's focus point is set.
                self.touch_state.zoom_focus = (slot.start + slot2.start) * 0.5;

                // Calculate relative distance change.

                let last_delta = slot2.point - old_point;
                let last_distance = last_delta.x.hypot(last_delta.y);

                let delta = slot2.point - slot.point;
                let distance = delta.x.hypot(delta.y);

                let zoom = distance / last_distance;
                self.zoom_by(zoom);

                // Set velocity as positive distance traveled since last zoom.

                let velocity = Point::new((distance - last_distance).abs(), 0.);
                self.touch_state.zoom_velocity.set(velocity);

                self.touch_state.velocity_zooming_in = distance > last_distance;
                self.touch_state.zoom_velocity_distance = distance;
            },
            TouchAction::Gps | TouchAction::Search | TouchAction::None => (),
        }
    }

    #[cfg_attr(feature = "profiling", profiling::function)]
    fn touch_up(&mut self, slot: i32) {
        // Reset touch slot, ignoring unknown slots.
        let removed = match self.touch_state.slots.remove(&slot) {
            Some(removed) => removed,
            None => return,
        };

        // Cancel pending long-press timers.
        self.touch_state.clear_long_press(&self.event_loop);

        match self.touch_state.action {
            // On tap, snap zoom to nearest integer scale.
            TouchAction::Tap => self.snap_zoom(),
            // Zoom in to next tile level on double-tap.
            TouchAction::DoubleTap if self.cursor_tile.z < MAX_ZOOM => {
                self.touch_state.zoom_focus = removed.point;
                self.zoom_by(2.);
            },
            // Handle search button press.
            TouchAction::Search if self.search_button.contains(removed.point) => {
                self.event_loop.insert_idle(move |state| state.window.set_view(View::Search));
            },
            // Handle GPS centering button press.
            TouchAction::Gps if self.gps_button.contains(removed.point) => {
                if let Some(RenderGeoPoint { point, .. }) = self.gps {
                    let (tile, offset) = point.tile(self.cursor_tile.z);

                    // Zoom in, if the GPS location is already centered.
                    // Toggle GPS lock if the map is already zoomed in.
                    if self.cursor_offset != offset || self.cursor_tile != tile {
                        self.cursor_offset = offset;
                        self.cursor_tile = tile;
                        self.dirty = true;
                    } else if self.cursor_tile.z != MAX_ZOOM {
                        let (tile, offset) = point.tile(MAX_ZOOM);
                        self.cursor_offset = offset;
                        self.cursor_tile = tile;
                        self.dirty = true;
                    } else {
                        self.gps_locked = !self.gps_locked;
                        self.dirty = true;
                    }
                }
            },
            _ => (),
        }

        // Block multi-tap if last action didn't result in a tap.
        if self.touch_state.action != TouchAction::Tap {
            self.touch_state.last_time = 0;
        }

        // Require all slots to be cleared to allow moving the map again.
        if self.touch_state.slots.is_empty() {
            self.touch_state.action = TouchAction::None;
        }
    }

    #[cfg_attr(feature = "profiling", profiling::function)]
    fn update_config(&mut self, config: &Config) {
        self.dirty |= self.tiles.update_config(config);

        if self.input_config != config.input {
            self.input_config = config.input;
            self.dirty = true;
        }
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// Touch event tracking.
#[derive(Default)]
struct TouchState {
    slots: HashMap<i32, TouchSlot>,
    action: TouchAction,

    long_press_token: Option<RegistrationToken>,
    last_point: Point<f64>,
    last_time: u32,

    move_velocity: Velocity,

    zoom_velocity: Velocity,
    zoom_velocity_distance: f64,
    velocity_zooming_in: bool,
    zoom_focus: Point<f64>,
}

impl TouchState {
    /// Stage time for long-press touch event.
    fn stage_long_press(
        &mut self,
        event_loop: &LoopHandle<'static, State>,
        input_config: &Input,
        point: Point<f64>,
    ) {
        // Clear any previous timeouts.
        self.clear_long_press(event_loop);

        // Stage new callback.
        let timer = Timer::from_duration(*input_config.long_press);
        let token = event_loop.insert_source(timer, move |_, _, state| {
            let map_view = state.window.views.map();
            map_view.trigger_long_press(point);
            TimeoutAction::Drop
        });

        self.long_press_token =
            token.inspect_err(|err| error!("Failed to stage long-press timer: {err}")).ok();
    }

    /// Cancel active long-press timer.
    fn clear_long_press(&mut self, event_loop: &LoopHandle<'static, State>) {
        if let Some(token) = self.long_press_token.take() {
            event_loop.remove(token);
        }
    }
}

/// Touch slot state.
#[derive(Copy, Clone, Default, Debug)]
struct TouchSlot {
    start: Point<f64>,
    point: Point<f64>,
}

/// Intention of a touch sequence.
#[derive(PartialEq, Eq, Default)]
enum TouchAction {
    #[default]
    None,

    DoubleTap,
    Search,
    Drag,
    Zoom,
    Gps,
    Tap,
}

/// Geographic point with a tile location cache.
#[derive(PartialEq, Copy, Clone)]
struct RenderGeoPoint {
    point: GeoPoint,
    cached: Option<(TileIndex, Point)>,
}

impl RenderGeoPoint {
    /// Get the tile index and offset for this point.
    fn tile(&mut self, z: u8) -> (TileIndex, Point) {
        match self.cached {
            Some(cached) if cached.0.z == z => cached,
            _ => *self.cached.insert(self.point.tile(z)),
        }
    }
}

impl From<GeoPoint> for RenderGeoPoint {
    fn from(point: GeoPoint) -> Self {
        Self { point, cached: Default::default() }
    }
}

/// Find the segment in a route closest to a point.
///
/// A segment is defined as two consecutive nodes. The first and last node are
/// part of one segment, all other nodes are part of two segments.
///
/// When multiple solutions have identical distances, this will always return
/// the segment with the highest indices.
#[cfg_attr(feature = "profiling", profiling::function)]
fn nearest_route_segment(route: &[RenderGeoPoint], point: GeoPoint) -> (usize, u32) {
    let mut min_naive_distance = f64::MAX;
    let mut min_nearest = None;
    let mut min_index = 0;

    // Abort search if we're no longer finding any closer nodes.
    const MAX_BAD_SEGMENTS: usize = 15;
    let mut bad_segments = 0;

    // Find segment with smallest distance.
    for i in 1..route.len() {
        // Get approximate flat earth distance to the segment.
        let nearest = nearest_point(route[i - 1].point, route[i].point, point);
        let delta_lat = (point.lat - nearest.lat).powi(2);
        let delta_lon = (point.lon - nearest.lon).powi(2);
        let distance = delta_lat + delta_lon;

        // Update smallest found segment.
        if distance <= min_naive_distance {
            min_naive_distance = distance;
            min_nearest = Some(nearest);
            min_index = i - 1;
        } else {
            bad_segments += 1;
        }

        if bad_segments > MAX_BAD_SEGMENTS {
            break;
        }
    }

    match min_nearest {
        Some(min_nearest) => (min_index, min_nearest.distance(point)),
        // Ignore routes with no segments.
        None => (0, u32::MAX),
    }
}

/// Get the closest point on a segment for a point.
///
/// This does not take the earth's curvature into account,
/// so it will be inaccurate for long segments.
fn nearest_point(start: GeoPoint, end: GeoPoint, point: GeoPoint) -> GeoPoint {
    // Handle zero-length segments.
    if start == end {
        return start;
    }

    // Use squared segment length, to avoid sqrt.
    let squared_lat = (end.lat - start.lat).powi(2);
    let squared_lon = (end.lon - start.lon).powi(2);
    let squared_length = squared_lat + squared_lon;

    // Calculate distance between start and end for the projection point.
    let projection_distance = ((point.lat - start.lat) * (end.lat - start.lat)
        + (point.lon - start.lon) * (end.lon - start.lon))
        / squared_length;

    // Clamp projection point distance on segment between start and end.
    let projection_distance = projection_distance.clamp(0., 1.);

    // Get position of the projection point.
    let projection_point_lat = start.lat + projection_distance * (end.lat - start.lat);
    let projection_point_lon = start.lon + projection_distance * (end.lon - start.lon);
    GeoPoint::new(projection_point_lat, projection_point_lon)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_segment_broken_route() {
        let (index, distance) = nearest_route_segment(&[], GeoPoint::new(0., 0.));
        assert_eq!(distance, u32::MAX);
        assert_eq!(index, 0);

        let (index, distance) =
            nearest_route_segment(&[GeoPoint::new(10., 10.).into()], GeoPoint::new(0., 0.));
        assert_eq!(distance, u32::MAX);
        assert_eq!(index, 0);
    }

    #[test]
    fn nearest_segment_tiny_route() {
        let route = vec![GeoPoint::new(1., 1.).into(), GeoPoint::new(0., 0.).into()];
        let (index, distance) = nearest_route_segment(&route, GeoPoint::new(0., 0.));
        assert_eq!(distance, 0);
        assert_eq!(index, 0);

        let route = vec![GeoPoint::new(0., 0.).into(), GeoPoint::new(1., 1.).into()];
        let (index, distance) = nearest_route_segment(&route, GeoPoint::new(0., 0.));
        assert_eq!(distance, 0);
        assert_eq!(index, 0);
    }

    #[test]
    fn nearest_segment_edge_segment() {
        let route = vec![
            GeoPoint::new(0., 0.).into(),
            GeoPoint::new(2., 2.).into(),
            GeoPoint::new(3., 3.).into(),
        ];
        let (index, distance) = nearest_route_segment(&route, GeoPoint::new(0., 0.));
        assert_eq!(distance, distance);
        assert_eq!(index, 0);

        let route = vec![
            GeoPoint::new(3., 3.).into(),
            GeoPoint::new(2., 2.).into(),
            GeoPoint::new(1., 1.).into(),
        ];
        let (index, distance) = nearest_route_segment(&route, GeoPoint::new(0., 0.));
        assert_eq!(distance, distance);
        assert_eq!(index, 1);
    }

    #[test]
    fn nearest_segment_center() {
        let route = vec![
            GeoPoint::new(-1., -1.).into(),
            GeoPoint::new(0., 0.).into(),
            GeoPoint::new(0.5, 0.5).into(),
            GeoPoint::new(1.5, 1.5).into(),
        ];
        let (index, distance) = nearest_route_segment(&route, GeoPoint::new(0., 0.));
        assert_eq!(distance, 0);
        assert_eq!(index, 1);
    }

    #[test]
    fn nearest_segment_local_optima() {
        let route = vec![
            GeoPoint::new(5., 5.).into(),
            GeoPoint::new(4., 4.).into(),
            GeoPoint::new(3., 3.).into(),
            GeoPoint::new(99., 99.).into(), //  !
            GeoPoint::new(99., 99.).into(),
            GeoPoint::new(99., 99.).into(), // +2
            GeoPoint::new(99., 99.).into(),
            GeoPoint::new(99., 99.).into(),
            GeoPoint::new(99., 99.).into(),
            GeoPoint::new(0., 0.).into(), //   +4
            GeoPoint::new(99., 99.).into(),
        ];
        let (index, distance) = nearest_route_segment(&route, GeoPoint::new(0., 0.));
        assert_eq!(distance, 0);
        assert_eq!(index, 9);

        let route = vec![
            GeoPoint::new(5., 5.).into(),
            GeoPoint::new(4., 4.).into(),
            GeoPoint::new(3., 3.).into(),
            GeoPoint::new(99., 99.).into(), //  !
            GeoPoint::new(99., 99.).into(),
            GeoPoint::new(99., 99.).into(), // +2
            GeoPoint::new(-1., -1.).into(),
            GeoPoint::new(0., 0.).into(),
            GeoPoint::new(1., 1.).into(),
            GeoPoint::new(2., 2.).into(), //  + 4
            GeoPoint::new(99., 99.).into(),
        ];
        let (index, distance) = nearest_route_segment(&route, GeoPoint::new(0., 0.));
        assert_eq!(distance, 0);
        assert_eq!(index, 7);
    }

    #[test]
    fn nearest_segment_identical_nodes() {
        let route = vec![GeoPoint::new(0., 0.).into(); 10];
        let (index, distance) = nearest_route_segment(&route, GeoPoint::new(0., 0.));
        assert_eq!(distance, 0);
        assert_eq!(index, 8);
    }

    #[test]
    fn nearest_segment_equal_distance_separate_nodes() {
        let route = vec![
            GeoPoint::new(9., 9.).into(),
            GeoPoint::new(1., 1.).into(),
            GeoPoint::new(1., -1.).into(),
            GeoPoint::new(-1., -1.).into(),
            GeoPoint::new(-1., 1.).into(),
            GeoPoint::new(1., 1.).into(),
            GeoPoint::new(9., 9.).into(),
        ];
        let origin = GeoPoint::new(0., 0.);
        let (index, distance) = nearest_route_segment(&route, origin);
        assert_eq!(distance, origin.distance(GeoPoint::new(0., 1.)));
        assert_eq!(index, 4);
    }

    #[test]
    fn nearest_segment_closer_segment_farther_node() {
        let route = vec![
            GeoPoint::new(0., 0.).into(),
            GeoPoint::new(10., 0.).into(),
            GeoPoint::new(10., 3.).into(),
            GeoPoint::new(5., 3.).into(),
            GeoPoint::new(99., 99.).into(),
        ];

        let origin = GeoPoint::new(5., 0.);
        let (index, distance) = nearest_route_segment(&route, origin);
        assert_eq!(distance, 0);
        assert_eq!(index, 0);
    }

    #[test]
    fn nearest_segment_parallel() {
        let route = vec![
            GeoPoint::new(0., 0.).into(),
            GeoPoint::new(1., 0.).into(),
            GeoPoint::new(2., 0.).into(),
            GeoPoint::new(3., 0.).into(),
            GeoPoint::new(4., 0.).into(),
        ];

        let origin = GeoPoint::new(0., 1.);
        let (index, distance) = nearest_route_segment(&route, origin);
        assert_eq!(distance, GeoPoint::new(0., 0.).distance(origin));
        assert_eq!(index, 0);

        let origin = GeoPoint::new(2.5, 1.);
        let (index, distance) = nearest_route_segment(&route, origin);
        assert_eq!(distance, GeoPoint::new(2.5, 0.).distance(origin));
        assert_eq!(index, 2);

        let origin = GeoPoint::new(4., 1.);
        let (index, distance) = nearest_route_segment(&route, origin);
        assert_eq!(distance, GeoPoint::new(4., 0.).distance(origin));
        assert_eq!(index, 3);
    }

    #[test]
    fn nearest_segment_on_route() {
        let route = vec![
            GeoPoint::new(0., 0.).into(),
            GeoPoint::new(1., 0.).into(),
            GeoPoint::new(2., 0.).into(),
            GeoPoint::new(3., 0.).into(),
            GeoPoint::new(4., 0.).into(),
        ];

        let (index, distance) = nearest_route_segment(&route, GeoPoint::new(0., 0.));
        assert_eq!(distance, 0);
        assert_eq!(index, 0);

        let (index, distance) = nearest_route_segment(&route, GeoPoint::new(2.5, 0.));
        assert_eq!(distance, 0);
        assert_eq!(index, 2);

        let (index, distance) = nearest_route_segment(&route, GeoPoint::new(4., 0.));
        assert_eq!(distance, 0);
        assert_eq!(index, 3);
    }

    #[test]
    fn nearest_segment_beyond_route() {
        let route = vec![
            GeoPoint::new(0., 0.).into(),
            GeoPoint::new(1., 0.).into(),
            GeoPoint::new(2., 0.).into(),
        ];

        let origin = GeoPoint::new(-1., 0.);
        let (index, distance) = nearest_route_segment(&route, GeoPoint::new(-1., 0.));
        assert_eq!(distance, route[0].point.distance(origin));
        assert_eq!(index, 0);

        let origin = GeoPoint::new(3., 0.);
        let (index, distance) = nearest_route_segment(&route, GeoPoint::new(3., 0.));
        assert_eq!(distance, route[2].point.distance(origin));
        assert_eq!(index, 1);
    }

    #[test]
    fn nearest_segment_real_route() {
        let route = vec![
            GeoPoint::new(51.504314, 7.058997).into(),
            GeoPoint::new(51.504311, 7.059138).into(),
            GeoPoint::new(51.504311, 7.059178).into(),
            GeoPoint::new(51.504311, 7.059279).into(),
            GeoPoint::new(51.504311, 7.059279).into(),
            GeoPoint::new(51.503564, 7.059259).into(),
        ];
        let origin = GeoPoint::new(51.504086, 7.0592);

        let (index, distance) = nearest_route_segment(&route, origin);

        assert_eq!(distance, 5);
        assert_eq!(index, 4);
    }
}
