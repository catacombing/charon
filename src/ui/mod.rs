use std::time::Instant;

use skia_safe::{Color4f, Paint, Rect};

use crate::config::Input;
use crate::geometry::{Point, Size, rect_contains};
use crate::ui::skia::{RenderState, Svg};
pub use crate::ui::text_field::TextField;

pub mod renderer;
pub mod skia;
mod text_field;
pub mod view;
pub mod window;

/// Percentage of the button size reserved as padding.
const BUTTON_PADDING: f64 = 0.1;

/// Velocity state.
#[derive(Default)]
pub struct Velocity {
    last_tick: Option<Instant>,
    velocity: Point<f64>,
}

impl Velocity {
    /// Check if there is any velocity active.
    pub fn is_moving(&self) -> bool {
        self.velocity != Point::default()
    }

    /// Set the velocity.
    pub fn set(&mut self, velocity: Point<f64>) {
        self.velocity = velocity;
        self.last_tick = None;
    }

    /// Reset all velocity.
    pub fn stop(&mut self) {
        self.velocity = Point::default();
    }

    /// Apply and update the current velocity.
    pub fn apply(&mut self, input: &Input) -> Option<Point<f64>> {
        // No-op without velocity.
        if self.velocity == Point::default() {
            return None;
        }

        // Initialize velocity on the first tick.
        //
        // This avoids applying velocity while the user is still interacting.
        let last_tick = match self.last_tick.take() {
            Some(last_tick) => last_tick,
            None => {
                self.last_tick = Some(Instant::now());
                return None;
            },
        };

        // Calculate velocity steps since last tick.
        let now = Instant::now();
        let interval =
            (now - last_tick).as_micros() as f64 / (input.velocity_interval as f64 * 1_000.);

        // Update velocity and calculate the expected delta.
        let apply = |velocity: &mut f64| {
            let delta = *velocity * (1. - input.velocity_friction.powf(interval + 1.))
                / (1. - input.velocity_friction);
            *velocity *= input.velocity_friction.powf(interval);
            delta
        };
        let x = apply(&mut self.velocity.x);
        let y = apply(&mut self.velocity.y);

        // Request next tick if velocity is significant.
        if self.velocity.x.abs() > 1. || self.velocity.y.abs() > 1. {
            self.last_tick = Some(now);
        } else {
            self.velocity = Point::default();
        }

        Some(Point::new(x, y))
    }
}

/// An SVG button.
struct Button {
    paint: Paint,
    point: Point,
    size: Size,
    svg: Svg,
}

impl Button {
    fn new(point: Point, size: Size, svg: Svg) -> Self {
        let paint = Paint::default();
        Self { paint, point, size, svg }
    }

    /// Render the button.
    #[cfg_attr(feature = "profiling", profiling::function)]
    fn draw(&mut self, render_state: &mut RenderState, background: impl Into<Color4f>) {
        self.paint.set_color4f(background.into(), None);

        let right = self.point.x as f32 + self.size.width as f32;
        let bottom = self.point.y as f32 + self.size.height as f32;
        let rect = Rect::new(self.point.x as f32, self.point.y as f32, right, bottom);
        render_state.draw_rect(rect, &self.paint);

        let padding = self.size * BUTTON_PADDING;
        let svg_size = self.size - Size::new(padding.width * 2, padding.height * 2);
        let x = self.point.x + padding.width as i32;
        let y = self.point.y + padding.height as i32;
        render_state.draw_svg(self.svg, Point::new(x, y), svg_size);
    }

    /// Update the button's position.
    pub fn set_point(&mut self, point: Point) {
        self.point = point;
    }

    /// Update the button's size.
    pub fn set_size(&mut self, size: Size) {
        self.size = size;
    }

    /// Change the button's SVG.
    pub fn set_svg(&mut self, svg: Svg) {
        self.svg = svg;
    }

    /// Check if a point lies within this button.
    pub fn contains(&self, point: Point<f64>) -> bool {
        let point = Point::new(point.x.round() as i32, point.y.round() as i32);
        rect_contains(self.point, self.size.into(), point)
    }
}
