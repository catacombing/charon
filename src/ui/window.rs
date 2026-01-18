//! Wayland window management.

use std::mem;
use std::ptr::NonNull;

use _text_input::zwp_text_input_v3::{ChangeCause, ContentHint, ContentPurpose, ZwpTextInputV3};
use calloop::LoopHandle;
use glutin::display::{Display, DisplayApiPreference};
use raw_window_handle::{RawDisplayHandle, WaylandDisplayHandle};
use smithay_client_toolkit::compositor::{CompositorState, Region};
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::text_input::zv3::client as _text_input;
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::wp_viewport::WpViewport;
use smithay_client_toolkit::seat::keyboard::{Keysym, Modifiers};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::xdg::window::{Window as XdgWindow, WindowDecorations};

use crate::config::Config;
use crate::geometry::{Point, Size};
use crate::ui::renderer::Renderer;
use crate::ui::skia::Canvas;
use crate::ui::view::{View, Views};
use crate::wayland::ProtocolStates;
use crate::{Error, State};

/// Wayland window.
pub struct Window {
    pub queue: QueueHandle<State>,
    pub views: Views,

    connection: Connection,
    xdg_window: XdgWindow,
    viewport: WpViewport,

    ime_cause: Option<ChangeCause>,
    text_input: Option<TextInput>,

    renderer: Renderer,
    canvas: Canvas,

    config: Config,

    size: Size,
    scale: f64,

    initial_draw_done: bool,
    text_input_dirty: bool,
    stalled: bool,
    dirty: bool,
}

impl Window {
    pub fn new(
        event_loop: &LoopHandle<'static, State>,
        protocol_states: &ProtocolStates,
        connection: Connection,
        queue: QueueHandle<State>,
        config: Config,
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

        let views = Views::new(event_loop, &config, size)?;
        let canvas = Canvas::new(&config);

        Ok(Self {
            connection,
            xdg_window,
            renderer,
            viewport,
            canvas,
            config,
            queue,
            views,
            size,
            stalled: true,
            dirty: true,
            scale: 1.,
            initial_draw_done: Default::default(),
            text_input_dirty: Default::default(),
            text_input: Default::default(),
            ime_cause: Default::default(),
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

        self.update_text_input();

        // Update viewporter logical render size.
        //
        // NOTE: This must be done every time we draw with Sway; it is not
        // persisted when drawing with the same surface multiple times.
        self.viewport.set_destination(self.size.width as i32, self.size.height as i32);

        // Mark entire window as damaged.
        let wl_surface = self.xdg_window.wl_surface();
        wl_surface.damage(0, 0, self.size.width as i32, self.size.height as i32);

        // Render the window content.
        let size = self.size * self.scale;
        self.renderer.draw(size, |renderer| {
            self.canvas.draw(renderer.skia_config(), size, |render_state| {
                self.views.draw(&self.config, render_state);
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
        if !mem::take(&mut self.stalled) {
            return;
        }

        self.draw();
        let _ = self.connection.flush();
    }

    /// Update the window's logical size.
    pub fn set_size(&mut self, compositor: &CompositorState, size: Size) {
        if self.size == size {
            return;
        }

        // Update both active and inactive views.
        for view in self.views.views_mut() {
            view.set_size(size);
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

        self.canvas.set_scale_factor(scale);

        // Update both active and inactive views.
        for view in self.views.views_mut() {
            view.set_scale_factor(scale);
        }

        self.scale = scale;
        self.dirty = true;

        self.unstall();
    }

    /// Handle config updates.
    #[cfg_attr(feature = "profiling", profiling::function)]
    pub fn update_config(&mut self, config: Config) {
        self.canvas.update_config(&config);

        // Update both active and inactive views.
        for view in self.views.views_mut() {
            view.update_config(&config);
        }

        self.config = config;

        if self.views.dirty() {
            self.unstall();
        }
    }

    /// Handle touch press.
    pub fn touch_down(&mut self, slot: i32, time: u32, point: Point<f64>) {
        self.views.touch_down(slot, time, point);

        if self.views.dirty() {
            self.unstall();
        }
    }

    /// Handle touch motion.
    pub fn touch_motion(&mut self, id: i32, point: Point<f64>) {
        self.views.touch_motion(id, point);

        if self.views.dirty() {
            self.unstall();
        }
    }

    /// Handle touch release.
    pub fn touch_up(&mut self, slot: i32) {
        self.views.touch_up(slot);

        if self.views.dirty() {
            self.unstall();
        }
    }

    /// Handle keyboard focus.
    pub fn keyboard_enter(&mut self) {
        for view in self.views.views_mut() {
            view.keyboard_enter();
        }

        if self.views.dirty() {
            self.unstall();
        }
    }

    /// Handle keyboard focus loss.
    pub fn keyboard_leave(&mut self) {
        for view in self.views.views_mut() {
            view.keyboard_leave();
        }

        if self.views.dirty() {
            self.unstall();
        }
    }

    /// Handle keyboard key press.
    pub fn press_key(&mut self, raw: u32, keysym: Keysym, modifiers: Modifiers) {
        self.views.press_key(raw, keysym, modifiers);

        if self.views.dirty() {
            self.unstall();
        }
    }

    /// Paste text into the window.
    pub fn paste(&mut self, text: &str) {
        self.views.paste(text);

        if self.views.dirty() {
            self.unstall();
        }
    }

    /// Handle IME focus.
    pub fn text_input_enter(&mut self, text_input: ZwpTextInputV3) {
        self.text_input = Some(text_input.into());

        for view in self.views.views_mut() {
            view.text_input_enter();
        }

        if self.views.dirty() {
            self.unstall();
        }
    }

    /// Handle IME focus loss.
    pub fn text_input_leave(&mut self) {
        self.text_input = None;

        for view in self.views.views_mut() {
            view.text_input_leave();
        }

        if self.views.dirty() {
            self.unstall();
        }
    }

    /// Delete text around the current cursor position.
    pub fn delete_surrounding_text(&mut self, before_length: u32, after_length: u32) {
        self.views.delete_surrounding_text(before_length, after_length);

        if self.views.dirty() {
            self.unstall();
        }
    }

    /// Insert text at the current cursor position.
    pub fn commit_string(&mut self, text: String) {
        self.views.commit_string(text);

        if self.views.dirty() {
            self.unstall();
        }
    }

    /// Set preedit text at the current cursor position.
    pub fn set_preedit_string(&mut self, text: String, cursor_begin: i32, cursor_end: i32) {
        self.views.set_preedit_string(text, cursor_begin, cursor_end);

        if self.views.dirty() {
            self.unstall();
        }
    }

    /// Apply pending text input changes.
    fn update_text_input(&mut self) {
        if !self.views.take_text_input_dirty() && !self.text_input_dirty {
            return;
        }

        let text_input = match &mut self.text_input {
            Some(text_input) => text_input,
            None => return,
        };

        if !self.views.text_input_enabled() {
            text_input.disable();
            return;
        }

        text_input.enable();

        let (text, cursor_start, cursor_end) = self.views.surrounding_text();
        text_input.set_surrounding_text(text, cursor_start, cursor_end);

        let cause = self.ime_cause.take().unwrap_or(ChangeCause::InputMethod);
        text_input.set_text_change_cause(cause);

        let content_hint = ContentHint::Completion
            | ContentHint::Spellcheck
            | ContentHint::Multiline
            | ContentHint::AutoCapitalization;
        text_input.set_content_type(content_hint, ContentPurpose::Normal);

        // Update logical cursor rectangle.
        if let Some((point, size)) = self.views.last_cursor_geometry() {
            let point: Point<i32> = point / self.scale;
            let size: Size<i32> = (size / self.scale).into();
            text_input.set_cursor_rectangle(point.x, point.y, size.width, size.height);
        }

        text_input.commit();
    }

    /// Switch to a different UI view.
    pub fn set_view(&mut self, view: View) {
        if self.views.active() == view {
            return;
        }

        self.views.set_view(view);

        // Notify view about getting opened.
        self.views.enter();

        if view == View::Search {
            let map_view = self.views.map();
            let map_center_point = map_view.center_point();
            let map_center_zoom = map_view.zoom();

            // Ensure search reference point matches last map view position.
            let search_view = self.views.search();
            search_view.set_map_center(map_center_point, map_center_zoom);
        }

        self.text_input_dirty = true;
        self.dirty = true;

        self.unstall();
    }

    /// Check whether the window requires a redraw.
    fn dirty(&self) -> bool {
        self.dirty || self.views.dirty()
    }
}

/// Text input with enabled-state tracking.
#[derive(Debug)]
pub struct TextInput {
    text_input: ZwpTextInputV3,
    enabled: bool,
}

impl From<ZwpTextInputV3> for TextInput {
    fn from(text_input: ZwpTextInputV3) -> Self {
        Self { text_input, enabled: false }
    }
}

impl TextInput {
    /// Enable text input on a surface.
    ///
    /// This is automatically debounced if the text input is already enabled.
    ///
    /// Does not automatically send a commit, to allow synchronized
    /// initialization of all IME state.
    pub fn enable(&mut self) {
        if self.enabled {
            return;
        }

        self.enabled = true;
        self.text_input.enable();
    }

    /// Disable text input on a surface.
    ///
    /// This is automatically debounced if the text input is already disabled.
    ///
    /// Contrary to `[Self::enable]`, this immediately sends a commit after
    /// disabling IME, since there's no need to synchronize with other
    /// events.
    pub fn disable(&mut self) {
        if !self.enabled {
            return;
        }

        self.enabled = false;
        self.text_input.disable();
        self.text_input.commit();
    }

    /// Set the surrounding text.
    pub fn set_surrounding_text(&self, text: String, cursor_index: i32, selection_anchor: i32) {
        self.text_input.set_surrounding_text(text, cursor_index, selection_anchor);
    }

    /// Indicate the cause of surrounding text change.
    pub fn set_text_change_cause(&self, cause: ChangeCause) {
        self.text_input.set_text_change_cause(cause);
    }

    /// Set text field content purpose and hint.
    pub fn set_content_type(&self, hint: ContentHint, purpose: ContentPurpose) {
        self.text_input.set_content_type(hint, purpose);
    }

    /// Set text field cursor position.
    pub fn set_cursor_rectangle(&self, x: i32, y: i32, width: i32, height: i32) {
        self.text_input.set_cursor_rectangle(x, y, width, height);
    }

    /// Commit IME state.
    pub fn commit(&self) {
        self.text_input.commit();
    }
}
