//! Route overview UI view.

use std::collections::HashMap;
use std::mem;
use std::sync::Arc;

use calloop::LoopHandle;
use skia_safe::textlayout::TextAlign;
use skia_safe::{Color4f, Paint, Rect};

use crate::config::{Config, Input};
use crate::geometry::{Point, Size};
use crate::router::{Mode as RouteMode, Route, Segment};
use crate::ui::skia::{RenderState, TextOptions};
use crate::ui::view::search::RouteOrigin;
use crate::ui::view::{self, UiView, View};
use crate::ui::{Button, Svg, Velocity};
use crate::{Error, State};

/// Button width and height at scale 1.
const BUTTON_SIZE: u32 = 48;

/// Padding around the screen edge at scale 1.
const OUTSIDE_PADDING: u32 = 16;

/// Padding around the content of the route segments at scale 1.
const SEGMENT_INSIDE_PADDING: f64 = 16.;

/// Vertical padding between paragraphs inside a segment at scale 1.
const SEGMENT_TEXT_PADDING: f64 = 8.;

/// Vertical space between route segments at scale 1.
const SEGMENT_Y_PADDING: f64 = 2.;

/// Segment distance/time font size relative to the default.
const ALT_FONT_SIZE: f32 = 0.75;

/// Route UI view.
pub struct RouteView {
    route: Arc<Route>,
    segment_heights: Vec<i32>,
    is_gps_route: bool,

    cancel_button: Button,
    back_button: Button,
    mode_button: Button,
    alt_bg_paint: Paint,

    touch_state: TouchState,
    input_config: Input,
    max_scroll_offset: Option<f64>,
    scroll_offset: f64,

    event_loop: LoopHandle<'static, State>,

    size: Size,
    scale: f64,

    dirty: bool,
}

impl RouteView {
    #[cfg_attr(feature = "profiling", profiling::function)]
    pub fn new(
        event_loop: LoopHandle<'static, State>,
        config: &Config,
        size: Size,
    ) -> Result<Self, Error> {
        // Initialize UI elements.
        let point = Self::mode_button_point(size, 1.);
        let size = Self::button_size(1.);
        let mode_button = Button::new(point, size, Svg::Car);

        let point = Self::back_button_point(size, 1.);
        let size = Self::button_size(1.);
        let back_button = Button::new(point, size, Svg::ArrowLeft);

        let point = Self::cancel_button_point(size, 1.);
        let size = Self::button_size(1.);
        let cancel_button = Button::new(point, size, Svg::CancelRoute);

        let mut alt_bg_paint = Paint::default();
        alt_bg_paint.set_color4f(Color4f::from(config.colors.alt_background), None);

        Ok(Self {
            alt_bg_paint,
            cancel_button,
            back_button,
            mode_button,
            event_loop,
            size,
            input_config: config.input,
            dirty: true,
            scale: 1.,
            max_scroll_offset: Default::default(),
            segment_heights: Default::default(),
            scroll_offset: Default::default(),
            is_gps_route: Default::default(),
            touch_state: Default::default(),
            route: Default::default(),
        })
    }

    /// Update the active route.
    pub fn set_route(&mut self, route: Arc<Route>, is_gps_route: bool) {
        self.mode_button.set_svg(route.mode.svg());

        self.is_gps_route = is_gps_route;
        self.route = route;

        self.max_scroll_offset = None;
        self.segment_heights.clear();
        self.scroll_offset = 0.;
        self.dirty = true;
    }

    /// Draw a route segment.
    ///
    /// This returns the height of the rendered segment.
    #[cfg_attr(feature = "profiling", profiling::function)]
    fn draw_segment<'a>(
        &self,
        config: &Config,
        render_state: &mut RenderState<'a>,
        point: Point,
        width: f32,
        segment: &Segment,
    ) -> f32 {
        let inside_padding = (SEGMENT_INSIDE_PADDING * self.scale).round() as f32;
        let text_padding = (SEGMENT_TEXT_PADDING * self.scale).round() as f32;
        let mut text_point =
            Point::new(point.x as f32 + inside_padding, point.y as f32 - inside_padding);
        let text_width = width - 2. * inside_padding;

        // Layout instruction text.

        let text_options = Some(TextOptions::new().ellipsize(false));
        let mut builder = render_state.paragraph(config.colors.foreground, 1., text_options);
        builder.add_text(&*segment.instruction);

        let mut instruction_paragraph = builder.build();
        instruction_paragraph.layout(text_width);
        let instruction_height = instruction_paragraph.height();

        // Layout segment duration.

        let hours = segment.time / 3600;
        let minutes = (segment.time % 3600 + 30) / 60;

        let mut builder = render_state.paragraph(config.colors.foreground, ALT_FONT_SIZE, None);
        builder.add_text(format!("{hours:0>2}:{minutes:0>2}"));

        let mut time_paragraph = builder.build();
        time_paragraph.layout(text_width);
        let time_height = time_paragraph.height();

        // Layout segment distance.

        let mut distance = String::with_capacity("X.XX km".len());
        view::format_distance(&mut distance, segment.length);

        let text_options = Some(TextOptions::new().align(TextAlign::Right));
        let mut builder =
            render_state.paragraph(config.colors.foreground, ALT_FONT_SIZE, text_options);
        builder.add_text(&distance);

        let mut distance_paragraph = builder.build();
        distance_paragraph.layout(text_width);

        // Skip rendering if segment is fully offscreen.
        let height = instruction_height + time_height + 2. * inside_padding + text_padding;
        if point.y as f32 - height >= self.size.height as f32 * self.scale as f32 || point.y < 0 {
            return height;
        }

        // Draw background.
        let bg_right = point.x as f32 + width;
        let bg_bottom = point.y as f32 - height;
        let bg_rect = Rect::new(point.x as f32, point.y as f32, bg_right, bg_bottom);
        render_state.draw_rect(bg_rect, &self.alt_bg_paint);

        // Draw all paragraphs.
        text_point.y -= time_height;
        time_paragraph.paint(render_state, text_point);
        distance_paragraph.paint(render_state, text_point);
        text_point.y -= instruction_height + text_padding;
        instruction_paragraph.paint(render_state, text_point);

        height
    }

    /// Physical size of the UI SVG buttons.
    fn button_size(scale: f64) -> Size {
        Size::new(BUTTON_SIZE, BUTTON_SIZE) * scale
    }

    /// Physical location of the back button.
    fn back_button_point(size: Size, scale: f64) -> Point {
        let padding = (OUTSIDE_PADDING as f64 * scale).round() as i32;
        let button_size = Self::button_size(scale);
        let physical_size = size * scale;

        let x = (physical_size.width - button_size.width) as i32 - padding;
        let y = (physical_size.height - button_size.height) as i32 - padding;

        Point::new(x, y)
    }

    /// Physical location of the mode button.
    fn mode_button_point(size: Size, scale: f64) -> Point {
        let padding = (OUTSIDE_PADDING as f64 * scale).round() as i32;
        let button_width = Self::button_size(scale).width as i32;
        let mut point = Self::back_button_point(size, scale);

        point.x -= button_width + padding;

        point
    }

    /// Physical location of the cancel button.
    fn cancel_button_point(size: Size, scale: f64) -> Point {
        let padding = (OUTSIDE_PADDING as f64 * scale).round() as i32;
        let button_width = Self::button_size(scale).width as i32;
        let mut point = Self::mode_button_point(size, scale);

        point.x -= button_width + padding;

        point
    }

    /// Physical location of the route summary text.
    fn summary_label_point(&self) -> Point {
        let outside_padding = (OUTSIDE_PADDING as f64 * self.scale).round() as i32;
        let button_point = Self::cancel_button_point(self.size, self.scale);

        Point::new(outside_padding, button_point.y)
    }

    /// Physical size of the route summary text.
    fn summary_label_size(&self) -> Size {
        let padding = (OUTSIDE_PADDING as f64 * self.scale).round() as u32;
        let button_point = Self::cancel_button_point(self.size, self.scale);
        let button_size = Self::button_size(self.scale);

        Size::new(button_point.x as u32 - 2 * padding, button_size.height)
    }

    /// Clamp viewport offset.
    fn clamp_scroll_offset(&mut self) {
        let old_offset = self.scroll_offset;
        let max_offset = self.max_scroll_offset.unwrap_or(0.);
        self.scroll_offset = self.scroll_offset.clamp(0., max_offset);

        // Cancel velocity after reaching the scroll limit.
        if old_offset != self.scroll_offset {
            self.touch_state.velocity.stop();
            self.dirty = true;
        }
    }
}

impl UiView for RouteView {
    #[cfg_attr(feature = "profiling", profiling::function)]
    fn draw<'a>(&mut self, config: &Config, mut render_state: RenderState<'a>) {
        let size = self.size * self.scale;

        // Apply scroll velocity.
        if let Some(delta) = self.touch_state.velocity.apply(&self.input_config) {
            self.scroll_offset += delta.y;
        }

        // Ensure offset is correct in case size changed.
        //
        // This uses the last draw's scroll offset, but since the segments never change
        // and the initial draw has no offset, this is not an issue.
        self.clamp_scroll_offset();

        // Clear dirtiness flag.
        //
        // This is inentionally placed after functions like `clamp_scroll_offset`, since
        // these modify dirtiness but do not require another redraw.
        self.dirty = false;

        // Ensure paints are up to date.
        self.alt_bg_paint.set_color4f(Color4f::from(config.colors.alt_background), None);

        render_state.clear(config.colors.background);

        // Calculate route segment list geometry.

        let back_button_point = Self::back_button_point(self.size, self.scale);
        let outside_padding = (OUTSIDE_PADDING as f64 * self.scale).round() as i32;
        let padding = (SEGMENT_Y_PADDING * self.scale).round() as i32;
        let segment_start = Point::new(outside_padding, back_button_point.y - outside_padding);
        let segment_width = size.width as f32 - 2. * outside_padding as f32;

        let mut segment_point = segment_start;
        segment_point.y += self.scroll_offset.round() as i32;

        // Set clipping mask to cut off segments overlapping the bottom button.
        let clip_rect = Rect::new(0., 0., size.width as f32, segment_start.y as f32);
        render_state.save();
        render_state.clip_rect(clip_rect, None, Some(false));

        // Render route segments.
        //
        // Since segments are variable in height based on content, we layout every
        // segment the first time a route is rendered. That information allows us to
        // calculate the maximum scroll offset and skip offscreen segments.
        for (i, segment) in self.route.segments.iter().enumerate() {
            // Skip offscreen segments, if we've already calculated the max scroll offset.
            if self.max_scroll_offset.is_some() {
                if segment_point.y < 0 {
                    break;
                } else if segment_point.y - self.segment_heights[i] >= segment_start.y {
                    segment_point.y -= self.segment_heights[i] + padding;
                    continue;
                }
            }

            let segment_height = self
                .draw_segment(config, &mut render_state, segment_point, segment_width, segment)
                .round() as i32;
            segment_point.y -= segment_height + padding;

            if self.max_scroll_offset.is_none() {
                self.segment_heights.push(segment_height);
            }
        }

        // Update maximum scroll offset based on rendered segments.
        if self.max_scroll_offset.is_none() {
            let max_offset = (-segment_point.y + outside_padding) as f64;
            self.max_scroll_offset = Some(max_offset.max(0.));
        }

        // Reset route segment clipping mask.
        render_state.restore();

        let inside_padding = (SEGMENT_INSIDE_PADDING * self.scale).round() as f32;
        let mut label_point: Point<f32> = self.summary_label_point().into();
        let label_size: Size<f32> = self.summary_label_size().into();

        // Layout overall route duration paragraph.

        let hours = self.route.time / 3600;
        let minutes = (self.route.time % 3600 + 30) / 60;

        let mut builder = render_state.paragraph(config.colors.foreground, 1., None);
        builder.add_text(format!("{hours:0>2}:{minutes:0>2}"));

        let mut time_paragraph = builder.build();
        time_paragraph.layout(label_size.width);

        // Layout overall route distance paragraph.

        let mut distance = String::with_capacity("X.XX km".len());
        view::format_distance(&mut distance, self.route.length);

        let mut builder = render_state.paragraph(config.colors.foreground, 1., None);
        builder.add_text(&distance);

        let mut distance_paragraph = builder.build();
        distance_paragraph.layout(label_size.width);

        // Draw summary vertically centered in its space.

        let distance_height = distance_paragraph.height();
        let time_height = time_paragraph.height();
        let y_offset = (label_size.height - time_height - distance_height) / 2.;
        label_point += Point::new(inside_padding, y_offset);

        distance_paragraph.paint(&render_state, label_point);
        label_point.y += distance_height;
        time_paragraph.paint(&render_state, label_point);

        // Render navigation button.
        self.cancel_button.draw(&mut render_state, config.colors.alt_background);
        self.mode_button.draw(&mut render_state, config.colors.alt_background);
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
        self.cancel_button.set_point(Self::cancel_button_point(size, self.scale));
        self.mode_button.set_point(Self::mode_button_point(size, self.scale));
        self.back_button.set_point(Self::back_button_point(size, self.scale));
    }

    #[cfg_attr(feature = "profiling", profiling::function)]
    fn set_scale_factor(&mut self, scale: f64) {
        self.scale = scale;
        self.dirty = true;

        // Update UI elements.
        self.cancel_button.set_point(Self::cancel_button_point(self.size, scale));
        self.cancel_button.set_size(Self::button_size(scale));
        self.back_button.set_point(Self::back_button_point(self.size, scale));
        self.back_button.set_size(Self::button_size(scale));
        self.mode_button.set_point(Self::mode_button_point(self.size, scale));
        self.mode_button.set_size(Self::button_size(scale));
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
        self.touch_state.action = if self.cancel_button.contains(point) {
            TouchAction::Cancel
        } else if self.back_button.contains(point) {
            TouchAction::Back
        } else if self.mode_button.contains(point) {
            TouchAction::Mode
        } else {
            TouchAction::Tap
        };

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
            // Handle route cancel button.
            TouchAction::Cancel if self.cancel_button.contains(removed.point) => {
                self.event_loop.insert_idle(|state| {
                    state.window.views.map().cancel_route();
                    state.window.set_view(View::Map);
                });
            },
            // Handle "back" button navigation.
            TouchAction::Back if self.back_button.contains(removed.point) => {
                self.event_loop.insert_idle(|state| state.window.set_view(View::Map));
            },
            // Handle route transportation mode toggle.
            TouchAction::Mode if self.mode_button.contains(removed.point) => {
                // Determine the current route's origin and target.
                let origin = self.route.segments.first().and_then(|s| s.points.first());
                let target = self.route.segments.last().and_then(|s| s.points.last());
                let (origin, target) = match origin.zip(target) {
                    Some((_, &target)) if self.is_gps_route => (RouteOrigin::Gps, target),
                    Some((&origin, &target)) => (origin.into(), target),
                    // Ignore request for invalid routes.
                    None => return,
                };

                let mode = match self.route.mode {
                    RouteMode::Pedestrian => RouteMode::Auto,
                    RouteMode::Auto => RouteMode::Pedestrian,
                };

                self.event_loop.insert_idle(move |state| {
                    state.window.views.search().route(origin, target, mode)
                });
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
    Cancel,
    Back,
    Mode,
}
