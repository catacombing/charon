//! Map rendering UI view.

use std::any::Any;
use std::collections::HashMap;
use std::mem;

use calloop::channel::{self, Event};
use calloop::timer::{TimeoutAction, Timer};
use calloop::{LoopHandle, RegistrationToken};
use reqwest::Client;
use skia_safe::{Color4f, FilterMode, MipmapMode, Paint, Rect, SamplingOptions};
use tracing::error;

use crate::config::{Config, Input};
use crate::geometry::{GeoPoint, Point, Size};
use crate::tiles::{MAX_ZOOM, TILE_SIZE, TileIndex, TileIter, Tiles};
use crate::ui::skia::RenderState;
use crate::ui::view::search::SearchView;
use crate::ui::view::{UiView, View};
use crate::ui::{Button, Svg, Velocity};
use crate::{Error, State};

/// Search button width and height at scale 1.
const SEARCH_BUTTON_SIZE: u32 = 48;

/// Padding around the search button at scale 1.
const SEARCH_BUTTON_PADDING: u32 = 16;

/// Border around the search button at scale 1.
const SEARCH_BUTTON_BORDER: f64 = 1.;

/// Attribution label font size relative to the default.
const ATTRIBUTION_FONT_SIZE: f32 = 0.5;

/// POI circle radius at scale 1.
const POI_RADIUS: f32 = 5.;

/// POI border size at scale 1.
const POI_BORDER: f32 = 2.;

/// Map rendering UI view.
pub struct MapView {
    pending_tiles: Vec<TileIndex>,
    tiles: Tiles,
    poi_tile: Option<TileIndex>,
    poi_offset: Option<Point>,
    poi: Option<GeoPoint>,

    cursor_tile: TileIndex,
    cursor_offset: Point,
    cursor_zoom: f64,

    search_button: Button,
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
        config: &Config,
        size: Size,
    ) -> Result<Self, Error> {
        // Initialize the tile cache.
        let (tile_tx, tile_rx) = channel::channel();
        event_loop.insert_source(tile_rx, |event, _, state| {
            let map_view: &mut Self = state.window.views.get_mut(View::Map).unwrap();
            if let Event::Msg(tile_index) = event
                && map_view.pending_tiles.contains(&tile_index)
            {
                map_view.dirty = true;

                if state.window.views.dirty() {
                    state.window.unstall();
                }
            }
        })?;
        let tiles = Tiles::new(client, tile_tx, config)?;

        // Set (0, 0) start location at a zoom level without empty space.
        let (cursor_tile, cursor_offset) = GeoPoint::new(0., 0.).tile(3);

        // Initialize UI elements.
        let point = Self::search_button_point(size, 1.);
        let size = Self::search_button_size(1.);
        let search_button = Button::new(point, size, Svg::Search);

        let mut tile_paint = Paint::default();
        tile_paint.set_color4f(Color4f::from(config.colors.background), None);

        Ok(Self {
            cursor_offset,
            search_button,
            cursor_tile,
            event_loop,
            tile_paint,
            tiles,
            size,
            input_config: config.input,
            dirty: true,
            scale: 1.,
            pending_tiles: Default::default(),
            cursor_zoom: Default::default(),
            touch_state: Default::default(),
            poi_offset: Default::default(),
            poi_tile: Default::default(),
            poi: Default::default(),
        })
    }

    /// Access the tile storage.
    pub fn tiles(&self) -> &Tiles {
        &self.tiles
    }

    /// Get the geographic location an the center of the screen.
    pub fn geographic_point(&self) -> GeoPoint {
        GeoPoint::from_tile(self.cursor_tile, self.cursor_offset)
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
            self.dirty = true;
        }
    }

    /// Highlight a specific point on the map.
    pub fn set_poi(&mut self, point: Option<GeoPoint>) {
        self.dirty |= self.poi != point;
        self.poi_offset = None;
        self.poi_tile = None;
        self.poi = point;
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
            let search_view: &mut SearchView = state.window.views.get_mut(View::Search).unwrap();
            search_view.reverse(geo_point, tile_index.z);
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

        // Calculate offset required to keep zoom focus stationary.
        //
        // Rounding and precision here means the focus point isn't kept precisely in the
        // same position, which is fine for our usecase.
        //
        // The rounding is required to avoid insignificant negative changes being
        // floored to integer cursor_offset changes and causing the map to move around
        // when moving the zoom points without changing their distance.
        let size = self.size * self.scale;
        let center = Point::new(size.width as f64, size.height as f64) / 2.;
        let focus_delta = self.touch_state.zoom_focus - center;
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

    /// Physical location of the search button.
    fn search_button_point(size: Size, scale: f64) -> Point {
        let padding = (SEARCH_BUTTON_PADDING as f64 * scale).round() as i32;
        let button_size = Self::search_button_size(scale);
        let physical_size = size * scale;

        let x = (physical_size.width - button_size.width) as i32 - padding;
        let y = (physical_size.height - button_size.height) as i32 - padding;

        Point::new(x, y)
    }

    /// Physical size of the search button.
    fn search_button_size(scale: f64) -> Size {
        Size::new(SEARCH_BUTTON_SIZE, SEARCH_BUTTON_SIZE) * scale
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
            let bg = config.colors.background;
            let mut builder = render_state.paragraph(bg, ATTRIBUTION_FONT_SIZE, None);
            builder.add_text(&*config.tiles.attribution);

            let mut paragraph = builder.build();
            paragraph.layout(size.width as f32);
            paragraph.paint(&render_state, Point::new(0., 0.));
        }

        // Draw POI if visible.
        if let Some(poi) = self.poi {
            // Convert geographic point to tile index + offset and cache it.
            let (tile, offset) = match (self.poi_tile, self.poi_offset) {
                (Some(tile), Some(offset)) if tile.z == self.cursor_tile.z => (tile, offset),
                _ => {
                    let (tile, offset) = poi.tile(self.cursor_tile.z);
                    self.poi_tile = Some(tile);
                    self.poi_offset = Some(offset);
                    (tile, offset)
                },
            };

            if let Some(point) = iter.screen_point(tile, offset) {
                let poi_radius = POI_RADIUS * self.scale as f32;
                let border_radius = poi_radius + POI_BORDER * self.scale as f32;

                self.tile_paint.set_color4f(Color4f::from(config.colors.background), None);
                render_state.draw_circle(point, border_radius, &self.tile_paint);

                self.tile_paint.set_color4f(Color4f::from(config.colors.highlight), None);
                render_state.draw_circle(point, poi_radius, &self.tile_paint);
            }
        }

        // Draw search button with a border to distinguish it from the map.

        let search_point: Point<f32> = Self::search_button_point(self.size, self.scale).into();
        let search_size: Size<f32> = Self::search_button_size(self.scale).into();
        let search_border = (SEARCH_BUTTON_BORDER * self.scale) as f32;

        let search_left = search_point.x - search_border;
        let search_top = search_point.y - search_border;
        let search_right = search_point.x + search_size.width + search_border;
        let search_bottom = search_point.y + search_size.height + search_border;
        let border_rect = Rect::new(search_left, search_top, search_right, search_bottom);

        self.tile_paint.set_color4f(Color4f::from(config.colors.background), None);
        render_state.draw_rect(border_rect, &self.tile_paint);

        self.search_button.draw(&mut render_state, config.colors.alt_background);

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
    }

    #[cfg_attr(feature = "profiling", profiling::function)]
    fn set_scale_factor(&mut self, scale: f64) {
        self.scale = scale;
        self.dirty = true;

        // Update UI elements.
        self.search_button.set_point(Self::search_button_point(self.size, scale));
        self.search_button.set_size(Self::search_button_size(scale));
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
            TouchAction::Search | TouchAction::None => (),
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
            let map_view: &mut MapView = state.window.views.get_mut(View::Map).unwrap();
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
    Tap,
}
