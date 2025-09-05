use std::collections::HashMap;

use derive_more::Deref;
use winit::{
    dpi::PhysicalPosition,
    event::{DeviceId, Modifiers},
};

use crate::geometry::WindowGeometry;

#[derive(Debug, Deref)]
pub struct WindowState {
    #[deref]
    pub geometry: WindowGeometry,
    pub focused: bool,
    pub keyboard_modifiers: Modifiers,
    cursor_states: HashMap<DeviceId, CursorState>,
}

impl WindowState {
    pub fn new(geometry: WindowGeometry) -> Self {
        Self {
            geometry,
            focused: false,
            keyboard_modifiers: Modifiers::default(),
            cursor_states: Default::default(),
        }
    }

    // Naming: cursor is overloaded with the terminal's cursor (use pointer ?).
    pub fn cursor_state(&self, device_id: DeviceId) -> CursorState {
        self.cursor_states
            .get(&device_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn cursor_entered(&mut self, device_id: DeviceId) {
        self.cursor_states.entry(device_id).or_default().entered = true;
    }

    pub fn cursor_left(&mut self, device_id: DeviceId) {
        self.cursor_states.entry(device_id).or_default().entered = false;
    }

    pub fn cursor_moved(&mut self, device_id: DeviceId, position: PhysicalPosition<f64>) {
        self.cursor_states.entry(device_id).or_default().pos_px = Some((position.x, position.y));
    }
}

#[derive(Debug, Clone, Default)]
pub struct CursorState {
    /// If one of the cursor's button (say it's a mouse) is pressed, entered / exited works like
    /// before, but pos gets updated outside the window, too.
    pub entered: bool,
    pub pos_px: Option<(f64, f64)>,
}
