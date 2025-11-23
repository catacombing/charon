//! Wayland window management.

use std::collections::HashMap;
use std::mem;
use std::ptr::NonNull;
use std::sync::Arc;

use calloop::LoopHandle;
use calloop::channel::{self, Event};
use glutin::display::{Display, DisplayApiPreference};
use raw_window_handle::{RawDisplayHandle, WaylandDisplayHandle};
use skia_safe::textlayout::{FontCollection, ParagraphBuilder, ParagraphStyle, TextStyle};
use skia_safe::{Color4f, FilterMode, FontMgr, MipmapMode, Paint, Rect, SamplingOptions};
use smithay_client_toolkit::compositor::{CompositorState, Region};
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::wp_viewport::WpViewport;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::xdg::window::{Window as XdgWindow, WindowDecorations};

use crate::config::{Color, Config, Input};
use crate::geometry::{GeoPoint, Point, Size};
use crate::tiles::{MAX_ZOOM, TILE_SIZE, TileIndex, TileIter, Tiles};
use crate::ui::Velocity;
use crate::ui::renderer::Renderer;
use crate::ui::skia::Canvas;
use crate::wayland::ProtocolStates;
use crate::{Error, State};

/// Wayland window.
pub struct Window {
    pub queue: QueueHandle<State>,

    connection: Connection,
    xdg_window: XdgWindow,
    viewport: WpViewport,
    renderer: Renderer,

    background: Color,
    tile_paint: Paint,
    canvas: Canvas,

    pending_tiles: Vec<TileIndex>,
    tiles: Tiles,

    cursor_tile: TileIndex,
    cursor_offset: Point,
    cursor_zoom: f64,

    touch_state: TouchState,
    input_config: Input,

    paragraph_style: ParagraphStyle,
    font_collection: FontCollection,
    font_family: Arc<String>,
    text_style: TextStyle,
    text_paint: Paint,
    font_size: f32,

    attribution: Arc<String>,

    initial_draw_done: bool,
    stalled: bool,
    dirty: bool,
    size: Size,
    scale: f64,
}

impl Window {
    pub fn new(
        event_loop: &LoopHandle<'static, State>,
        protocol_states: &ProtocolStates,
        connection: Connection,
        queue: QueueHandle<State>,
        config: &Config,
    ) -> Result<Self, Error> {
        // Get EGL display.
        let display = NonNull::new(connection.backend().display_ptr().cast()).unwrap();
        let wayland_display = WaylandDisplayHandle::new(display);
        let raw_display = RawDisplayHandle::Wayland(wayland_display);
        let egl_display = unsafe { Display::new(raw_display, DisplayApiPreference::Egl)? };

        // Create surface's Wayland global handles.
        let surface = protocol_states.compositor.create_surface(&queue);
        if let Some(fractional_scale) = &protocol_states.fractional_scale {
            fractional_scale.fractional_scaling(&queue, &surface);
        }
        let viewport = protocol_states.viewporter.viewport(&queue, &surface);

        // Create the XDG shell window.
        let xdg_window = protocol_states.xdg_shell.create_window(
            surface.clone(),
            WindowDecorations::RequestClient,
            &queue,
        );
        xdg_window.set_title("Charon");
        xdg_window.set_app_id("Charon");
        xdg_window.commit();

        // Create OpenGL renderer.
        let renderer = Renderer::new(egl_display, surface);

        // Default to a reasonable default size.
        let size = Size { width: 360, height: 720 };

        // Initialize the tile cache.
        let (tile_tx, tile_rx) = channel::channel();
        event_loop.insert_source(tile_rx, |event, _, state| {
            if let Event::Msg(tile_index) = event
                && state.window.pending_tiles.contains(&tile_index)
            {
                state.window.dirty = true;
                state.window.unstall();
            }
        })?;
        let tiles = Tiles::new(tile_tx, config)?;

        // Set (0, 0) start location at a zoom level without empty space.
        let (cursor_tile, cursor_offset) = GeoPoint::new(0., 0.).tile(3);

        // Initialize text rendering config.

        let mut text_paint = Paint::default();
        text_paint.set_color4f(Color4f::from(config.colors.foreground), None);
        text_paint.set_anti_alias(true);

        let font_family = config.font.family.clone();
        let mut text_style = TextStyle::new();
        text_style.set_foreground_paint(&text_paint);
        text_style.set_font_size(config.font.size);
        text_style.set_font_families(&[&*font_family]);

        let mut paragraph_style = ParagraphStyle::new();
        paragraph_style.set_text_style(&text_style);
        paragraph_style.set_ellipsis("…");

        let mut font_collection = FontCollection::new();
        font_collection.set_default_font_manager(FontMgr::new(), None);

        Ok(Self {
            font_collection,
            paragraph_style,
            cursor_offset,
            cursor_tile,
            font_family,
            connection,
            text_paint,
            text_style,
            xdg_window,
            viewport,
            renderer,
            queue,
            tiles,
            size,
            attribution: config.tiles.attribution.clone(),
            background: config.colors.background,
            font_size: config.font.size,
            input_config: config.input,
            stalled: true,
            dirty: true,
            scale: 1.,
            initial_draw_done: Default::default(),
            pending_tiles: Default::default(),
            cursor_zoom: Default::default(),
            touch_state: Default::default(),
            tile_paint: Default::default(),
            canvas: Default::default(),
        })
    }

    /// Redraw the window.
    #[cfg_attr(feature = "profiling", profiling::function)]
    pub fn draw(&mut self) {
        // Notify profiler about frame start.
        #[cfg(feature = "profiling")]
        profiling::finish_frame!();

        // Stall rendering if nothing changed since last redraw.
        if !self.dirty() {
            self.stalled = true;
            return;
        }
        self.initial_draw_done = true;
        self.dirty = false;

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

        // Update viewporter logical render size.
        //
        // NOTE: This must be done every time we draw with Sway; it is not
        // persisted when drawing with the same surface multiple times.
        self.viewport.set_destination(self.size.width as i32, self.size.height as i32);

        // Mark entire window as damaged.
        let wl_surface = self.xdg_window.wl_surface();
        wl_surface.damage(0, 0, self.size.width as i32, self.size.height as i32);

        // Reset tiles pending download.
        self.pending_tiles.clear();

        // Get sub-tile map scale.
        let scale = self.zoom_scale();

        // Render the window content.
        let size = self.size * self.scale;
        self.renderer.draw(size, |renderer| {
            self.canvas.draw(renderer.skia_config(), size, |canvas| {
                canvas.clear(self.background);

                // Create iterator over visible tiles.
                let mut iter = TileIter::new(size, self.cursor_tile, self.cursor_offset, scale);
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
                    canvas.draw_image_rect_with_sampling_options(
                        image,
                        None,
                        dst_rect,
                        sampling,
                        &self.tile_paint,
                    );
                }

                // Render attribution message.
                if !self.attribution.is_empty() {
                    let mut builder =
                        ParagraphBuilder::new(&self.paragraph_style, &self.font_collection);
                    builder.add_text(&*self.attribution);

                    let mut paragraph = builder.build();
                    paragraph.layout(size.width as f32);
                    paragraph.paint(canvas, Point::new(0., 0.));
                }

                // If no downloads are pending, pre-download tiles just outside the viewport.
                #[cfg(feature = "profiling")]
                profiling::scope!("fetch_background_tiles");
                if self.pending_tiles.is_empty() {
                    for index in iter.border_tiles() {
                        self.tiles.preload(index);
                    }
                }
            });
        });

        // Request a new frame.
        wl_surface.frame(&self.queue, wl_surface.clone());

        // Apply surface changes.
        wl_surface.commit();
    }

    /// Perform draw for the initial commit.
    pub fn perform_initial_draw(&mut self) {
        if !self.initial_draw_done {
            self.draw();
        }
    }

    /// Unstall the renderer.
    ///
    /// This will render a new frame if there currently is no frame request
    /// pending.
    pub fn unstall(&mut self) {
        // Ignore if unstalled or request came from background engine.
        if !mem::take(&mut self.stalled) {
            return;
        }

        // Redraw immediately to unstall rendering.
        self.draw();
        let _ = self.connection.flush();
    }

    /// Update the window's logical size.
    pub fn set_size(&mut self, compositor: &CompositorState, size: Size) {
        if self.size == size {
            return;
        }

        self.size = size;
        self.dirty = true;

        // Update the window's opaque region.
        //
        // This is done here since it can only change on resize, but the commit happens
        // atomically on redraw.
        if let Ok(region) = Region::new(compositor) {
            region.add(0, 0, size.width as i32, size.height as i32);
            self.xdg_window.wl_surface().set_opaque_region(Some(region.wl_region()));
        }

        self.unstall();
    }

    /// Update the window's DPI factor.
    pub fn set_scale_factor(&mut self, scale: f64) {
        if self.scale == scale {
            return;
        }

        self.scale = scale;
        self.dirty = true;

        // Update text size on scale change.
        self.text_style.set_font_size(self.font_size * self.scale as f32);
        self.paragraph_style.set_text_style(&self.text_style);

        self.unstall();
    }

    /// Handle config updates.
    #[cfg_attr(feature = "profiling", profiling::function)]
    pub fn update_config(&mut self, config: &Config) {
        self.dirty |= self.tiles.update_config(config);

        if self.background != config.colors.background {
            self.background = config.colors.background;
            self.dirty = true;
        }

        if self.input_config != config.input {
            self.input_config = config.input;
            self.dirty = true;
        }

        if self.attribution != config.tiles.attribution {
            self.attribution = config.tiles.attribution.clone();
            self.dirty = true;
        }

        // Handle text rendering changes.

        let mut text_dirty = false;

        let foreground = config.colors.foreground.into();
        if self.text_paint.color4f() != foreground {
            self.text_paint.set_color4f(foreground, None);
            self.text_style.set_foreground_paint(&self.text_paint);
            text_dirty = true;
        }

        if self.font_size != config.font.size {
            self.text_style.set_font_size(config.font.size * self.scale as f32);
            self.font_size = config.font.size;
            text_dirty = true;
        }

        if self.font_family != config.font.family {
            self.font_family = config.font.family.clone();
            self.text_style.set_font_families(&[&*self.font_family]);
            text_dirty = true;
        }

        if text_dirty {
            self.paragraph_style.set_text_style(&self.text_style);
        }
        self.dirty |= text_dirty;

        if self.dirty {
            self.unstall();
        }
    }

    /// Handle touch press.
    pub fn touch_down(&mut self, slot: i32, point: Point<f64>) {
        // Cancel velocity if a new touch sequence starts.
        self.touch_state.move_velocity.stop();
        self.touch_state.zoom_velocity.stop();

        // Only allow at most 2 touch slots at a time.
        match self.touch_state.slots.len() {
            0 => self.touch_state.action = TouchAction::Tap,
            1 => self.touch_state.action = TouchAction::Zoom,
            _ => return,
        }

        let slot = self.touch_state.slots.entry(slot).or_default();

        // Convert touch point to physical space.
        let point = point * self.scale;
        slot.point = point;
        slot.start = point;
    }

    /// Handle touch motion.
    pub fn touch_motion(&mut self, id: i32, point: Point<f64>) {
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
            TouchAction::Tap | TouchAction::Drag => {
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
            TouchAction::None => (),
        }
    }

    /// Handle touch release.
    pub fn touch_up(&mut self, slot: i32) {
        let removed = self.touch_state.slots.remove(&slot);

        // On tap, snap zoom to nearest integer scale.
        if removed.is_some() && self.touch_state.action == TouchAction::Tap {
            self.snap_zoom();
        }

        // Require all slots to be cleared to allow moving the map again.
        if self.touch_state.slots.is_empty() {
            self.touch_state.action = TouchAction::None;
        }
    }

    /// Check whether the window requires a redraw.
    fn dirty(&self) -> bool {
        self.dirty
            || self.touch_state.move_velocity.is_moving()
            || self.touch_state.zoom_velocity.is_moving()
    }

    /// Move the map by a pixel delta.
    fn move_by(&mut self, delta: Point<f64>) {
        if delta == Point::default() {
            return;
        }

        // Apply sub-tile scale, since the cursor is in tile coordinates.
        let scale = self.zoom_scale();
        let x = (-delta.x / scale).round() as i32;
        let y = (-delta.y / scale).round() as i32;

        let max_tile = (1 << self.cursor_tile.z) - 1;
        let offset_x = self.cursor_offset.x + x;
        let offset_y = self.cursor_offset.y + y;

        // Update center tile.
        let tile_x = self.cursor_tile.x as i32 + offset_x.div_euclid(TILE_SIZE);
        let tile_y = self.cursor_tile.y as i32 + offset_y.div_euclid(TILE_SIZE);
        self.cursor_tile.x = tile_x.clamp(0, max_tile) as u32;
        self.cursor_tile.y = tile_y.clamp(0, max_tile) as u32;

        // Update offset within center tile.
        let clamp_offset = |tile: i32, offset: i32| {
            if tile > max_tile {
                255
            } else if tile < 0 {
                0
            } else {
                offset.rem_euclid(TILE_SIZE)
            }
        };
        self.cursor_offset.x = clamp_offset(tile_x, offset_x);
        self.cursor_offset.y = clamp_offset(tile_y, offset_y);

        self.dirty = true;
        self.unstall();
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
        self.unstall();
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
        self.unstall();
    }

    /// Get the current sub-tile zoom level.
    ///
    /// A value of 1.5 means tiles should be rendered at 150% of their size.
    /// This value will always be between 0.5 and 2.0, never reaching either
    /// bound.
    fn zoom_scale(&self) -> f64 {
        (2f64.powf(self.cursor_zoom) * 100.).round() / 100.
    }
}

/// Touch event tracking.
#[derive(Default)]
struct TouchState {
    slots: HashMap<i32, TouchSlot>,
    action: TouchAction,

    move_velocity: Velocity,

    zoom_velocity: Velocity,
    zoom_velocity_distance: f64,
    velocity_zooming_in: bool,
    zoom_focus: Point<f64>,
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
    Drag,
    Zoom,
    Tap,
}
