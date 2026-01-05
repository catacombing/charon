//! Download UI view.

use std::any::Any;
use std::collections::HashMap;
use std::fmt::Write;
use std::sync::Arc;
use std::{fs, mem};

use calloop::LoopHandle;
use skia_safe::{Color4f, Paint, Rect};
use tracing::error;

use crate::config::{Config, Input};
use crate::db::Db;
use crate::geometry::{Point, Size, rect_contains};
use crate::region::{DownloadState, Region, Regions};
use crate::ui::skia::RenderState;
use crate::ui::view::{UiView, View};
use crate::ui::{Button, Svg, Velocity};
use crate::{Error, State};

/// Back button width and height at scale 1.
const BACK_BUTTON_SIZE: u32 = 48;

/// Padding around the screen edge at scale 1.
const OUTSIDE_PADDING: u32 = 16;

/// Padding around the content of the region entries at scale 1.
const REGION_INSIDE_PADDING: f64 = 16.;

/// Vertical space between region entries at scale 1.
const REGION_Y_PADDING: f64 = 2.;

/// Region entry height at scale 1.
const REGION_HEIGHT: u32 = 50;

/// Progress/Download/Delete button width and height at scale 1.
const REGION_BUTTON_SIZE: u32 = 32;

/// Progress bar height at scale 1.
const PROGRESS_HEIGHT: f32 = 8.;

/// Secondary font size for region size/count relative to primary font.
const ALT_FONT_SIZE: f32 = 0.5;

/// Download UI view.
pub struct DownloadView {
    regions: Arc<Regions>,
    current_region: [usize; 5],
    tiles_size: u64,

    back_button: Button,
    alt_bg_paint: Paint,
    bg_paint: Paint,
    hl_paint: Paint,

    touch_state: TouchState,
    input_config: Input,
    scroll_offset: f64,

    event_loop: LoopHandle<'static, State>,

    size: Size,
    scale: f64,

    dirty: bool,
}

impl DownloadView {
    #[cfg_attr(feature = "profiling", profiling::function)]
    pub fn new(
        event_loop: LoopHandle<'static, State>,
        config: &Config,
        regions: Arc<Regions>,
        size: Size,
    ) -> Result<Self, Error> {
        // Initialize UI elements.
        let point = Self::back_button_point(size, 1.);
        let size = Self::back_button_size(1.);
        let back_button = Button::new(point, size, Svg::ArrowLeft);

        let mut alt_bg_paint = Paint::default();
        alt_bg_paint.set_color4f(Color4f::from(config.colors.alt_background), None);
        let mut bg_paint = Paint::default();
        bg_paint.set_color4f(Color4f::from(config.colors.background), None);
        let mut hl_paint = Paint::default();
        hl_paint.set_color4f(Color4f::from(config.colors.highlight), None);

        Ok(Self {
            alt_bg_paint,
            back_button,
            event_loop,
            bg_paint,
            hl_paint,
            regions,
            size,
            current_region: [usize::MAX; 5],
            input_config: config.input,
            dirty: true,
            scale: 1.,
            scroll_offset: Default::default(),
            touch_state: Default::default(),
            tiles_size: Default::default(),
        })
    }

    /// Mark the view as dirty.
    pub fn set_dirty(&mut self) {
        self.dirty = true;
    }

    /// Draw a region entry.
    #[cfg_attr(feature = "profiling", profiling::function)]
    fn draw_region<'a>(
        &self,
        config: &Config,
        render_state: &mut RenderState<'a>,
        point: Point,
        size: Size,
        region: &Region,
    ) {
        let padding = (REGION_INSIDE_PADDING * self.scale).round() as f32;

        // Draw background.
        let bg_width = point.x as f32 + size.width as f32;
        let bg_height = point.y as f32 + size.height as f32;
        let bg_rect = Rect::new(point.x as f32, point.y as f32, bg_width, bg_height);
        render_state.draw_rect(bg_rect, &self.alt_bg_paint);

        // Draw region's button.
        let (button_svg, downloading) = match region.download_state() {
            DownloadState::NoData => (None, false),
            DownloadState::Downloading => (None, true),
            DownloadState::Available => (Some(Svg::Download), false),
            DownloadState::Downloaded => (Some(Svg::Bin), false),
        };
        let text_width = match (button_svg, downloading) {
            (Some(button_svg), _) => {
                let region_button_point = self.region_button_point();
                let button_point = point + region_button_point;
                let button_size = self.region_button_size();
                render_state.draw_svg(button_svg, button_point, button_size);

                region_button_point.x as f32 - padding * 2.
            },
            // Draw download progress bar.
            (None, true) => {
                let region_button_point: Point<f32> = self.region_button_point().into();
                let button_point = region_button_point + point.into();
                let button_size: Size<f32> = self.region_button_size().into();
                let progress_height = PROGRESS_HEIGHT * self.scale as f32;
                let progress = region.download_progress() as f32;

                // Draw progress bar background.
                let right = button_point.x + button_size.width;
                let top = button_point.y + (button_size.height - progress_height) / 2.;
                let bottom = top + progress_height;
                let mut rect = Rect::new(button_point.x, top, right, bottom);
                render_state.draw_rect(rect, &self.bg_paint);

                // Draw progress bar foreground.
                rect.right -= button_size.width * (1. - progress);
                render_state.draw_rect(rect, &self.hl_paint);

                region_button_point.x - padding * 2.
            },
            (None, false) => size.width as f32 - padding * 2.,
        };

        let mut text_point = point;
        text_point.x += padding as i32;

        // Layout region name.

        let mut builder = render_state.paragraph(config.colors.foreground, 1., None);
        builder.add_text(&region.name);

        let mut region_paragraph = builder.build();
        region_paragraph.layout(text_width);

        // Layout required storage size/region count text.

        let mut size_text = String::with_capacity("X.XX GB · 99 Regions".len());
        format_size(&mut size_text, region.storage_size);
        match region.regions.len() {
            0 => (),
            1 => size_text.push_str(" · 1 Region"),
            count => _ = write!(&mut size_text, " · {count} Regions"),
        }

        let mut builder = render_state.paragraph(config.colors.alt_foreground, ALT_FONT_SIZE, None);
        builder.add_text(&size_text);

        let mut size_paragraph = builder.build();
        size_paragraph.layout(text_width);

        // Draw both labels.

        let region_text_height = region_paragraph.height().round() as i32;
        let size_text_height = size_paragraph.height().round() as i32;

        text_point.y += (size.height as i32 - region_text_height - size_text_height) / 2;
        region_paragraph.paint(render_state, text_point);

        text_point.y += region_text_height;
        size_paragraph.paint(render_state, text_point);
    }

    /// Physical location of the back button.
    fn back_button_point(size: Size, scale: f64) -> Point {
        let padding = (OUTSIDE_PADDING as f64 * scale).round() as i32;
        let button_size = Self::back_button_size(scale);
        let physical_size = size * scale;

        let x = (physical_size.width - button_size.width) as i32 - padding;
        let y = (physical_size.height - button_size.height) as i32 - padding;

        Point::new(x, y)
    }

    /// Physical size of the back button.
    fn back_button_size(scale: f64) -> Size {
        Size::new(BACK_BUTTON_SIZE, BACK_BUTTON_SIZE) * scale
    }

    /// Physical location of the current install size label.
    fn installed_label_point(&self) -> Point {
        let outside_padding = (OUTSIDE_PADDING as f64 * self.scale).round() as i32;
        let inside_padding = (REGION_INSIDE_PADDING * self.scale).round() as i32;
        let button_point = Self::back_button_point(self.size, self.scale);

        Point::new(outside_padding + inside_padding, button_point.y)
    }

    /// Physical size of the current install size label.
    fn installed_label_size(&self) -> Size {
        let padding = (OUTSIDE_PADDING as f64 * self.scale).round() as u32;
        let button_size = Self::back_button_size(self.scale);
        let size = self.size * self.scale;

        let width = size.width - 2 * padding - button_size.width;

        Size::new(width, button_size.height)
    }

    /// Physical point of the bottommost region entry.
    fn region_point(&self) -> Point {
        let back_button_point = Self::back_button_point(self.size, self.scale);
        let outside_padding = (OUTSIDE_PADDING as f64 * self.scale).round() as i32;
        let region_size = self.region_size();

        let y = back_button_point.y - outside_padding - region_size.height as i32;
        Point::new(outside_padding, y)
    }

    /// Physical size of a region entry.
    fn region_size(&self) -> Size {
        let outside_padding = (OUTSIDE_PADDING as f64 * self.scale).round() as u32;
        let size = self.size * self.scale;

        let width = size.width - outside_padding * 2;
        let height = (REGION_HEIGHT as f64 * self.scale).round() as u32;

        Size::new(width, height)
    }

    /// Physical point of the region button relative to the region origin.
    fn region_button_point(&self) -> Point {
        let region_button_size = self.region_button_size();
        let region_size = self.region_size();
        let padding = (region_size.height - region_button_size.height) as i32 / 2;

        let x = region_size.width as i32 - region_button_size.width as i32 - padding;
        Point::new(x, padding)
    }

    /// Physical size of the region's delete/download button.
    fn region_button_size(&self) -> Size {
        Size::new(REGION_BUTTON_SIZE, REGION_BUTTON_SIZE) * self.scale
    }

    /// Get region at the specified location.
    fn region_at(&self, mut point: Point<f64>) -> Option<(usize, &Region, bool)> {
        let region_point = self.region_point();
        let region_size = self.region_size();
        let region_end = region_point.y as f64 + region_size.height as f64;

        // Short-circuit if point is outside the region list.
        if point.x < region_point.x as f64
            || point.x >= region_point.x as f64 + region_size.width as f64
            || point.y >= region_end
        {
            return None;
        }

        // Apply current scroll offset.
        point.y -= self.scroll_offset;

        // Ignore taps within vertical padding.
        let region_height = region_size.height as f64 + REGION_Y_PADDING * self.scale;
        let bottom_relative = region_end - point.y - 1.;
        if bottom_relative % region_height >= region_size.height as f64 {
            return None;
        }

        // Find index at the specified offset.
        let rindex = (bottom_relative / region_height).floor() as usize;
        let index = self.region().regions.len().checked_sub(rindex + 1)?;

        // Check whether the tap is within the region's icon.
        let relative_x = point.x - region_point.x as f64;
        let relative_y = region_height - 1. - (bottom_relative % region_height);
        let relative_point = Point::new(relative_x, relative_y);
        let region_button_point: Point<f64> = self.region_button_point().into();
        let region_button_size: Size<f64> = self.region_button_size().into();
        let button_pressed = rect_contains(region_button_point, region_button_size, relative_point);

        Some((index, &self.region().regions[index], button_pressed))
    }

    /// Clamp viewport offset.
    fn clamp_scroll_offset(&mut self) {
        let old_offset = self.scroll_offset;
        let max_offset = self.max_scroll_offset() as f64;
        self.scroll_offset = self.scroll_offset.clamp(0., max_offset);

        // Cancel velocity after reaching the scroll limit.
        if old_offset != self.scroll_offset {
            self.touch_state.velocity.stop();
            self.dirty = true;
        }
    }

    /// Get maximum viewport offset.
    fn max_scroll_offset(&self) -> usize {
        let outside_padding = (OUTSIDE_PADDING as f64 * self.scale).round() as usize;
        let region_padding = (REGION_Y_PADDING * self.scale).round() as usize;
        let region_height = self.region_size().height as usize;

        // Calculate height of all regions plus top padding.
        let region_count = self.region().regions.len();
        let regions_height = (region_count * (region_height + region_padding))
            .saturating_sub(region_padding)
            + outside_padding;

        // Calculate tab content outside the viewport.
        regions_height.saturating_sub(self.region_point().y as usize + region_height)
    }

    /// Get the currently selected region.
    fn region(&self) -> &Region {
        Self::index_region(self.regions.world(), &self.current_region)
    }

    /// Get a sub-region using a list of region indices.
    ///
    /// # Panics
    ///
    /// Panics if the index does not exist.
    fn index_region<'a>(mut region: &'a Region, index: &'a [usize]) -> &'a Region {
        for i in index.iter().take_while(|i| **i != usize::MAX) {
            region = &region.regions[*i];
        }
        region
    }
}

impl UiView for DownloadView {
    #[cfg_attr(feature = "profiling", profiling::function)]
    fn draw<'a>(&mut self, config: &Config, mut render_state: RenderState<'a>) {
        let size = self.size * self.scale;

        // Apply scroll velocity.
        if let Some(delta) = self.touch_state.velocity.apply(&self.input_config) {
            self.scroll_offset += delta.y;
        }

        // Ensure offset is correct in case size changed.
        self.clamp_scroll_offset();

        // Clear dirtiness flag.
        //
        // This is inentionally placed after functions like `clamp_scroll_offset`, since
        // these modify dirtiness but do not require another redraw.
        self.dirty = false;

        // Ensure paints are up to date.
        self.alt_bg_paint.set_color4f(Color4f::from(config.colors.alt_background), None);
        self.bg_paint.set_color4f(Color4f::from(config.colors.background), None);
        self.hl_paint.set_color4f(Color4f::from(config.colors.highlight), None);

        render_state.clear(config.colors.background);

        // Calculate region list geometry.

        let padding = (REGION_Y_PADDING * self.scale).round() as i32;
        let region_size = self.region_size();

        let region_start = self.region_point();
        let mut region_point = region_start;
        region_point.y += self.scroll_offset.round() as i32;

        // Set clipping mask to cut off regions overlapping the bottom button.
        let bottom = region_start.y as f32 + region_size.height as f32;
        let clip_rect = Rect::new(0., 0., size.width as f32, bottom);
        render_state.save();
        render_state.clip_rect(clip_rect, None, Some(false));

        // Render region entries.
        let region = self.region();
        for (_, region) in region.regions.iter().rev() {
            if region_point.y > region_start.y + (region_size.height as i32) {
                region_point.y -= region_size.height as i32 + padding;
                continue;
            } else if region_point.y + (region_size.height as i32) < 0 {
                break;
            }

            self.draw_region(config, &mut render_state, region_point, region_size, region);
            region_point.y -= region_size.height as i32 + padding;
        }

        // Reset region clipping mask.
        render_state.restore();

        let mut label_point: Point<f32> = self.installed_label_point().into();
        let label_size = self.installed_label_size();

        // Layout tile storage size text if the toplevel region is displayed.
        let tiles_size_paragraph = (self.current_region[0] == usize::MAX).then(|| {
            let mut builder = render_state.paragraph(config.colors.foreground, 1., None);
            let mut tiles_size_text = String::with_capacity("Tiles: X.XXGB".len());
            tiles_size_text.push_str("Tiles: ");
            format_size(&mut tiles_size_text, self.tiles_size);
            builder.add_text(&tiles_size_text);

            let mut paragraph = builder.build();
            paragraph.layout(label_size.width as f32);

            paragraph
        });

        // Layout region's installation size text.

        let mut builder = render_state.paragraph(config.colors.foreground, 1., None);
        let mut downloaded_text = String::with_capacity("Downloaded: X.XX GB".len());
        downloaded_text.push_str("Downloaded: ");
        format_size(&mut downloaded_text, region.current_install_size());
        builder.add_text(&downloaded_text);

        let mut region_size_paragraph = builder.build();
        region_size_paragraph.layout(label_size.width as f32);

        // Draw text vertically centered in its space.

        let tiles_size_height = tiles_size_paragraph.as_ref().map_or(0., |p| p.height());
        let region_size_height = region_size_paragraph.height();
        let y_offset = (label_size.height as f32 - region_size_height - tiles_size_height) / 2.;
        label_point.y += y_offset;

        region_size_paragraph.paint(&render_state, label_point);

        if let Some(paragraph) = tiles_size_paragraph {
            label_point.y += region_size_height;
            paragraph.paint(&render_state, label_point);
        }

        // Render navigation button.
        self.back_button.draw(&mut render_state, config.colors.alt_background);
    }

    fn dirty(&self) -> bool {
        self.dirty || self.touch_state.velocity.is_moving()
    }

    #[cfg_attr(feature = "profiling", profiling::function)]
    fn set_size(&mut self, size: Size) {
        self.size = size;
        self.dirty = true;

        // Update UI elements.
        self.back_button.set_point(Self::back_button_point(size, self.scale));
    }

    #[cfg_attr(feature = "profiling", profiling::function)]
    fn set_scale_factor(&mut self, scale: f64) {
        self.scale = scale;
        self.dirty = true;

        // Update UI elements.
        self.back_button.set_point(Self::back_button_point(self.size, scale));
        self.back_button.set_size(Self::back_button_size(scale));
    }

    #[cfg_attr(feature = "profiling", profiling::function)]
    fn touch_down(&mut self, slot: i32, _time: u32, point: Point<f64>) {
        // Cancel velocity if a new touch sequence starts.
        self.touch_state.velocity.stop();

        // Only allow a single active touch slot.
        if !self.touch_state.slots.is_empty() {
            return;
        }

        // Determine goal of this touch sequence.
        let point = point * self.scale;
        self.touch_state.action =
            if self.back_button.contains(point) { TouchAction::Back } else { TouchAction::Tap };

        // Convert position to physical space.
        let slot = self.touch_state.slots.entry(slot).or_default();
        slot.point = point;
        slot.start = point;
    }

    #[cfg_attr(feature = "profiling", profiling::function)]
    fn touch_motion(&mut self, slot: i32, point: Point<f64>) {
        // Ignore unknown touch slots.
        let slot = match self.touch_state.slots.get_mut(&slot) {
            Some(slot) => slot,
            None => return,
        };

        // Update touch point.
        let point = point * self.scale;
        let old_point = mem::replace(&mut slot.point, point);

        // Handle action transitions.
        if let TouchAction::Tap | TouchAction::Drag = self.touch_state.action {
            // Ignore dragging until tap distance limit is exceeded.
            let max_tap_distance = self.input_config.max_tap_distance;
            let delta = slot.point - slot.start;
            if delta.x.powi(2) + delta.y.powi(2) <= max_tap_distance {
                return;
            }
            self.touch_state.action = TouchAction::Drag;

            // Update pending scroll velocity.
            let delta = slot.point.y - old_point.y;
            self.touch_state.velocity.set(Point::new(0., delta));

            // Apply scroll motion.
            let old_offset = self.scroll_offset;
            self.scroll_offset += delta;
            self.clamp_scroll_offset();
            self.dirty |= self.scroll_offset != old_offset;
        }
    }

    #[cfg_attr(feature = "profiling", profiling::function)]
    fn touch_up(&mut self, slot: i32) {
        // Reset touch slot, ignoring unknown slots.
        let removed = match self.touch_state.slots.remove(&slot) {
            Some(removed) => removed,
            None => return,
        };

        // Dispatch tap actions on release.
        match self.touch_state.action {
            // Handle touch tap on region entries.
            TouchAction::Tap => {
                let (index, region, button_pressed) = match self.region_at(removed.point) {
                    Some(region) => region,
                    None => return,
                };
                let download_state =
                    if button_pressed { region.download_state() } else { DownloadState::NoData };

                match (index, region, download_state) {
                    // Ignore button interactions during download
                    (.., DownloadState::Downloading) => (),
                    // Download region's data.
                    (_, region, DownloadState::Available) => {
                        // Immediately mark region as downloading.
                        region.set_download_state(DownloadState::Downloading);
                        self.dirty = true;

                        let current_region = self.current_region;
                        let regions = self.regions.clone();
                        tokio::spawn(async move {
                            // Re-index the region, since we can't move the reference.
                            let mut region = Self::index_region(regions.world(), &current_region);
                            region = &region.regions[index];

                            match regions.download(region).await {
                                Ok(_) => region.set_download_state(DownloadState::Downloaded),
                                Err(err) => {
                                    error!("Region data download failed: {err}");

                                    // Delete all data to avoid tempfiles stealing storage space.
                                    regions.delete(region).await;

                                    region.set_download_state(DownloadState::Available);
                                },
                            }

                            // Wake UI to display the download state update.
                            regions.redraw_download_view();
                        });
                    },
                    // Delete region's local data.
                    (_, region, DownloadState::Downloaded) => {
                        // Immediately mark region as available for download.
                        region.set_download_state(DownloadState::Available);
                        self.dirty = true;

                        // Delete region data in the background.
                        let current_region = self.current_region;
                        let regions = self.regions.clone();
                        tokio::spawn(async move {
                            // Re-index the region, since we can't move the reference.
                            let mut region = Self::index_region(regions.world(), &current_region);
                            region = &region.regions[index];

                            regions.delete(region).await
                        });
                    },
                    // Ignore touch on region when region doesn't have child regions.
                    (_, region, _) if region.regions.is_empty() => (),
                    // Handle navigation into the next region.
                    (index, ..) => {
                        match self.current_region.iter_mut().find(|i| **i == usize::MAX) {
                            Some(region_index) => {
                                *region_index = index;
                                self.scroll_offset = 0.;
                                self.dirty = true;
                            },
                            None => error!("Insufficient region depth; please file a bug report"),
                        }
                    },
                }
            },
            // Handle "back" button navigation.
            TouchAction::Back if self.back_button.contains(removed.point) => {
                match self.current_region.iter_mut().rfind(|i| **i != usize::MAX) {
                    Some(index) => {
                        *index = usize::MAX;
                        self.dirty = true;
                    },
                    None => {
                        self.event_loop.insert_idle(|state| state.window.set_view(View::Search));
                    },
                }
            },
            _ => (),
        }
    }

    #[cfg_attr(feature = "profiling", profiling::function)]
    fn update_config(&mut self, config: &Config) {
        if self.input_config != config.input {
            self.input_config = config.input;
            self.dirty = true;
        }
    }

    fn enter(&mut self) {
        // Update current tiles storage size.
        //
        // While the database includes data beyond just the tile storage itself, that
        // should be negligible in comparison to the size used for the tiles.
        self.tiles_size = Db::path()
            .ok()
            .and_then(|path| fs::metadata(path).ok())
            .map_or(0, |metadata| metadata.len());
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

    velocity: Velocity,
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
    Tap,
    Drag,
    Back,
}

/// Format a byte size into a 3 digit human-readable size.
fn format_size(w: &mut impl Write, size: u64) {
    // Define bounds to ensure maximum value is 999 after rounding.
    let (unit, divisor) = match size {
        ..1_000 => ("B", 1.),
        1_000..1_024_000 => ("KB", 1024.),
        1_024_000..1_048_052_000 => ("MB", 1024. * 1024.),
        _ => ("GB", 1024. * 1024. * 1024.),
    };

    let size = size as f64 / divisor;
    let precision = 2usize.saturating_sub(size.log10() as usize);

    let _ = write!(w, "{size:.precision$} {unit}");
}
