//! Skia graphics rendering.

use skia_safe::gpu::gl::{Format, FramebufferInfo, Interface};
use skia_safe::gpu::{
    DirectContext, SurfaceOrigin, backend_render_targets, direct_contexts, surfaces,
};
use skia_safe::{Canvas as SkiaCanvas, ColorType, Surface as SkiaSurface};

use crate::geometry::Size;
use crate::gl;
use crate::gl::types::GLint;

/// OpenGL-based Skia render target.
#[derive(Default)]
pub struct Canvas {
    surface: Option<Surface>,
}

impl Canvas {
    /// Draw to the Skia canvas.
    ///
    /// This will return the underlying OpenGL texture ID.
    pub fn draw<F>(&mut self, gl_config: GlConfig, size: Size, f: F)
    where
        F: FnOnce(&SkiaCanvas),
    {
        // Create Skia surface on-demand.
        let surface = self.surface.get_or_insert_with(|| Surface::new(gl_config, size));

        // Resize surface if necessary.
        surface.resize(gl_config, size);

        // Perform custom rendering operations.
        f(surface.surface.canvas());

        // Flush GPU commands.
        surface.context.flush_and_submit();
    }
}

struct Surface {
    fb_info: FramebufferInfo,
    context: DirectContext,
    surface: SkiaSurface,
    size: Size,
}

impl Surface {
    fn new(gl_config: GlConfig, size: Size) -> Self {
        let interface = Interface::new_native().unwrap();
        let mut context = direct_contexts::make_gl(interface, None).unwrap();

        let fb_info = {
            let mut fboid: GLint = 0;
            unsafe { gl::GetIntegerv(gl::FRAMEBUFFER_BINDING, &mut fboid) };

            FramebufferInfo {
                fboid: fboid.try_into().unwrap(),
                format: Format::RGBA8.into(),
                ..Default::default()
            }
        };

        let surface = Self::create_surface(fb_info, &mut context, gl_config, size);

        Self { context, surface, fb_info, size }
    }

    /// Resize the underlying Skia surface.
    fn resize(&mut self, gl_config: GlConfig, size: Size) {
        if self.size != size {
            self.surface = Self::create_surface(self.fb_info, &mut self.context, gl_config, size);
            self.size = size;
        }
    }

    /// Create a new Skia surface for a framebuffer.
    fn create_surface(
        fb_info: FramebufferInfo,
        context: &mut DirectContext,
        gl_config: GlConfig,
        size: Size,
    ) -> SkiaSurface {
        let size = (size.width as i32, size.height as i32);
        let target = backend_render_targets::make_gl(
            size,
            gl_config.sample_count,
            gl_config.stencil_size,
            fb_info,
        );
        surfaces::wrap_backend_render_target(
            context,
            &target,
            SurfaceOrigin::BottomLeft,
            ColorType::RGBA8888,
            None,
            None,
        )
        .unwrap()
    }
}

/// Skia OpenGL config parameters.
#[derive(Copy, Clone)]
pub struct GlConfig {
    pub stencil_size: usize,
    pub sample_count: usize,
}
