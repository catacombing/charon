//! OpenGL renderer.

use std::ffi::CString;
use std::num::NonZeroU32;
use std::ptr::NonNull;

use glutin::config::{Api, Config, ConfigTemplateBuilder};
use glutin::context::{ContextApi, ContextAttributesBuilder, PossiblyCurrentContext, Version};
use glutin::display::Display;
use glutin::prelude::*;
use glutin::surface::{Surface, SurfaceAttributesBuilder, SwapInterval, WindowSurface};
use raw_window_handle::{RawWindowHandle, WaylandWindowHandle};
use smithay_client_toolkit::reexports::client::Proxy;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;

use crate::geometry::Size;
use crate::gl;
use crate::ui::skia::GlConfig as SkiaGlConfig;

/// OpenGL renderer.
#[derive(Debug)]
pub struct Renderer {
    sized: Option<SizedRenderer>,
    surface: WlSurface,
    display: Display,
}

impl Renderer {
    /// Initialize a new renderer.
    pub fn new(display: Display, surface: WlSurface) -> Self {
        // Setup OpenGL symbol loader.
        gl::load_with(|symbol| {
            let symbol = CString::new(symbol).unwrap();
            display.get_proc_address(symbol.as_c_str()).cast()
        });

        Renderer { surface, display, sized: Default::default() }
    }

    /// Perform drawing with this renderer mapped.
    #[cfg_attr(feature = "profiling", profiling::function)]
    pub fn draw<F: FnOnce(&SizedRenderer)>(&mut self, size: Size, fun: F) {
        let sized = self.sized(size);
        sized.make_current();

        // Resize OpenGL viewport.
        //
        // This isn't done in `Self::resize` since the renderer must be current.
        unsafe { gl::Viewport(0, 0, size.width as i32, size.height as i32) };

        fun(sized);

        unsafe { gl::Flush() };

        sized.swap_buffers();
    }

    /// Get render state requiring a size.
    fn sized(&mut self, size: Size) -> &SizedRenderer {
        // Initialize or resize sized state.
        match &mut self.sized {
            // Resize renderer.
            Some(sized) => sized.resize(size),
            // Create sized state.
            None => {
                self.sized = Some(SizedRenderer::new(&self.display, &self.surface, size));
            },
        }

        self.sized.as_ref().unwrap()
    }
}

/// Render state requiring known size.
///
/// This state is initialized on-demand, to avoid Mesa's issue with resizing
/// before the first draw.
#[derive(Debug)]
pub struct SizedRenderer {
    egl_surface: Surface<WindowSurface>,
    egl_context: PossiblyCurrentContext,
    egl_config: Config,

    size: Size,
}

impl SizedRenderer {
    /// Create sized renderer state.
    fn new(display: &Display, surface: &WlSurface, size: Size) -> Self {
        // Create EGL surface and context and make it current.
        let (egl_surface, egl_context, egl_config) = Self::create_surface(display, surface, size);

        Self { egl_surface, egl_context, egl_config, size }
    }

    /// Get Skia OpenGL configuration.
    pub fn skia_config(&self) -> SkiaGlConfig {
        SkiaGlConfig {
            stencil_size: self.egl_config.stencil_size() as usize,
            sample_count: self.egl_config.num_samples() as usize,
        }
    }

    /// Resize the renderer.
    fn resize(&mut self, size: Size) {
        if self.size == size {
            return;
        }

        // Resize EGL texture.
        self.egl_surface.resize(
            &self.egl_context,
            NonZeroU32::new(size.width).unwrap(),
            NonZeroU32::new(size.height).unwrap(),
        );

        self.size = size;
    }

    /// Make EGL surface current.
    fn make_current(&self) {
        self.egl_context.make_current(&self.egl_surface).unwrap();
    }

    /// Perform OpenGL buffer swap.
    fn swap_buffers(&self) {
        self.egl_surface.swap_buffers(&self.egl_context).unwrap();
    }

    /// Create a new EGL surface.
    fn create_surface(
        display: &Display,
        surface: &WlSurface,
        size: Size,
    ) -> (Surface<WindowSurface>, PossiblyCurrentContext, Config) {
        assert!(size.width > 0 && size.height > 0);

        // Create EGL config.
        let config_template = ConfigTemplateBuilder::new().with_api(Api::GLES2).build();
        let egl_config = unsafe {
            display
                .find_configs(config_template)
                .ok()
                .and_then(|mut configs| configs.next())
                .unwrap()
        };

        // Create EGL context.
        let context_attributes = ContextAttributesBuilder::new()
            .with_context_api(ContextApi::Gles(Some(Version::new(2, 0))))
            .build(None);
        let egl_context =
            unsafe { display.create_context(&egl_config, &context_attributes).unwrap() };
        let egl_context = egl_context.treat_as_possibly_current();

        let surface = NonNull::new(surface.id().as_ptr().cast()).unwrap();
        let raw_window_handle = WaylandWindowHandle::new(surface);
        let raw_window_handle = RawWindowHandle::Wayland(raw_window_handle);
        let surface_attributes = SurfaceAttributesBuilder::<WindowSurface>::new().build(
            raw_window_handle,
            NonZeroU32::new(size.width).unwrap(),
            NonZeroU32::new(size.height).unwrap(),
        );

        let egl_surface =
            unsafe { display.create_window_surface(&egl_config, &surface_attributes).unwrap() };

        // Ensure rendering never blocks.
        egl_context.make_current(&egl_surface).unwrap();
        egl_surface.set_swap_interval(&egl_context, SwapInterval::DontWait).unwrap();

        (egl_surface, egl_context, egl_config)
    }
}
