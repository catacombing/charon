//! UI render views.

use std::fmt::Write;
use std::ops::{Deref, DerefMut};

use calloop::LoopHandle;
use smithay_client_toolkit::seat::keyboard::{Keysym, Modifiers};

use crate::config::Config;
use crate::db::Db;
use crate::geometry::{Point, Size};
use crate::region::Regions;
use crate::ui::skia::RenderState;
use crate::ui::view::download::DownloadView;
use crate::ui::view::map::MapView;
use crate::ui::view::route::RouteView;
use crate::ui::view::search::SearchView;
use crate::{Error, State};

pub mod download;
pub mod map;
pub mod route;
pub mod search;

pub trait UiView {
    /// Redraw the view.
    fn draw<'a>(&mut self, config: &Config, render_state: RenderState<'a>);

    /// Check whether the view requires a redraw.
    fn dirty(&self) -> bool;

    /// Indicate this view was newly activated.
    fn enter(&mut self) {}

    /// Update the view's logical size.
    fn set_size(&mut self, size: Size);

    /// Update the view's DPI factor.
    fn set_scale_factor(&mut self, scale: f64);

    /// Handle touch press.
    fn touch_down(&mut self, slot: i32, time: u32, point: Point<f64>);

    /// Handle touch motion.
    fn touch_motion(&mut self, id: i32, point: Point<f64>);

    /// Handle touch release.
    fn touch_up(&mut self, slot: i32);

    /// Handle keyboard focus.
    fn keyboard_enter(&mut self) {}

    /// Handle keyboard focus loss.
    fn keyboard_leave(&mut self) {}

    /// Handle keyboard key press.
    fn press_key(&mut self, _raw: u32, _keysym: Keysym, _modifiers: Modifiers) {}

    /// Paste text into the view.
    fn paste(&mut self, _text: &str) {}

    /// Handle IME focus.
    fn text_input_enter(&mut self) {}

    /// Handle IME focus loss.
    fn text_input_leave(&mut self) {}

    /// Delete text around the current cursor position.
    fn delete_surrounding_text(&mut self, _before_length: u32, _after_length: u32) {}

    /// Insert text at the current cursor position.
    fn commit_string(&mut self, _text: String) {}

    /// Set preedit text at the current cursor position.
    fn set_preedit_string(&mut self, _text: String, _cursor_begin: i32, _cursor_end: i32) {}

    /// Retrieve and reset current IME dirtiness state.
    fn take_text_input_dirty(&mut self) -> bool {
        false
    }

    /// Get current IME state.
    fn text_input_enabled(&self) -> bool {
        false
    }

    /// Get surrounding text for IME.
    ///
    /// This will return at most `MAX_SURROUNDING_BYTES` bytes plus the current
    /// cursor positions relative to the surrounding text's origin.
    fn surrounding_text(&self) -> (String, i32, i32) {
        (String::new(), 0, 0)
    }

    /// Get physical dimensions of the last rendered cursor.
    fn last_cursor_geometry(&self) -> Option<(Point, Size)> {
        None
    }

    /// Handle config updates.
    fn update_config(&mut self, config: &Config);
}

/// Available UI views.
#[derive(Default, PartialEq, Eq, Copy, Clone, Debug)]
pub enum View {
    #[default]
    Map,
    Route,
    Search,
    Download,
}

/// UI view tracking.
pub struct Views {
    download: DownloadView,
    search: SearchView,
    route: RouteView,
    map: MapView,
    active_view: View,
}

impl Views {
    pub fn new(
        event_loop: &LoopHandle<'static, State>,
        config: &Config,
        db: Db,
        size: Size,
    ) -> Result<Self, Error> {
        let client = crate::http_client()?;

        // Create geographic region manager.
        let regions = Regions::new(event_loop.clone(), client.clone(), db.clone())?;

        let download = DownloadView::new(event_loop.clone(), config, regions.clone(), size)?;
        let search =
            SearchView::new(event_loop.clone(), client.clone(), config, regions.clone(), size)?;
        let route = RouteView::new(event_loop.clone(), config, size)?;
        let map = MapView::new(event_loop.clone(), client, db, config, size)?;

        Ok(Self { download, search, route, map, active_view: Default::default() })
    }

    /// Get a mutable iterator over all views.
    pub fn views_mut(&mut self) -> [&mut dyn UiView; 4] {
        [&mut self.map, &mut self.route, &mut self.search, &mut self.download]
    }

    /// Update the active view.
    pub fn set_view(&mut self, view: View) {
        self.active_view = view;
    }

    /// Get mutable access to the download view.
    pub fn download(&mut self) -> &mut DownloadView {
        &mut self.download
    }

    /// Get mutable access to the search view.
    pub fn search(&mut self) -> &mut SearchView {
        &mut self.search
    }

    /// Get mutable access to the route view.
    pub fn route(&mut self) -> &mut RouteView {
        &mut self.route
    }

    /// Get mutable access to the map view.
    pub fn map(&mut self) -> &mut MapView {
        &mut self.map
    }

    /// Get the active view.
    pub fn active(&self) -> View {
        self.active_view
    }
}

impl Deref for Views {
    type Target = dyn UiView;

    fn deref(&self) -> &Self::Target {
        match self.active_view {
            View::Download => &self.download,
            View::Search => &self.search,
            View::Route => &self.route,
            View::Map => &self.map,
        }
    }
}

impl DerefMut for Views {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self.active_view {
            View::Download => &mut self.download,
            View::Search => &mut self.search,
            View::Route => &mut self.route,
            View::Map => &mut self.map,
        }
    }
}

/// Format a distance targeting 3 visible digits.
pub fn format_distance(w: &mut impl Write, distance: u32) {
    let (unit, divisor) = match distance {
        ..1_000 => ("m", 1.),
        _ => ("km", 1000.),
    };

    let distance = distance as f64 / divisor;
    let precision = 2usize.saturating_sub(distance.log10() as usize);

    let _ = write!(w, "{distance:.precision$} {unit}");
}
