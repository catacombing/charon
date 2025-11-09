pub mod renderer;
pub mod skia;
pub mod window;

use std::time::Instant;

use crate::config::Input;

/// Scroll velocity state.
#[derive(Default)]
pub struct ScrollVelocity {
    last_tick: Option<Instant>,
    velocity: f64,
}

impl ScrollVelocity {
    /// Check if there is any velocity active.
    pub fn is_moving(&self) -> bool {
        self.velocity != 0.
    }

    /// Set the velocity.
    pub fn set(&mut self, velocity: f64) {
        self.velocity = velocity;
        self.last_tick = None;
    }

    /// Apply and update the current scroll velocity.
    pub fn apply(&mut self, input: &Input, scroll_offset: &mut f64) {
        // No-op without velocity.
        if self.velocity == 0. {
            return;
        }

        // Initialize velocity on the first tick.
        //
        // This avoids applying velocity while the user is still actively scrolling.
        let last_tick = match self.last_tick.take() {
            Some(last_tick) => last_tick,
            None => {
                self.last_tick = Some(Instant::now());
                return;
            },
        };

        // Calculate velocity steps since last tick.
        let now = Instant::now();
        let interval =
            (now - last_tick).as_micros() as f64 / (input.velocity_interval as f64 * 1_000.);

        // Apply and update velocity.
        *scroll_offset += self.velocity * (1. - input.velocity_friction.powf(interval + 1.))
            / (1. - input.velocity_friction);
        self.velocity *= input.velocity_friction.powf(interval);

        // Request next tick if velocity is significant.
        if self.velocity.abs() > 1. {
            self.last_tick = Some(now);
        } else {
            self.velocity = 0.
        }
    }
}
