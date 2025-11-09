//! Wayland window management.

use std::mem;
use std::ptr::NonNull;

use glutin::display::{Display, DisplayApiPreference};
use raw_window_handle::{RawDisplayHandle, WaylandDisplayHandle};
use smithay_client_toolkit::compositor::{CompositorState, Region};
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::wp_viewport::WpViewport;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::xdg::window::{Window as XdgWindow, WindowDecorations};

use crate::config::{Color, Config};
use crate::geometry::{Point, Size};
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
    canvas: Canvas,

    initial_draw_done: bool,
    stalled: bool,
    dirty: bool,
    size: Size,
    scale: f64,
}

impl Window {
    pub fn new(
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

        Ok(Self {
            connection,
            xdg_window,
            viewport,
            renderer,
            queue,
            size,
            background: config.colors.background,
            stalled: true,
            dirty: true,
            scale: 1.,
            initial_draw_done: Default::default(),
            canvas: Default::default(),
        })
    }

    /// Redraw the window.
    pub fn draw(&mut self) {
        // Stall rendering if nothing changed since last redraw.
        if !mem::take(&mut self.dirty) {
            self.stalled = true;
            return;
        }
        self.initial_draw_done = true;

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
            self.canvas.draw(renderer.skia_config(), size, |canvas| {
                canvas.clear(self.background);
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

        self.unstall();
    }

    /// Handle config updates.
    pub fn update_config(&mut self, config: &Config) {
        if self.background != config.colors.background {
            self.background = config.colors.background;
            self.dirty = true;
            self.unstall();
        }
    }

    /// Handle touch press.
    pub fn touch_down(&mut self, point: Point<f64>) {
        // TODO
    }

    /// Handle touch motion.
    pub fn touch_motion(&mut self, config: &Config, point: Point<f64>) {
        // TODO
    }

    /// Handle touch release.
    pub fn touch_up(&mut self) {
        // TODO
    }
}
