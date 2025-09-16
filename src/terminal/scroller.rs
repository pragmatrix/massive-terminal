//! Scrolling control for terminals.
use std::time::Duration;

use massive_animation::{Interpolation, Timeline};
use massive_shell::Scene;

#[allow(unused)]
#[derive(Debug)]
pub struct TerminalScroller {
    /// The duration until we reach the final scrolling speed.
    phase_in_duration: Duration,
    /// The duration until we reach the resting point.
    phase_out_duration: Duration,

    /// The current velocity, normalized 0..1
    velocity: Timeline<f64>,

    /// The current scroll offset, either manually updated by the current velocity or animated.
    scroll_offset: Timeline<f64>,

    state: ScrollAnimationState,
}

#[allow(unused)]
#[derive(Debug)]
enum ScrollAnimationState {
    NoScrolling,
    PhasingInOrScrolling,
    PhasingOut,
}

#[allow(unused)]
impl TerminalScroller {
    pub fn new(scene: &Scene, phase_in_duration: Duration, phase_out_duration: Duration) -> Self {
        let velocity = scene.timeline(0.0);
        let scroll_offset = scene.timeline(0.0);

        Self {
            phase_in_duration,
            phase_out_duration,
            velocity,
            scroll_offset,
            state: ScrollAnimationState::NoScrolling,
        }
    }

    /// Set a new velocity and start scrolling if not currently so.
    pub fn set_velocity(&mut self, pixels_per_second: f64) {
        self.velocity.animate_to(
            pixels_per_second,
            self.phase_in_duration,
            Interpolation::CubicOut,
        );
        self.state = ScrollAnimationState::PhasingInOrScrolling;
    }

    // Architecture: May embed the transition here in the type system by taking self and return a
    // different type?
    pub fn rest(&mut self, resting_point: f64) {
        // Research: How do we combine these two animations?
        self.velocity
            .animate_to(0.0, self.phase_out_duration, Interpolation::CubicIn);
        self.scroll_offset.animate_to(
            resting_point,
            self.phase_out_duration,
            Interpolation::CubicIn,
        );
        self.state = ScrollAnimationState::PhasingOut;
    }

    /// Proceed the scrolling animation and return the value. Returns `None` if no scrolling is
    /// active and / or the resting point has been returned before.
    pub fn proceed(&mut self) -> Option<f64> {
        match self.state {
            ScrollAnimationState::NoScrolling => None,
            ScrollAnimationState::PhasingInOrScrolling => {
                let velocity = self.velocity.value();
                let new_scroll_offset = self.scroll_offset.value() + velocity;
                self.scroll_offset.animate_to(
                    new_scroll_offset,
                    Duration::ZERO,
                    Interpolation::Linear,
                );
                let effective_scroll_offset = self.scroll_offset.value();
                Some(effective_scroll_offset)
            }
            ScrollAnimationState::PhasingOut => {
                if !self.velocity.is_animating() && !self.scroll_offset.is_animating() {
                    return None;
                }

                // Research: This implementation is wrong, this is somehow a blend of velocity going
                // down and the final scroll offset taking hold.
                let velocity = self.velocity.value();
                let new_scroll_offset = self.scroll_offset.value() + velocity;
                self.scroll_offset.animate_to(
                    new_scroll_offset,
                    Duration::ZERO,
                    Interpolation::Linear,
                );
                let effective_scroll_offset = self.scroll_offset.value();

                Some(effective_scroll_offset)
            }
        }
    }
}
