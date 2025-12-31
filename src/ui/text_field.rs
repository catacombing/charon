//! Text field input element.

use std::f32::consts::SQRT_2;
use std::io::Read;
use std::ops::{Bound, Range, RangeBounds};
use std::{cmp, mem};

use calloop::LoopHandle;
use skia_safe::textlayout::{LineMetrics, Paragraph};
use skia_safe::{
    Canvas as SkiaCanvas, Color4f, Paint, Path, Point as SkiaPoint, Rect, Shader, TileMode,
};
use smithay_client_toolkit::seat::keyboard::{Keysym, Modifiers};
use tracing::{error, warn};

use crate::State;
use crate::config::{Config, Input as InputConfig};
use crate::geometry::{Point, Size};
use crate::ui::rect_contains;
use crate::ui::skia::{RenderState, TextOptions};

/// Maximum number of surrounding bytes submitted to IME.
///
/// The value `4000` is chosen to match the maximum Wayland protocol message
/// size, a higher value will lead to errors.
const MAX_SURROUNDING_BYTES: usize = 4000;

/// Inner padding relative to font scale.
const TEXT_PADDING: f32 = 15.;

/// Selection caret size at scale 1.
const CARET_SIZE: f64 = 5.;

/// Caret outline width at scale 1.
const CARET_STROKE: f64 = 3.;

/// Single line text input field.
pub struct TextField {
    event_loop: LoopHandle<'static, State>,

    last_paragraph: Option<Paragraph>,
    last_cursor_rect: Option<Rect>,

    gradient_paint: Paint,
    font_scale: f32,
    paint: Paint,

    placeholder: &'static str,
    preedit_text: String,
    text: String,

    selection: Option<Range<usize>>,
    cursor_index: usize,

    touch_state: TouchState,
    scroll_offset: f64,
    keyboard_focused: bool,
    focus_cursor: bool,
    ime_focused: bool,

    point: Point<f32>,
    size: Size<f32>,
    scale: f64,

    text_input_dirty: bool,
    dirty: bool,
}

impl TextField {
    pub fn new(
        event_loop: LoopHandle<'static, State>,
        point: Point,
        size: Size,
        font_scale: f32,
    ) -> Self {
        let mut paint = Paint::default();
        paint.set_stroke_width(CARET_STROKE as f32);

        Self {
            event_loop,
            font_scale,
            paint,
            gradient_paint: Paint::default(),
            text_input_dirty: true,
            point: point.into(),
            size: size.into(),
            dirty: true,
            scale: 1.,
            keyboard_focused: Default::default(),
            last_cursor_rect: Default::default(),
            last_paragraph: Default::default(),
            scroll_offset: Default::default(),
            focus_cursor: Default::default(),
            cursor_index: Default::default(),
            preedit_text: Default::default(),
            ime_focused: Default::default(),
            placeholder: Default::default(),
            touch_state: Default::default(),
            selection: Default::default(),
            text: Default::default(),
        }
    }

    /// Render text content to the canvas.
    pub fn draw<'a>(
        &mut self,
        config: &Config,
        render_state: &mut RenderState<'a>,
        background: impl Into<Color4f>,
    ) {
        // Draw backdrop.
        let right = self.point.x + self.size.width;
        let bottom = self.point.y + self.size.height;
        let bg_rect = Rect::new(self.point.x, self.point.y, right, bottom);
        self.paint.set_color4f(background.into(), None);
        render_state.draw_rect(bg_rect, &self.paint);

        // Get selection range, defaulting to an empty selection.
        let selection = match self.selection.as_ref() {
            Some(selection) => selection.start..selection.end,
            None => self.text.len()..usize::MAX,
        };

        let fg = config.colors.foreground;
        let options = TextOptions::new().ellipsize(false);
        let mut paragraph_builder = render_state.paragraph(fg, self.font_scale, options);

        // Draw text before the selection, or entire text without selection.
        if selection.start > 0 {
            paragraph_builder.add_text(&self.text[..selection.start]);
        }

        // Draw selection and text after it.
        if selection.start < self.text.len() {
            let bg = config.colors.background;
            let hl = config.colors.highlight;

            paragraph_builder.push_style(render_state.selection_style(bg, hl, self.font_scale));
            paragraph_builder.add_text(&self.text[selection.start..selection.end]);

            paragraph_builder.pop();
            paragraph_builder.add_text(&self.text[selection.end..]);
        }

        // Handle preedit text and placeholder.
        if !self.preedit_text.is_empty() {
            // Draw preedit text in faint + underlined.
            paragraph_builder.push_style(render_state.preedit_style(fg, self.font_scale));
            paragraph_builder.add_text(&self.preedit_text);
        } else if self.text.is_empty() {
            // Draw placeholder text in faint.
            paragraph_builder.push_style(render_state.placeholder_style(fg, self.font_scale));
            paragraph_builder.add_text(self.placeholder);
        }

        // Layout text without bounds, since we handel boundaries with gradients and
        // clips.
        let mut paragraph = paragraph_builder.build();
        paragraph.layout(f32::MAX);
        self.last_paragraph = Some(paragraph);

        // Ensure cursor is within visible text section.
        //
        // XXX: This must happen after [`Self::last_paragraph`] is updated, since the
        // cursor offset otherwise cannot be calculated.
        if self.selection.is_none() {
            self.scroll_to(self.cursor_index);
        }
        let paragraph = self.last_paragraph.as_ref().unwrap();

        // Setup clipping mask to hide text outside the text field.
        render_state.save();
        let field_right = self.point.x + self.size.width;
        let field_bottom = self.point.y + self.size.height;
        let field_rect = Rect::new(self.point.x, self.point.y, field_right, field_bottom);
        render_state.clip_rect(field_rect, None, Some(false));

        // Render text vertically centered.
        let mut text_point = self.point;
        text_point.x += self.text_padding() + self.scroll_offset as f32;
        text_point.y += (self.size.height - paragraph.height()) / 2.;
        paragraph.paint(render_state, text_point);

        // Draw gradient to indicate text scroll availability.
        if let Some(shader) = self.gradient_shader(config.colors.alt_background) {
            self.gradient_paint.set_shader(shader);
            render_state.draw_rect(field_rect, &self.gradient_paint);
        }

        // Draw cursor or selection carets while focused.
        self.last_cursor_rect = if self.keyboard_focused || self.ime_focused {
            self.draw_cursor(config, render_state, text_point)
        } else {
            None
        };

        // Reset clipping mask used for text rendering.
        render_state.restore();

        // Clear dirtiness flag.
        //
        // This is inentionally placed after functions like `scroll_to_cursor`, since
        // these modify dirtiness but do not require another redraw.
        self.dirty = false;
    }

    /// Draw input or selection cursors.
    fn draw_cursor(
        &mut self,
        config: &Config,
        canvas: &SkiaCanvas,
        point: Point<f32>,
    ) -> Option<Rect> {
        match self.selection {
            Some(Range { start, end }) => {
                // Get points required for drawing the triangles.
                let (start_points, line_height) = self.caret_points(point, start)?;
                let start_path = Path::polygon(&start_points, true, None, true);
                let (end_points, _) = self.caret_points(point, end)?;
                let end_path = Path::polygon(&end_points, true, None, true);

                // Draw the caret outlines.
                self.paint.set_stroke(true);
                self.paint.set_color4f(Color4f::from(config.colors.highlight), None);
                canvas.draw_path(&start_path, &self.paint);
                canvas.draw_path(&end_path, &self.paint);

                // Draw the center/background.
                self.paint.set_stroke(false);
                self.paint.set_color4f(Color4f::from(config.colors.background), None);
                canvas.draw_path(&start_path, &self.paint);
                canvas.draw_path(&end_path, &self.paint);

                // Use entire selection as IME cursor rectangle.
                let start = start_points[2];
                let end = end_points[2];
                Some(Rect::new(start.x, start.y, end.x, end.y + line_height))
            },
            None => {
                // Get metrics at cursor position.
                let metrics = self.metrics_at(self.cursor_index)?;

                // Calculate cursor bounding box.
                let x = point.x + metrics.x;
                let y = point.y + metrics.baseline - metrics.ascent;
                let width = self.scale.round() as f32;
                let height = (metrics.ascent + metrics.descent).round();

                // Render the cursor rectangle.
                self.paint.set_color4f(Color4f::from(config.colors.foreground), None);
                let rect = Rect::new(x, y, x + width, y + height);
                canvas.draw_rect(rect, &self.paint);

                Some(rect)
            },
        }
    }

    /// Check whether the text box requires a redraw.
    pub fn dirty(&self) -> bool {
        self.dirty
    }

    /// Retrieve and reset current IME dirtiness state.
    pub fn take_text_input_dirty(&mut self) -> bool {
        mem::take(&mut self.text_input_dirty)
    }

    /// Update the text field's position.
    pub fn set_point(&mut self, point: impl Into<Point<f32>>) {
        let point = point.into();
        if self.point == point {
            return;
        }

        self.point = point;
        self.dirty = true;
    }

    /// Set the text box's physical size.
    pub fn set_size(&mut self, size: Size) {
        let size = size.into();
        if self.size == size {
            return;
        }

        self.size = size;
        self.dirty = true;

        // Ensure cursor is visible after resize.
        self.focus_cursor = true;
    }

    /// Set the text box's font scale.
    pub fn set_scale_factor(&mut self, scale: f64) {
        if self.scale == scale {
            return;
        }

        self.scale = scale;
        self.dirty = true;

        self.paint.set_stroke_width(self.stroke_size());
    }

    /// Check if a point lies within this text field.
    pub fn contains(&self, point: Point<f64>) -> bool {
        rect_contains(self.point, self.size, point.into())
    }

    /// Set keyboard focus state.
    pub fn set_keyboard_focus(&mut self, focused: bool) {
        self.dirty |= self.keyboard_focused != focused;
        self.keyboard_focused = focused;
    }

    /// Set IME focus state.
    pub fn set_ime_focus(&mut self, focused: bool) {
        self.dirty |= self.ime_focused != focused;
        self.ime_focused = focused;
    }

    /// Replace the entire text box content.
    pub fn set_text(&mut self, text: impl Into<String>) {
        let text = text.into();
        if self.text == text {
            return;
        }

        self.cursor_index = text.len();
        self.focus_cursor = true;
        self.text = text;

        self.clear_selection();

        self.text_input_dirty = true;
        self.dirty = true;
    }

    /// Get the field's current text content.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Set placeholder text, if no text is present.
    pub fn set_placeholder(&mut self, placeholder: &'static str) {
        self.placeholder = placeholder;
    }

    /// Handle new key press.
    pub fn press_key(&mut self, keysym: Keysym, modifiers: Modifiers) {
        // Ignore input with logo/alt key held.
        if modifiers.logo || modifiers.alt {
            return;
        }

        // Ensure cursor is visible after keyboard input.
        self.focus_cursor = true;

        match (keysym, modifiers.shift, modifiers.ctrl) {
            (Keysym::Left, false, false) => {
                self.cursor_index = match self.selection.take() {
                    Some(selection) => selection.start,
                    None => self.cursor_index.saturating_sub(1),
                };

                self.text_input_dirty = true;
                self.dirty = true;
            },
            (Keysym::Right, false, false) => {
                self.cursor_index = match self.selection.take() {
                    Some(selection) => selection.end,
                    None => cmp::min(self.cursor_index + 1, self.text.len()),
                };

                self.text_input_dirty = true;
                self.dirty = true;
            },
            (Keysym::BackSpace, false, false) => {
                if self.text.is_empty() {
                    return;
                }

                match self.selection.take() {
                    Some(selection) => self.delete_selected(selection),
                    None if self.cursor_index == 0 => return,
                    None => {
                        if self.text.is_empty() || self.cursor_index == 0 {
                            return;
                        }

                        // Jump to the previous character.
                        self.cursor_index = self.cursor_index.saturating_sub(1);
                        while self.cursor_index > 0
                            && !self.text.is_char_boundary(self.cursor_index)
                        {
                            self.cursor_index -= 1;
                        }

                        // Pop the character after the cursor.
                        self.text.remove(self.cursor_index);
                    },
                }

                self.text_input_dirty = true;
                self.dirty = true;
            },
            (Keysym::Delete, false, false) => {
                match self.selection.take() {
                    Some(selection) => self.delete_selected(selection),
                    None if self.cursor_index >= self.text.len() => return,
                    // Pop character after the cursor.
                    None => _ = self.text.remove(self.cursor_index),
                }

                self.text_input_dirty = true;
                self.dirty = true;
            },
            (Keysym::Return, false, false) => {
                // Submit current search.
                self.event_loop.insert_idle(move |state| {
                    state.window.views.search().submit_search();
                    state.window.unstall();
                });
            },
            (Keysym::XF86_Copy, ..) | (Keysym::C, true, true) => {
                // Get selected text.
                let text = match self.selection_text() {
                    Some(text) => text.to_owned(),
                    None => return,
                };

                self.event_loop.insert_idle(move |state| {
                    let serial = state.clipboard.next_serial();
                    let copy_paste_source = state
                        .protocol_states
                        .data_device_manager
                        .create_copy_paste_source(&state.window.queue, ["text/plain"]);
                    copy_paste_source.set_selection(&state.protocol_states.data_device, serial);
                    state.clipboard.source = Some(copy_paste_source);
                    state.clipboard.text = text;
                });
            },
            (Keysym::XF86_Paste, ..) | (Keysym::V, true, true) => {
                self.event_loop.insert_idle(|state| {
                    // Get available Wayland text selection.
                    let selection_offer =
                        match state.protocol_states.data_device.data().selection_offer() {
                            Some(selection_offer) => selection_offer,
                            None => return,
                        };
                    let mut pipe = match selection_offer.receive("text/plain".into()) {
                        Ok(pipe) => pipe,
                        Err(err) => {
                            warn!("Clipboard paste failed: {err}");
                            return;
                        },
                    };

                    // Read text from pipe.
                    let mut text = String::new();
                    if let Err(err) = pipe.read_to_string(&mut text) {
                        error!("Failed to read from clipboard pipe: {err}");
                        return;
                    }

                    // Paste text into text box.
                    state.window.paste(&text);
                });
            },
            (keysym, _, false) => {
                let key_char = match keysym.key_char() {
                    Some(key_char) => key_char,
                    None => return,
                };

                // Delete selection before writing new text.
                if let Some(selection) = self.selection.take() {
                    self.delete_selected(selection);
                }

                // Add text at cursor position.
                self.text.insert(self.cursor_index, key_char);

                // Move cursor behind inserted character.
                self.cursor_index += key_char.len_utf8();

                self.text_input_dirty = true;
                self.dirty = true;
            },
            _ => (),
        }
    }

    /// Handle touch press events.
    pub fn touch_down(&mut self, input_config: &InputConfig, time: u32, point: Point<f64>) {
        let offset = self.offset_at(point).unwrap_or(0);
        self.touch_state.down(input_config, time, point, offset);
    }

    /// Handle touch release.
    pub fn touch_motion(&mut self, input_config: &InputConfig, point: Point<f64>) {
        let delta = self.touch_state.motion(input_config, point, self.selection.as_ref());

        // Handle touch drag actions.
        match self.touch_state.action {
            TouchAction::Drag => {
                // Update scroll offset.
                let old_offset = self.scroll_offset;
                self.scroll_offset += delta.x;
                self.clamp_scroll_offset();

                // Force cursor to move within the new visible area.
                self.clamp_cursor();

                let dirty = self.scroll_offset != old_offset;
                self.text_input_dirty |= dirty;
                self.dirty |= dirty;
            },
            TouchAction::DragSelectionStart | TouchAction::DragSelectionEnd => {
                let offset = self.offset_at(point).unwrap_or(0);
                let selection = self.selection.as_mut().unwrap();

                // Update selection if it is at least one character wide.
                let modifies_start = self.touch_state.action == TouchAction::DragSelectionStart;
                if modifies_start && offset != selection.end {
                    selection.start = offset;
                } else if !modifies_start && offset != selection.start {
                    selection.end = offset;
                }

                // Swap modified side when input carets "overtake" each other.
                if selection.start > selection.end {
                    mem::swap(&mut selection.start, &mut selection.end);
                    self.touch_state.action = if modifies_start {
                        TouchAction::DragSelectionEnd
                    } else {
                        TouchAction::DragSelectionStart
                    };
                }

                // Ensure cursor is visible after selection change.
                self.focus_cursor = true;

                self.text_input_dirty = true;
                self.dirty = true;
            },
            // Ignore touch motion for tap actions.
            _ => (),
        }
    }

    /// Handle touch release.
    pub fn touch_up(&mut self) {
        // Ignore release handling for drag/focus actions.
        if matches!(
            self.touch_state.action,
            TouchAction::Drag | TouchAction::DragSelectionStart | TouchAction::DragSelectionEnd
        ) {
            return;
        }

        // Get byte offset from X/Y position.
        let point = self.touch_state.last_point;

        // Handle tap actions.
        match self.touch_state.action {
            TouchAction::Tap => {
                self.cursor_index = self.offset_at(point).unwrap_or(0);
                self.focus_cursor = true;

                self.clear_selection();

                self.text_input_dirty = true;
                self.dirty = true;
            },
            // Select word at touch position.
            TouchAction::DoubleTap => {
                let offset = self.offset_at(point).unwrap_or(0);

                let mut word_start = 0;
                let mut word_end = self.text.len();
                for (i, c) in self.text.char_indices() {
                    let c_end = i + c.len_utf8();
                    if c_end < offset && !c.is_alphanumeric() {
                        word_start = c_end;
                    } else if i > offset && !c.is_alphanumeric() {
                        word_end = i;
                        break;
                    }
                }

                self.select(word_start..word_end);
            },
            // Select everything.
            TouchAction::TripleTap => {
                let offset = self.offset_at(point).unwrap_or(0);
                let start = self.text[..offset].rfind('\n').map_or(0, |i| i + 1);
                let end = self.text[offset..].find('\n').map_or(self.text.len(), |i| offset + i);
                self.select(start..end);
            },
            TouchAction::Drag | TouchAction::DragSelectionStart | TouchAction::DragSelectionEnd => {
                unreachable!()
            },
        }
    }

    /// Paste text into the input element.
    pub fn paste(&mut self, text: &str) {
        // Delete selection before writing new text.
        if let Some(selection) = self.selection.take() {
            self.delete_selected(selection);
        }

        // Add text to input element.
        if self.cursor_index >= self.text.len() {
            self.text.push_str(text);
        } else {
            self.text.insert_str(self.cursor_index, text);
        }

        // Move cursor behind the new characters.
        self.cursor_index += text.len();
        self.focus_cursor = true;

        self.text_input_dirty = true;
        self.dirty = true;
    }

    /// Delete text around the current cursor position.
    pub fn delete_surrounding_text(&mut self, before_length: u32, after_length: u32) {
        // Calculate removal boundaries.
        let end = (self.cursor_index + after_length as usize).min(self.text.len());
        let start = self.cursor_index.saturating_sub(before_length as usize);

        // Remove all bytes in the range from the text.
        self.text.truncate(end);
        self.text = self.text.split_off(start);

        // Update cursor position.
        self.cursor_index = start;
        self.focus_cursor = true;

        self.text_input_dirty = true;
        self.dirty = true;
    }

    /// Insert text at the current cursor position.
    pub fn commit_string(&mut self, text: &str) {
        self.paste(text);
    }

    /// Set preedit text at the current cursor position.
    pub fn set_preedit_string(&mut self, text: String, _cursor_begin: i32, _cursor_end: i32) {
        // Ignore if preedit text did not change.
        if self.preedit_text == text {
            return;
        }

        // Delete selection as soon as preedit starts.
        if !text.is_empty()
            && let Some(selection) = self.selection.take()
        {
            self.delete_selected(selection);
        }

        self.preedit_text = text;
        self.focus_cursor = true;

        self.dirty = true;
    }

    /// Get surrounding text for IME.
    ///
    /// This will return at most `MAX_SURROUNDING_BYTES` bytes plus the current
    /// cursor positions relative to the surrounding text's origin.
    pub fn surrounding_text(&self) -> (String, i32, i32) {
        // Get up to half of `MAX_SURROUNDING_BYTES` after the cursor.
        let mut end = self.cursor_index + MAX_SURROUNDING_BYTES / 2;
        if end >= self.text.len() {
            end = self.text.len();
        } else {
            while end > 0 && !self.text.is_char_boundary(end) {
                end -= 1;
            }
        };

        // Get as many bytes as available before the cursor.
        let remaining = MAX_SURROUNDING_BYTES - (end - self.cursor_index);
        let mut start = self.cursor_index.saturating_sub(remaining);
        while start < self.text.len() && !self.text.is_char_boundary(start) {
            start += 1;
        }

        let (cursor_start, cursor_end) = match &self.selection {
            Some(selection) => (selection.start as i32, selection.end as i32),
            None => (self.cursor_index as i32, self.cursor_index as i32),
        };

        (self.text[start..end].into(), cursor_start - start as i32, cursor_end - start as i32)
    }

    /// Get physical dimensions of the last rendered cursor.
    pub fn last_cursor_rect(&self) -> Option<Rect> {
        self.last_cursor_rect
    }

    /// Modify text selection.
    fn select<R>(&mut self, range: R)
    where
        R: RangeBounds<usize>,
    {
        let mut start = match range.start_bound() {
            Bound::Included(start) => *start,
            Bound::Excluded(start) => *start + 1,
            Bound::Unbounded => usize::MIN,
        };
        start = start.max(0);
        let mut end = match range.end_bound() {
            Bound::Included(end) => *end + 1,
            Bound::Excluded(end) => *end,
            Bound::Unbounded => usize::MAX,
        };
        end = end.min(self.text.len());

        if start < end {
            self.selection = Some(start..end);

            // Ensure cursor is visible after selection change.
            self.focus_cursor = true;

            self.text_input_dirty = true;
            self.dirty = true;
        } else {
            self.clear_selection();
        }
    }

    /// Clear text selection.
    fn clear_selection(&mut self) {
        if self.selection.is_none() {
            return;
        }

        self.selection = None;

        self.text_input_dirty = true;
        self.dirty = true;
    }

    /// Get selection text.
    fn selection_text(&self) -> Option<&str> {
        let selection = self.selection.as_ref()?;
        Some(&self.text[selection.start..selection.end])
    }

    /// Delete the selected text.
    ///
    /// This automatically places the cursor at the start of the selection.
    fn delete_selected(&mut self, selection: Range<usize>) {
        // Remove selected text from input.
        self.text.drain(selection.start..selection.end);

        // Update cursor.
        self.cursor_index = selection.start;
        self.focus_cursor = true;

        self.text_input_dirty = true;
        self.dirty = true;
    }

    /// Get byte index at the specified absolute position.
    fn offset_at(&self, point: impl Into<Point<f32>>) -> Option<usize> {
        // Translate point into relative position.
        //
        // We can safely ignore the Y coordinate since we're dealing with one line.
        let mut point = point.into() - self.point;
        point.x -= self.text_padding() + self.scroll_offset as f32;
        point.y = 0.;

        // Get glyph cluster at the location.
        let paragraph = self.last_paragraph.as_ref()?;
        let cluster = paragraph.get_closest_glyph_cluster_at(point)?;

        // Calculate index based on position within the cluster.
        let width = cluster.bounds.right - cluster.bounds.left;
        let index = if point.x - cluster.bounds.left < width / 2. {
            cluster.text_range.start
        } else {
            cluster.text_range.end
        };

        Some(index.min(self.text.len()))
    }

    /// Get metrics for the glyph at the specified offset.
    fn metrics_at(&mut self, offset: usize) -> Option<GlyphMetrics> {
        let paragraph = self.last_paragraph.as_ref()?;

        if offset > 0 {
            let metrics = paragraph.get_line_metrics_at(0).unwrap();
            let cluster = paragraph.get_glyph_cluster_at(offset - 1);
            let x = cluster.map_or(0., |cluster| cluster.bounds.right);
            Some(GlyphMetrics::new(x, metrics))
        } else {
            let metrics = paragraph.get_line_metrics_at(0).unwrap();
            Some(GlyphMetrics::new(0., metrics))
        }
    }

    /// Get the caret's triangle points at the specified offset.
    fn caret_points(&mut self, offset: Point<f32>, index: usize) -> Option<([SkiaPoint; 3], f32)> {
        let caret_size = (CARET_SIZE * self.scale).round() as f32;
        let metrics = self.metrics_at(index)?;

        // Calculate width of the triangle outline at the tip.
        let stroke_point_width = SQRT_2 * self.stroke_size();

        let y = metrics.baseline - metrics.ascent - stroke_point_width / 2.;
        let line_height = metrics.ascent + metrics.descent;

        let points = [
            SkiaPoint::new(offset.x + metrics.x - caret_size, offset.y + y - caret_size),
            SkiaPoint::new(offset.x + metrics.x + caret_size, offset.y + y - caret_size),
            SkiaPoint::new(offset.x + metrics.x, offset.y + y),
        ];

        Some((points, line_height))
    }

    /// Clamp horizontal scroll offset.
    fn clamp_scroll_offset(&mut self) {
        let old_offset = self.scroll_offset;
        let max_offset = self.max_scroll_offset();
        self.scroll_offset = self.scroll_offset.clamp(max_offset, 0.);
        self.dirty |= old_offset != self.scroll_offset;
    }

    /// Get maximum viewport offset.
    fn max_scroll_offset(&self) -> f64 {
        let text_width = self.size.width - 2. * self.text_padding();

        self.last_paragraph
            .as_ref()
            .map_or(0., |p| -(p.max_intrinsic_width() - text_width).max(0.) as f64)
    }

    /// Update scroll offset to ensure byte index is visible.
    fn scroll_to(&mut self, index: usize) {
        let index_x = match self.metrics_at(index) {
            Some(metrics) => metrics.x as f64,
            // Reset scroll offset if no text for metrics is present.
            None => {
                self.scroll_offset = 0.;
                return;
            },
        };

        let text_width = (self.size.width - 2. * self.text_padding()) as f64;

        let new_offset = self.scroll_offset.clamp(-index_x, -index_x + text_width);
        self.dirty |= self.scroll_offset != new_offset;
        self.scroll_offset = new_offset;
    }

    /// Move cursor within the visible text section.
    fn clamp_cursor(&mut self) {
        let text_padding = self.text_padding();

        // We intentionally shrink the acceptable cursor range by 1 at each end, to
        // avoid the case where the index is rounded due to the position within the
        // glyph and we then revert the scroll offset change by scrolling the cursor
        // back into the visible region.

        let min_point = self.point + Point::new(text_padding, 0.);
        let min = self.offset_at(min_point).map_or(0, |min| min + 1);

        let max_point = self.point + Point::new(self.size.width - text_padding, 0.);
        let max = self.offset_at(max_point).map_or(usize::MAX, |max| max - 1);

        let new_index = self.cursor_index.clamp(min, max);
        self.dirty |= new_index != self.cursor_index;
        self.cursor_index = new_index;
    }

    /// Get the shader for drawing gradients at the left/right input borders.
    fn gradient_shader(&self, background: impl Into<Color4f>) -> Option<Shader> {
        // Calculate absolute gradient positions.
        let start = Point::new(self.point.x, self.point.y);
        let end = Point::new(self.point.x + self.size.width, self.point.y);
        let points = (start, end);

        // Create gradient color stops for both sides of the text field.
        let background = background.into();
        let transparent = Color4f { a: 0., ..background };
        let colors: &[Color4f] = &[background, transparent, transparent, background];

        // Position color stops to match text padding areas.
        let padding_percentage = self.text_padding() / self.size.width;
        let color_stops: &[f32] = &[0., padding_percentage, 1. - padding_percentage, 1.];

        Shader::linear_gradient(points, colors, color_stops, TileMode::Clamp, None, None)
    }

    /// Get text's X offset.
    fn text_padding(&self) -> f32 {
        TEXT_PADDING * self.scale as f32 * self.font_scale
    }

    /// Get the current caret stroke size.
    fn stroke_size(&self) -> f32 {
        (CARET_STROKE * self.scale) as f32
    }
}

#[derive(Default)]
struct TouchState {
    action: TouchAction,
    last_time: u32,
    last_point: Point<f64>,
    last_motion_point: Point<f64>,
    start_offset: usize,
}

impl TouchState {
    /// Update state from touch down event.
    fn down(&mut self, input_config: &InputConfig, time: u32, point: Point<f64>, offset: usize) {
        // Update touch action.
        let delta = point - self.last_point;
        self.action = if self.last_time + input_config.max_multi_tap.as_millis() as u32 >= time
            && delta.x.powi(2) + delta.y.powi(2) <= input_config.max_tap_distance
        {
            match self.action {
                TouchAction::Tap => TouchAction::DoubleTap,
                TouchAction::DoubleTap => TouchAction::TripleTap,
                _ => TouchAction::Tap,
            }
        } else {
            TouchAction::Tap
        };

        // Reset touch origin state.
        self.last_motion_point = point;
        self.start_offset = offset;
        self.last_point = point;
        self.last_time = time;
    }

    /// Update state from touch motion event.
    ///
    /// Returns the distance moved since the last touch down or motion.
    fn motion(
        &mut self,
        input_config: &InputConfig,
        point: Point<f64>,
        selection: Option<&Range<usize>>,
    ) -> Point<f64> {
        // Update incremental delta.
        let delta = point - self.last_motion_point;
        self.last_motion_point = point;

        // Never transfer out of drag/multi-tap states.
        if self.action != TouchAction::Tap {
            return delta;
        }

        // Ignore drags below the tap deadzone.
        let delta = point - self.last_point;
        if delta.x.powi(2) + delta.y.powi(2) <= input_config.max_tap_distance {
            return delta;
        }

        // Check if touch motion started on selection caret, with one character leeway.
        self.action = match selection {
            Some(selection) => {
                let start_delta = (self.start_offset as i32 - selection.start as i32).abs();
                let end_delta = (self.start_offset as i32 - selection.end as i32).abs();

                if end_delta <= start_delta && end_delta < 2 {
                    TouchAction::DragSelectionEnd
                } else if start_delta < 2 {
                    TouchAction::DragSelectionStart
                } else {
                    TouchAction::Drag
                }
            },
            _ => TouchAction::Drag,
        };

        delta
    }
}

/// Intention of a touch sequence.
#[derive(Default, PartialEq, Eq, Copy, Clone, Debug)]
enum TouchAction {
    #[default]
    Tap,
    Drag,
    DoubleTap,
    TripleTap,
    DragSelectionStart,
    DragSelectionEnd,
}

/// Glyph position metrics for a paragraph.
#[derive(Debug)]
struct GlyphMetrics {
    /// Baseline position from the top of the paragraph.
    baseline: f32,
    /// Glyph descent.
    descent: f32,
    /// Glyph ascent.
    ascent: f32,
    /// X position from the left of the paragraph.
    x: f32,
}

impl GlyphMetrics {
    fn new(x: f32, metrics: LineMetrics<'_>) -> Self {
        Self {
            x,
            baseline: metrics.baseline as f32,
            descent: metrics.descent as f32,
            ascent: metrics.ascent as f32,
        }
    }
}
