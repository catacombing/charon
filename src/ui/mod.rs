pub mod renderer;
pub mod skia;
pub mod window;

use std::time::Instant;

use crate::config::Input;
use crate::geometry::Point;

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
