//! Skia graphics rendering.

use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;

use resvg::tiny_skia::Pixmap as SvgPixmap;
use resvg::usvg::{Options as SvgOptions, Transform as SvgTransform, Tree as SvgTree};
use skia_safe::gpu::ganesh::images as gpu_images;
use skia_safe::gpu::gl::{Format, FramebufferInfo, Interface};
use skia_safe::gpu::{
    Budgeted, DirectContext, Mipmapped, SurfaceOrigin, backend_render_targets, direct_contexts,
    surfaces,
};
use skia_safe::image::images as cpu_images;
use skia_safe::textlayout::{
    FontCollection, ParagraphBuilder, ParagraphStyle, TextAlign, TextDecoration, TextStyle,
};
use skia_safe::{
    AlphaType, Canvas as SkiaCanvas, Color4f, ColorType, Data, FontMgr, Image, ImageInfo, Paint,
    Rect, Surface as SkiaSurface,
};

use crate::config::Config;
use crate::geometry::{Point, Size};
use crate::gl;
use crate::gl::types::GLint;

/// Alpha value for preedit and placeholder text.
const HINT_TEXT_ALPHA: f32 = 0.6;

/// OpenGL-based Skia render target.
pub struct Canvas {
    surface: Option<Surface>,

    font_collection: FontCollection,
    placeholder_style: TextStyle,
    selection_style: TextStyle,
    font_family: Arc<String>,
    preedit_style: TextStyle,
    text_style: TextStyle,
    text_paint: Paint,
    font_size: f32,

    svg_cache: HashMap<SvgCacheKey, Image>,
    svg_paint: Paint,

    scale: f32,
}

impl Canvas {
    pub fn new(config: &Config) -> Self {
        // Initialize text rendering state.

        let mut text_paint = Paint::default();
        text_paint.set_color4f(Color4f::from(config.colors.foreground), None);
        text_paint.set_anti_alias(true);

        let font_family = config.font.family.clone();
        let font_size = config.font.size;

        let mut text_style = TextStyle::new();
        text_style.set_font_families(&[&*font_family]);
        text_style.set_foreground_paint(&text_paint);
        text_style.set_font_size(font_size);

        let mut selection_style = text_style.clone();
        text_paint.set_color4f(Color4f::from(config.colors.background), None);
        selection_style.set_foreground_paint(&text_paint);
        text_paint.set_color4f(Color4f::from(config.colors.highlight), None);
        selection_style.set_background_paint(&text_paint);

        text_paint.set_color4f(Color4f { a: HINT_TEXT_ALPHA, ..text_paint.color4f() }, None);
        let mut placeholder_style = text_style.clone();
        placeholder_style.set_foreground_paint(&text_paint);

        let mut preedit_style = placeholder_style.clone();
        preedit_style.set_decoration_type(TextDecoration::UNDERLINE);

        let font_mgr = FontMgr::new();
        let mut font_collection = FontCollection::new();
        font_collection.set_default_font_manager(font_mgr.clone(), None);

        Self {
            placeholder_style,
            font_collection,
            selection_style,
            preedit_style,
            font_family,
            text_paint,
            text_style,
            font_size,
            svg_paint: Paint::default(),
            scale: 1.,
            svg_cache: Default::default(),
            surface: Default::default(),
        }
    }

    /// Draw to the Skia canvas.
    ///
    /// This will return the underlying OpenGL texture ID.
    #[cfg_attr(feature = "profiling", profiling::function)]
    pub fn draw<F>(&mut self, gl_config: GlConfig, size: Size, f: F)
    where
        F: FnOnce(RenderState),
    {
        // Create Skia surface on-demand.
        let surface = self.surface.get_or_insert_with(|| Surface::new(gl_config, size));

        // Resize surface if necessary.
        surface.resize(gl_config, size);

        // Perform custom rendering operations.
        f(RenderState {
            placeholder_style: &mut self.placeholder_style,
            selection_style: &mut self.selection_style,
            font_collection: &self.font_collection,
            preedit_style: &mut self.preedit_style,
            canvas: surface.surface.canvas(),
            text_paint: &mut self.text_paint,
            text_style: &mut self.text_style,
            svg_cache: &mut self.svg_cache,
            svg_paint: &self.svg_paint,
            font_size: self.font_size,
            scale: self.scale,
        });

        // Flush GPU commands.
        surface.context.flush_and_submit();
    }

    /// Handle DPI factor updates.
    pub fn set_scale_factor(&mut self, scale: f64) {
        self.scale = scale as f32;
    }

    /// Handle config updates.
    #[cfg_attr(feature = "profiling", profiling::function)]
    pub fn update_config(&mut self, config: &Config) {
        if self.font_family != config.font.family {
            self.font_family = config.font.family.clone();
            self.text_style.set_font_families(&[&*self.font_family]);
        }
        self.font_size = config.font.size;
    }
}

/// Skia state for rendering.
pub struct RenderState<'a> {
    svg_cache: &'a mut HashMap<SvgCacheKey, Image>,
    svg_paint: &'a Paint,

    placeholder_style: &'a mut TextStyle,
    font_collection: &'a FontCollection,
    selection_style: &'a mut TextStyle,
    preedit_style: &'a mut TextStyle,
    text_style: &'a mut TextStyle,
    text_paint: &'a mut Paint,
    font_size: f32,

    canvas: &'a SkiaCanvas,

    scale: f32,
}

impl<'a> RenderState<'a> {
    /// Create a paragraph ready for rendering.
    ///
    /// The `scale` parameter is the text size relative to the default text
    /// size, not the actual font size.
    pub fn paragraph(
        &mut self,
        color: impl Into<Color4f>,
        text_scale: f32,
        options: impl Into<Option<TextOptions>>,
    ) -> ParagraphBuilder {
        // Update the text color if necessary.
        let color = color.into();
        if self.text_style.foreground().color4f() != color {
            self.text_paint.set_color4f(color, None);
            self.text_style.set_foreground_paint(self.text_paint);
        }

        // Update the font size if necessary.
        let font_size = self.font_size * self.scale * text_scale;
        if self.text_style.font_size() != font_size {
            self.text_style.set_font_size(font_size);
        }

        let options = options.into().unwrap_or_default();
        let mut paragraph_style = ParagraphStyle::new();
        paragraph_style.set_text_style(self.text_style);
        paragraph_style.set_text_align(options.align);
        if options.ellipsize {
            paragraph_style.set_ellipsis("…");
        }

        ParagraphBuilder::new(&paragraph_style, self.font_collection)
    }

    /// Get text style for selections.
    pub fn selection_style(
        &mut self,
        foreground: impl Into<Color4f>,
        background: impl Into<Color4f>,
        text_scale: f32,
    ) -> &TextStyle {
        // Update text foreground color if necessary.
        let foreground = foreground.into();
        if self.selection_style.foreground().color4f() != foreground {
            self.text_paint.set_color4f(foreground, None);
            self.selection_style.set_foreground_paint(self.text_paint);
        }

        // Update text background color if necessary.
        let background = background.into();
        if self.selection_style.background().color4f() != background {
            self.text_paint.set_color4f(background, None);
            self.selection_style.set_background_paint(self.text_paint);
        }

        // Update the font size if necessary.
        let font_size = self.font_size * self.scale * text_scale;
        if self.selection_style.font_size() != font_size {
            self.selection_style.set_font_size(font_size);
        }

        self.selection_style
    }

    /// Get text style for IME preedit text.
    pub fn preedit_style(&mut self, color: impl Into<Color4f>, text_scale: f32) -> &TextStyle {
        // Update text foreground color if necessary.
        let color = Color4f { a: HINT_TEXT_ALPHA, ..color.into() };
        if self.preedit_style.foreground().color4f() != color {
            self.text_paint.set_color4f(color, None);
            self.preedit_style.set_foreground_paint(self.text_paint);
        }

        // Update the font size if necessary.
        let font_size = self.font_size * self.scale * text_scale;
        if self.preedit_style.font_size() != font_size {
            self.preedit_style.set_font_size(font_size);
        }

        self.preedit_style
    }

    /// Get text style for placeholder text.
    pub fn placeholder_style(&mut self, color: impl Into<Color4f>, text_scale: f32) -> &TextStyle {
        // Update text foreground color if necessary.
        let color = Color4f { a: HINT_TEXT_ALPHA, ..color.into() };
        if self.placeholder_style.foreground().color4f() != color {
            self.text_paint.set_color4f(color, None);
            self.placeholder_style.set_foreground_paint(self.text_paint);
        }

        // Update the font size if necessary.
        let font_size = self.font_size * self.scale * text_scale;
        if self.placeholder_style.font_size() != font_size {
            self.placeholder_style.set_font_size(font_size);
        }

        self.placeholder_style
    }

    /// Render an SVG with automatic caching.
    #[cfg_attr(feature = "profiling", profiling::function)]
    pub fn draw_svg(&mut self, svg: Svg, point: Point, size: Size) {
        // Get SVG from cache or render it.
        let key = SvgCacheKey { svg, size };
        let image =
            self.svg_cache.entry(key).or_insert_with(|| Self::upload_svg(self.canvas, svg, size));

        // Draw GPU image to the canvas.
        let right = point.x as f32 + size.width as f32;
        let bottom = point.y as f32 + size.height as f32;
        let rect = Rect::new(point.x as f32, point.y as f32, right, bottom);
        self.canvas.draw_image_rect(image, None, rect, self.svg_paint);
    }

    /// Create a GPU-backed Skia image for an SVG.
    #[cfg_attr(feature = "profiling", profiling::function)]
    fn upload_svg(canvas: &SkiaCanvas, svg: Svg, size: Size) -> Image {
        // Parse SVG data.
        let svg_tree = SvgTree::from_data(svg.content(), &SvgOptions::default()).unwrap();

        // Calculate transforms to scale and center SVG within target buffer.
        let tree_size = svg_tree.size();
        let svg_width = tree_size.width();
        let svg_height = tree_size.height();
        let (svg_scale, x_padding, y_padding) = if svg_width > svg_height {
            (size.width as f32 / svg_width, 0., (svg_width - svg_height) / 2.)
        } else {
            (size.height as f32 / svg_height, (svg_height - svg_width) / 2., 0.)
        };
        let transform =
            SvgTransform::from_translate(x_padding, y_padding).post_scale(svg_scale, svg_scale);

        // Render SVG into CPU buffer.
        //
        // SAFETY: Since we upload the buffer to the GPU immediately anyway, we don't
        // have to worry about the lifetime of the pixmap's data.
        let mut pixmap = SvgPixmap::new(size.width, size.height).unwrap();
        resvg::render(&svg_tree, transform, &mut pixmap.as_mut());
        let data = unsafe { Data::new_bytes(pixmap.data()) };

        // Convert resvg pixmap to skia image.
        let info = ImageInfo::new(size, ColorType::RGBA8888, AlphaType::Unpremul, None);
        let cpu_image = cpu_images::raster_from_data(&info, data, size.width as usize * 4).unwrap();

        // Upload CPU image to the GPU.
        let surface = unsafe { canvas.surface().unwrap() };
        let mut context = surface.direct_context().unwrap();
        gpu_images::texture_from_image(&mut context, &cpu_image, Mipmapped::No, Budgeted::Yes)
            .unwrap()
    }
}

impl<'a> Deref for RenderState<'a> {
    type Target = SkiaCanvas;

    fn deref(&self) -> &Self::Target {
        self.canvas
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

/// Available SVG images.
#[derive(Hash, PartialEq, Eq, Copy, Clone, Debug)]
pub enum Svg {
    ArrowLeft,
    Download,
    Config,
    Search,
    Bin,
}

impl Svg {
    /// Get SVG's text content.
    const fn content(&self) -> &'static [u8] {
        match self {
            Self::ArrowLeft => include_bytes!("../../svgs/arrow_left.svg"),
            Self::Download => include_bytes!("../../svgs/download.svg"),
            Self::Config => include_bytes!("../../svgs/config.svg"),
            Self::Search => include_bytes!("../../svgs/search.svg"),
            Self::Bin => include_bytes!("../../svgs/bin.svg"),
        }
    }
}

/// HashMap key for the SVG image cache.
#[derive(Hash, PartialEq, Eq, Copy, Clone)]
struct SvgCacheKey {
    svg: Svg,
    size: Size,
}

/// Text rendering style options.
#[derive(Copy, Clone)]
pub struct TextOptions {
    pub align: TextAlign,
    pub ellipsize: bool,
}

impl Default for TextOptions {
    fn default() -> Self {
        Self { align: TextAlign::Left, ellipsize: true }
    }
}

impl TextOptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set whether text should be multiline or ellipsized.
    pub fn ellipsize(&mut self, ellipsize: bool) -> Self {
        self.ellipsize = ellipsize;
        *self
    }

    /// Set the horizontal text alignment.
    pub fn align(&mut self, align: TextAlign) -> Self {
        self.align = align;
        *self
    }
}
