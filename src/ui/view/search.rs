//! Search UI view.

use std::any::Any;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Write;
use std::mem;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use calloop::LoopHandle;
use reqwest::Client;
use skia_safe::textlayout::TextAlign;
use skia_safe::{Color4f, Paint, Rect};
use smithay_client_toolkit::seat::keyboard::{Keysym, Modifiers};

use crate::config::{Config, Input};
use crate::geocoder::{Geocoder, QueryResult, ReverseQuery, SearchQuery};
use crate::geometry::{GeoPoint, Point, Size};
use crate::region::Regions;
use crate::router::{Mode as RouteMode, Router, RoutingQuery};
use crate::ui::skia::{RenderState, TextOptions};
use crate::ui::view::{self, UiView, View};
use crate::ui::{Button, Svg, TextField, Velocity, rect_contains};
use crate::{Error, State};

/// Padding around the screen edge at scale 1.
const OUTSIDE_PADDING: u32 = 16;

/// Back button width and height at scale 1.
const BUTTON_SIZE: u32 = 48;

/// Padding around the content of the search results at scale 1.
const RESULTS_INSIDE_PADDING: f64 = 16.;

/// Vertical space between search results at scale 1.
const RESULTS_Y_PADDING: f64 = 2.;

/// Region entry height at scale 1.
const RESULTS_HEIGHT: u32 = 100;

/// Size of the routing button inside geocoding search results at scale 1.
const ROUTING_BUTTON_SIZE: u32 = 32;

/// Padding between text inside the result entries at scale 1.
const TEXT_PADDING: f64 = 3.;

/// Search state text font size relative to the default.
const SEARCH_STATE_FONT_SIZE: f32 = 1.2;

/// Search result address text font size relative to the default.
const ADDRESS_FONT_SIZE: f32 = 0.6;

/// Search UI view.
pub struct SearchView {
    event_loop: LoopHandle<'static, State>,

    geocoder: Geocoder,
    router: Router,

    last_query: String,
    reference_point: GeoPoint,
    reference_zoom: u8,
    pending_reverse: bool,
    active_route: Option<(RouteOrigin, GeoPoint)>,
    route_origin: Option<RouteOrigin>,
    route_mode: RouteMode,
    gps: Option<GeoPoint>,

    cancel_route_button: Button,
    route_mode_button: Button,
    search_field: TextField,
    config_button: Button,
    search_button: Button,
    back_button: Button,
    gps_button: Button,
    bg_paint: Paint,
    error: &'static str,

    touch_state: TouchState,
    input_config: Input,
    scroll_offset: f64,

    keyboard_focused: bool,
    search_focused: bool,
    ime_focused: bool,

    size: Size,
    scale: f64,

    dirty: bool,
}

impl SearchView {
    #[cfg_attr(feature = "profiling", profiling::function)]
    pub fn new(
        event_loop: LoopHandle<'static, State>,
        client: Client,
        config: &Config,
        regions: Arc<Regions>,
        size: Size,
    ) -> Result<Self, Error> {
        let geocoder = Geocoder::new(event_loop.clone(), config, client.clone(), regions.clone())?;
        let router = Router::new(event_loop.clone(), config, client, regions)?;

        // Initialize UI elements.

        let mut bg_paint = Paint::default();
        bg_paint.set_color4f(Color4f::from(config.colors.background), None);

        let point = Self::back_button_point(size, 1.);
        let button_size = Self::button_size(1.);
        let back_button = Button::new(point, button_size, Svg::ArrowLeft);

        let point = Self::search_button_point(size, 1.);
        let search_button = Button::new(point, button_size, Svg::Search);

        let point = Self::config_button_point(size, 1.);
        let config_button = Button::new(point, button_size, Svg::Config);

        let point = Self::gps_button_point(size, 1.);
        let gps_button = Button::new(point, button_size, Svg::Gps);

        let point = Self::cancel_route_button_point(size, 1.);
        let cancel_route_button = Button::new(point, button_size, Svg::CancelRoute);

        let route_mode = RouteMode::default();
        let point = Self::route_mode_button_point(size, 1.);
        let route_mode_button = Button::new(point, button_size, route_mode.svg());

        let search_size = Self::search_field_size(size, 1.);
        let point = Self::search_field_point(size, 1.);
        let mut search_field = TextField::new(event_loop.clone(), point, search_size, 1.);
        search_field.set_placeholder("Search…");

        Ok(Self {
            cancel_route_button,
            route_mode_button,
            config_button,
            search_button,
            search_field,
            back_button,
            event_loop,
            gps_button,
            route_mode,
            bg_paint,
            geocoder,
            router,
            size,
            input_config: config.input,
            search_focused: true,
            dirty: true,
            scale: 1.,
            keyboard_focused: Default::default(),
            pending_reverse: Default::default(),
            reference_point: Default::default(),
            reference_zoom: Default::default(),
            scroll_offset: Default::default(),
            ime_focused: Default::default(),
            touch_state: Default::default(),
            last_query: Default::default(),
            active_route: Default::default(),
            route_origin: Default::default(),
            error: Default::default(),
            gps: Default::default(),
        })
    }

    /// Get mutable access to the geocoder.
    pub fn geocoder_mut(&mut self) -> &mut Geocoder {
        &mut self.geocoder
    }

    /// Get mutable access to the routing engine.
    pub fn router_mut(&mut self) -> &mut Router {
        &mut self.router
    }

    /// Mark view for a redraw.
    pub fn set_dirty(&mut self) {
        self.dirty = true;
    }

    /// Update the search reference point.
    pub fn set_reference(&mut self, point: GeoPoint, zoom: u8) {
        self.reference_point = point;
        self.reference_zoom = zoom;
    }

    /// Update the current GPS location.
    pub fn set_gps(&mut self, point: Option<GeoPoint>) {
        self.dirty |= self.gps != point;
        self.gps = point;
    }

    /// Set an error message indicating that an operation has failed.
    pub fn set_error(&mut self, error: &'static str) {
        self.dirty |= self.error != error;
        self.error = error;
    }

    /// Submit current search field text for geocoding.
    pub fn submit_search(&mut self) {
        self.last_query = self.search_field.text().to_owned();
        self.dirty = true;

        if self.last_query.trim().is_empty() {
            // Reset search without query.
            self.geocoder.reset();
        } else {
            // Submit background query.
            let mut query = SearchQuery::new(&self.last_query);
            query.set_reference(self.reference_point, self.reference_zoom);
            self.geocoder.search(query);
        }

        // Clear current POI map marker.
        self.event_loop.insert_idle(move |state| state.window.views.map().set_poi(None));
    }

    /// Run reverse geocoding search.
    pub fn reverse(&mut self, point: GeoPoint, zoom: u8) {
        self.last_query = format!("{} {}", point.lat, point.lon);
        self.pending_reverse = true;
        self.dirty = true;

        // Submit background query.
        let query = ReverseQuery::new(point, zoom);
        self.geocoder.reverse(query);

        self.search_field.set_text("");
    }

    /// Set the origin and target of the current route, if one is active.
    pub fn set_route(&mut self, route: Option<(RouteOrigin, GeoPoint)>) {
        self.dirty |= self.active_route != route;
        self.active_route = route;
    }

    /// Start a new route calculation.
    pub fn route(&mut self, origin: RouteOrigin, target: GeoPoint) {
        // Determine route origin and whether the route should be updated from GPS.
        let (origin, is_gps_route) = match origin {
            RouteOrigin::GeoPoint(origin) => (origin, false),
            RouteOrigin::Gps => match self.gps {
                Some(origin) => (origin, true),
                // Reset routing if GPS routing was requested but we lost the GPS signal.
                None => {
                    self.route_origin = None;
                    self.dirty = true;
                    return;
                },
            },
        };

        self.search_field.set_text("");
        self.route_origin = None;
        self.dirty = true;

        self.geocoder.reset();

        // Submit background query.
        let query = RoutingQuery::new(origin, target, self.route_mode);
        self.router.route(query, is_gps_route);
    }

    /// Set origin for routing and start route target selection.
    fn set_route_origin(&mut self, origin: RouteOrigin) {
        self.route_origin = Some(origin);
        self.search_field.set_text("");
        self.geocoder.reset();
        self.dirty = true;
    }

    /// Draw a geocoding search result entry.
    #[cfg_attr(feature = "profiling", profiling::function)]
    fn draw_geocoding_result<'a>(
        &self,
        config: &Config,
        render_state: &mut RenderState<'a>,
        point: Point,
        size: Size,
        result: &QueryResult,
    ) {
        let padding = (RESULTS_INSIDE_PADDING * self.scale).round() as f32;
        let mut routing_button_point = self.routing_button_point();
        let routing_button_size = self.routing_button_size();

        let text_width = routing_button_point.x as f32 - padding * 2.;
        let mut text_point = point;
        text_point.x += padding as i32;

        // Draw background.
        let bg_width = point.x as f32 + size.width as f32;
        let bg_height = point.y as f32 + size.height as f32;
        let bg_rect = Rect::new(point.x as f32, point.y as f32, bg_width, bg_height);
        render_state.draw_rect(bg_rect, &self.bg_paint);

        // Layout title and distance text.

        let mut builder = render_state.paragraph(config.colors.foreground, 1., None);
        builder.add_text(&result.title);

        let mut title_paragraph = builder.build();
        title_paragraph.layout(text_width);

        // Layout entity type and distance text.

        let options = TextOptions::new().ellipsize(true);
        let mut builder =
            render_state.paragraph(config.colors.foreground, ADDRESS_FONT_SIZE, options);
        let entity_text = match result.distance {
            Some(distance) => {
                let mut text =
                    String::with_capacity(result.entity_type.len() + " · XXXXX km".len());
                let _ = write!(&mut text, "{} · ", result.entity_type);
                view::format_distance(&mut text, distance);
                Cow::Owned(text)
            },
            None => Cow::Borrowed(result.entity_type),
        };
        builder.add_text(entity_text);

        let mut entity_paragraph = builder.build();
        entity_paragraph.layout(text_width);

        // Layout address text.

        let options = TextOptions::new().ellipsize(false);
        let mut builder =
            render_state.paragraph(config.colors.alt_foreground, ADDRESS_FONT_SIZE, options);
        builder.add_text(&result.address);

        let mut address_paragraph = builder.build();
        address_paragraph.layout(text_width);

        // Draw all labels.

        let text_padding = (TEXT_PADDING * self.scale).round() as i32;
        let title_text_height = title_paragraph.height().round() as i32;
        let entity_text_height = entity_paragraph.height().round() as i32 + text_padding;
        let address_text_height = address_paragraph.height().round() as i32 + text_padding;

        text_point.y +=
            (size.height as i32 - entity_text_height - title_text_height - address_text_height) / 2;

        title_paragraph.paint(render_state, text_point);
        text_point.y += title_text_height + text_padding;

        entity_paragraph.paint(render_state, text_point);
        text_point.y += entity_text_height + text_padding;

        address_paragraph.paint(render_state, text_point);

        // Draw routing button.
        routing_button_point += point;
        render_state.draw_svg(Svg::Route, routing_button_point, routing_button_size);
    }

    /// Physical location of the search text field.
    fn search_field_point(size: Size, scale: f64) -> Point {
        let search_button_point = Self::search_button_point(size, scale);
        let padding = (OUTSIDE_PADDING as f64 * scale).round() as i32;
        let button_width = Self::button_size(scale).width as i32;

        let x = search_button_point.x + button_width + padding;

        Point::new(x, search_button_point.y)
    }

    /// Physical size of the search text field.
    fn search_field_size(size: Size, scale: f64) -> Size {
        let view_width = (size.width as f64 * scale).round() as u32;
        let padding = (OUTSIDE_PADDING as f64 * scale).round() as u32;
        let button_size = Self::button_size(scale);

        let width = view_width - 2 * button_size.width - 4 * padding;

        Size::new(width, button_size.height)
    }

    /// Physical location of the search button.
    fn search_button_point(size: Size, scale: f64) -> Point {
        let padding = (OUTSIDE_PADDING as f64 * scale).round() as i32;
        let button_size = Self::button_size(scale);
        let physical_size = size * scale;

        let y = (physical_size.height - button_size.height) as i32 - padding;

        Point::new(padding, y)
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

    /// Physical location of the config button.
    fn config_button_point(size: Size, scale: f64) -> Point {
        let padding = (OUTSIDE_PADDING as f64 * scale).round() as i32;
        let button_size = Self::button_size(scale);
        let physical_size = size * scale;

        let x = (physical_size.width - button_size.width) as i32 - padding;
        let y = (physical_size.height - button_size.height * 2) as i32 - padding * 2;

        Point::new(x, y)
    }

    /// Physical location of the GPS location button.
    fn gps_button_point(size: Size, scale: f64) -> Point {
        let config_button_point = Self::config_button_point(size, scale);
        let padding = (OUTSIDE_PADDING as f64 * scale).round() as i32;
        let button_size = Self::button_size(scale);

        let x = config_button_point.x - button_size.width as i32 - padding;

        Point::new(x, config_button_point.y)
    }

    /// Physical location of the route cancellation button.
    fn cancel_route_button_point(size: Size, scale: f64) -> Point {
        let config_button_point = Self::config_button_point(size, scale);
        let padding = (OUTSIDE_PADDING as f64 * scale).round() as i32;
        let button_size = Self::button_size(scale);

        let y = config_button_point.y - button_size.height as i32 - padding;

        Point::new(config_button_point.x, y)
    }

    /// Physical location of the route travel mode button.
    fn route_mode_button_point(size: Size, scale: f64) -> Point {
        let config_button_point = Self::config_button_point(size, scale);
        let padding = (OUTSIDE_PADDING as f64 * scale).round() as i32;
        let button_size = Self::button_size(scale);

        let y = config_button_point.y - button_size.height as i32 - padding;
        let x = config_button_point.x - button_size.width as i32 - padding;

        Point::new(x, y)
    }

    /// Physical size of the back/search buttons.
    fn button_size(scale: f64) -> Size {
        Size::new(BUTTON_SIZE, BUTTON_SIZE) * scale
    }

    /// Physical point of the bottommost search result entry.
    fn result_point(&self) -> Point {
        let search_button_point = Self::search_button_point(self.size, self.scale);
        let outside_padding = (OUTSIDE_PADDING as f64 * self.scale).round() as i32;
        let result_size = self.result_size();

        let x = search_button_point.x;
        let y = search_button_point.y - outside_padding - result_size.height as i32;

        Point::new(x, y)
    }

    /// Physical size of a search result.
    fn result_size(&self) -> Size {
        let outside_padding = (OUTSIDE_PADDING as f64 * self.scale).round() as u32;
        let size = self.size * self.scale;

        let width = size.width - outside_padding * 2;
        let height = (RESULTS_HEIGHT as f64 * self.scale).round() as u32;

        Size::new(width, height)
    }

    /// Physical point of the routing button relative to the result origin.
    fn routing_button_point(&self) -> Point {
        let padding = (RESULTS_INSIDE_PADDING * self.scale).round() as i32;
        let button_size = self.routing_button_size();
        let result_size = self.result_size();

        let x = (result_size.width - button_size.width) as i32 - padding;
        let y = (result_size.height - button_size.height) as i32 / 2;

        Point::new(x, y)
    }

    /// Physical size of the routing button.
    fn routing_button_size(&self) -> Size {
        Size::new(ROUTING_BUTTON_SIZE, ROUTING_BUTTON_SIZE) * self.scale
    }

    /// Get current search results.
    fn results(&self) -> &[QueryResult] {
        if self.router.routing() { &[] } else { self.geocoder.results() }
    }

    /// Check whether the config/gps buttons should be rendered.
    fn show_extra_buttons(&self) -> bool {
        self.results().is_empty() && !self.geocoder.searching() && !self.router.routing()
    }

    /// Check whether the route cancellation/travel mode buttons should be
    /// rendered.
    fn show_route_buttons(&self) -> bool {
        self.show_extra_buttons() && (self.route_origin.is_some() || self.active_route.is_some())
    }

    /// Get result at the specified location.
    fn result_at(&self, mut point: Point<f64>) -> Option<(&QueryResult, bool)> {
        let result_point = self.result_point();
        let result_size = self.result_size();
        let results_end = result_point.y as f64 + result_size.height as f64;

        // Short-circuit if point is outside the results list.
        if point.x < result_point.x as f64
            || point.x >= result_point.x as f64 + result_size.width as f64
            || point.y >= results_end
        {
            return None;
        }

        // Apply current scroll offset.
        point.y -= self.scroll_offset;

        // Ignore taps within vertical padding.
        let results_height = result_size.height as f64 + RESULTS_Y_PADDING * self.scale;
        let bottom_relative = results_end - point.y - 1.;
        if bottom_relative % results_height >= result_size.height as f64 {
            return None;
        }

        // Find index at the specified offset.
        let index = (bottom_relative / results_height).floor() as usize;
        let result = self.results().get(index)?;

        // Check whether the tap is within the result's button.
        let relative_x = point.x - result_point.x as f64;
        let relative_y = results_height - 1. - (bottom_relative % results_height);
        let relative_point = Point::new(relative_x, relative_y);
        let routing_button_point: Point<f64> = self.routing_button_point().into();
        let routing_button_size: Size<f64> = self.routing_button_size().into();
        let button_pressed =
            rect_contains(routing_button_point, routing_button_size, relative_point);

        Some((result, button_pressed))
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
        let results_padding = (RESULTS_Y_PADDING * self.scale).round() as usize;
        let result_height = self.result_size().height as usize;

        // Calculate height of all results plus top padding.
        let results_count = self.results().len();
        let results_height = (results_count * (result_height + results_padding))
            .saturating_sub(results_padding)
            + outside_padding;

        // Calculate tab content outside the viewport.
        results_height.saturating_sub(self.result_point().y as usize + result_height)
    }
}

impl UiView for SearchView {
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

        // Ensure background paint is up to date.
        self.bg_paint.set_color4f(Color4f::from(config.colors.alt_background), None);

        render_state.clear(config.colors.background);

        // Calculate results list geometry.

        let padding = (RESULTS_Y_PADDING * self.scale).round() as i32;
        let result_size = self.result_size();

        let results_start = self.result_point();
        let mut result_point = results_start;
        result_point.y += self.scroll_offset.round() as i32;

        // Set clipping mask to cut off results overlapping the bottom buttons.
        let bottom = results_start.y as f32 + result_size.height as f32;
        let clip_rect = Rect::new(0., 0., size.width as f32, bottom);
        render_state.save();
        render_state.clip_rect(clip_rect, None, Some(false));

        // Draw query results.
        let results = self.results();
        for result in results {
            if result_point.y > results_start.y + (result_size.height as i32) {
                result_point.y -= result_size.height as i32 + padding;
                continue;
            } else if result_point.y + (result_size.height as i32) < 0 {
                break;
            }

            self.draw_geocoding_result(
                config,
                &mut render_state,
                result_point,
                result_size,
                result,
            );
            result_point.y -= result_size.height as i32 + padding;
        }

        // Reset region clipping mask.
        render_state.restore();

        // Draw current search status indicator.
        if results.is_empty() {
            let msg = match (self.route_origin, self.geocoder.searching(), self.router.routing()) {
                (_, _, true) => Cow::Borrowed("Calculating Route …"),
                (_, true, _) => Cow::Owned(format!("Searching for \"{}\" …", self.last_query)),
                (Some(_), ..) => Cow::Borrowed("Enter Destination"),
                (None, false, false) if self.error.is_empty() => {
                    Cow::Borrowed("Search for an Address or POI")
                },
                (None, false, false) => Cow::Borrowed(self.error),
            };

            let options = TextOptions::new().ellipsize(false).align(TextAlign::Center);
            let mut builder = render_state.paragraph(
                config.colors.alt_foreground,
                SEARCH_STATE_FONT_SIZE,
                options,
            );
            builder.add_text(msg);

            let mut paragraph = builder.build();
            let outside_padding = (OUTSIDE_PADDING as f64 * self.scale).round();
            paragraph.layout(size.width as f32 - outside_padding as f32);

            let result_end = results_start.y as f32 + result_size.height as f32;
            let y = (result_end - paragraph.height()) / 2.;
            paragraph.paint(&render_state, Point::new(0., y));
        }

        // Render input elements.

        self.search_field.draw(config, &mut render_state, config.colors.alt_background);

        if self.show_extra_buttons() {
            if self.show_route_buttons() {
                self.cancel_route_button.draw(&mut render_state, config.colors.alt_background);
                self.route_mode_button.draw(&mut render_state, config.colors.alt_background);
            }
            if self.gps.is_some() {
                self.gps_button.draw(&mut render_state, config.colors.alt_background);
            }
            self.config_button.draw(&mut render_state, config.colors.alt_background);
        }
        self.search_button.draw(&mut render_state, config.colors.alt_background);
        self.back_button.draw(&mut render_state, config.colors.alt_background);
    }

    fn dirty(&self) -> bool {
        self.dirty || self.touch_state.velocity.is_moving() || self.search_field.dirty()
    }

    fn enter(&mut self) {
        self.error = "";

        // Focus input on enter, unless view was opened for reverse geocoding.
        if mem::take(&mut self.pending_reverse) {
            self.search_field.set_keyboard_focus(false);
            self.search_field.set_ime_focus(false);
            self.search_focused = false;
        } else {
            self.search_field.set_keyboard_focus(self.keyboard_focused);
            self.search_field.set_ime_focus(self.ime_focused);
            self.search_focused = true;
        }
    }

    #[cfg_attr(feature = "profiling", profiling::function)]
    fn set_size(&mut self, size: Size) {
        self.size = size;
        self.dirty = true;

        // Update UI elements.

        self.cancel_route_button.set_point(Self::cancel_route_button_point(size, self.scale));
        self.route_mode_button.set_point(Self::route_mode_button_point(size, self.scale));
        self.config_button.set_point(Self::config_button_point(size, self.scale));
        self.search_button.set_point(Self::search_button_point(size, self.scale));
        self.back_button.set_point(Self::back_button_point(size, self.scale));
        self.gps_button.set_point(Self::gps_button_point(size, self.scale));

        self.search_field.set_point(Self::search_field_point(size, self.scale));
        self.search_field.set_size(Self::search_field_size(size, self.scale));
    }

    #[cfg_attr(feature = "profiling", profiling::function)]
    fn set_scale_factor(&mut self, scale: f64) {
        self.scale = scale;
        self.dirty = true;

        // Update UI elements.

        let button_size = Self::button_size(scale);

        self.cancel_route_button.set_point(Self::cancel_route_button_point(self.size, scale));
        self.cancel_route_button.set_size(button_size);

        self.route_mode_button.set_point(Self::route_mode_button_point(self.size, scale));
        self.route_mode_button.set_size(button_size);

        self.config_button.set_point(Self::config_button_point(self.size, scale));
        self.config_button.set_size(button_size);

        self.search_button.set_point(Self::search_button_point(self.size, scale));
        self.search_button.set_size(button_size);

        self.back_button.set_point(Self::back_button_point(self.size, scale));
        self.back_button.set_size(button_size);

        self.gps_button.set_point(Self::gps_button_point(self.size, scale));
        self.gps_button.set_size(button_size);

        self.search_field.set_point(Self::search_field_point(self.size, scale));
        self.search_field.set_scale_factor(scale);
        self.search_field.set_size(button_size);
    }

    #[cfg_attr(feature = "profiling", profiling::function)]
    fn touch_down(&mut self, slot: i32, time: u32, point: Point<f64>) {
        // Cancel velocity if a new touch sequence starts.
        self.touch_state.velocity.stop();

        // Only allow a single active touch slot.
        if !self.touch_state.slots.is_empty() {
            return;
        }

        let point = point * self.scale;

        // Handle focus changes for search field input.
        self.search_focused = self.search_field.contains(point);
        if self.search_focused {
            self.search_field.set_keyboard_focus(self.keyboard_focused);
            self.search_field.set_ime_focus(self.ime_focused);
        } else {
            self.search_field.set_keyboard_focus(false);
            self.search_field.set_ime_focus(false);
        }

        // Determine goal of this touch sequence.
        let show_extra_buttons = self.show_extra_buttons();
        self.touch_state.action = if self.search_focused {
            self.search_field.touch_down(&self.input_config, time, point);
            TouchAction::SearchField
        } else if self.show_route_buttons() && self.cancel_route_button.contains(point) {
            TouchAction::CancelRoute
        } else if self.show_route_buttons() && self.route_mode_button.contains(point) {
            TouchAction::RouteMode
        } else if show_extra_buttons && self.gps.is_some() && self.gps_button.contains(point) {
            TouchAction::RouteGps
        } else if show_extra_buttons && self.config_button.contains(point) {
            TouchAction::Config
        } else if self.search_button.contains(point) {
            TouchAction::Search
        } else if self.back_button.contains(point) {
            TouchAction::Back
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

        match self.touch_state.action {
            // Handle action transitions.
            TouchAction::Tap | TouchAction::Drag => {
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
            },
            TouchAction::SearchField => self.search_field.touch_motion(&self.input_config, point),
            _ => (),
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
            TouchAction::Tap => match self.result_at(removed.point) {
                Some((&QueryResult { point, ref address, .. }, false)) => {
                    let zoom = zoom_from_address(address);
                    self.event_loop.insert_idle(move |state| {
                        let map_view = state.window.views.map();
                        map_view.goto(point, zoom);
                        map_view.set_poi(Some(point));
                        state.window.set_view(View::Map);
                    });
                },
                Some((&QueryResult { point, .. }, true)) => match self.route_origin {
                    Some(origin) => self.route(origin, point),
                    None => self.set_route_origin(point.into()),
                },
                None => (),
            },
            TouchAction::Config
                if self.show_extra_buttons() && self.config_button.contains(removed.point) =>
            {
                self.event_loop.insert_idle(|state| state.window.set_view(View::Download));
            },
            TouchAction::CancelRoute
                if self.show_route_buttons()
                    && self.cancel_route_button.contains(removed.point) =>
            {
                self.event_loop.insert_idle(|state| state.window.views.map().cancel_route());
                self.active_route = None;
                self.route_origin = None;
                self.dirty = true;
            },
            TouchAction::RouteMode
                if self.show_route_buttons() && self.route_mode_button.contains(removed.point) =>
            {
                self.route_mode = match self.route_mode {
                    RouteMode::Pedestrian => RouteMode::Auto,
                    RouteMode::Auto => RouteMode::Pedestrian,
                };
                self.route_mode_button.set_svg(self.route_mode.svg());
                self.dirty = true;

                // Resubmit route if one is already active.
                if let Some((origin, target)) = self.active_route {
                    self.route(origin, target);
                }
            },
            TouchAction::RouteGps
                if self.show_extra_buttons() && self.gps_button.contains(removed.point) =>
            {
                match (self.gps, self.route_origin) {
                    (Some(gps), Some(origin)) => self.route(origin, gps),
                    (Some(_), None) => self.set_route_origin(RouteOrigin::Gps),
                    (None, _) => (),
                }
            },
            TouchAction::Search if self.search_button.contains(removed.point) => {
                self.submit_search()
            },
            TouchAction::Back if self.back_button.contains(removed.point) => {
                self.event_loop.insert_idle(|state| state.window.set_view(View::Map));
            },
            TouchAction::SearchField => self.search_field.touch_up(),
            _ => (),
        }
    }

    fn keyboard_enter(&mut self) {
        self.keyboard_focused = true;

        if self.search_focused {
            self.search_field.set_keyboard_focus(true);
        }
    }

    fn keyboard_leave(&mut self) {
        self.keyboard_focused = false;

        // Always remove focus, since it's idempotent anyway.
        self.search_field.set_keyboard_focus(false);
    }

    fn press_key(&mut self, _raw: u32, keysym: Keysym, modifiers: Modifiers) {
        self.search_field.press_key(keysym, modifiers);
    }

    fn paste(&mut self, text: &str) {
        self.search_field.paste(text);
    }

    fn text_input_enter(&mut self) {
        self.ime_focused = true;

        if self.search_focused {
            self.search_field.set_ime_focus(true);
        }
    }

    fn text_input_leave(&mut self) {
        self.ime_focused = false;

        // Always remove focus, since it's idempotent anyway.
        self.search_field.set_ime_focus(false);
    }

    fn delete_surrounding_text(&mut self, before_length: u32, after_length: u32) {
        self.search_field.delete_surrounding_text(before_length, after_length);
    }

    fn commit_string(&mut self, text: String) {
        self.search_field.commit_string(&text);
    }

    fn set_preedit_string(&mut self, text: String, cursor_begin: i32, cursor_end: i32) {
        self.search_field.set_preedit_string(text, cursor_begin, cursor_end);
    }

    fn take_text_input_dirty(&mut self) -> bool {
        self.search_field.take_text_input_dirty()
    }

    fn text_input_enabled(&self) -> bool {
        self.search_focused
    }

    fn surrounding_text(&self) -> (String, i32, i32) {
        self.search_field.surrounding_text()
    }

    fn last_cursor_geometry(&self) -> Option<(Point, Size)> {
        let rect = self.search_field.last_cursor_rect()?;
        let point = Point::new(rect.left, rect.top).into();
        let size = Size::new(rect.right - rect.left, rect.bottom - rect.top).into();
        Some((point, size))
    }

    #[cfg_attr(feature = "profiling", profiling::function)]
    fn update_config(&mut self, config: &Config) {
        self.geocoder.update_config(config);
        self.router.update_config(config);

        if self.input_config != config.input {
            self.input_config = config.input;
            self.dirty = true;
        }
    }

    fn as_any(&mut self) -> &mut dyn Any {
        self
    }
}

/// Unique ID of a search query.
#[derive(PartialEq, Eq, Copy, Clone)]
pub struct QueryId(u64);

impl QueryId {
    pub fn new() -> Self {
        static NEXT_QUERY_ID: AtomicU64 = AtomicU64::new(0);
        Self(NEXT_QUERY_ID.fetch_add(1, Ordering::Relaxed))
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
    SearchField,
    CancelRoute,
    RouteMode,
    RouteGps,
    Search,
    Config,
    Back,
    Drag,
    #[default]
    Tap,
}

/// Routing origin point source.
#[derive(PartialEq, Copy, Clone)]
pub enum RouteOrigin {
    GeoPoint(GeoPoint),
    Gps,
}

impl From<GeoPoint> for RouteOrigin {
    fn from(point: GeoPoint) -> Self {
        Self::GeoPoint(point)
    }
}

/// Get zoom level necessary to make an address fully or mostly visible.
fn zoom_from_address(address: &str) -> u8 {
    match address.matches(',').count() {
        0 => 6,
        1 => 7,
        2 | 3 => 11,
        _ => 18,
    }
}
