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
    cursors: HashMap<DeviceId, CursorState>,
}

impl WindowState {
    pub fn new(geometry: WindowGeometry) -> Self {
        Self {
            geometry,
            focused: false,
            keyboard_modifiers: Modifiers::default(),
            cursors: Default::default(),
        }
    }

    pub fn cursor_entered(&mut self, device_id: DeviceId) {
        self.cursors.entry(device_id).or_default().entered = true;
    }

    pub fn cursor_left(&mut self, device_id: DeviceId) {
        self.cursors.entry(device_id).or_default().entered = false;
    }

    pub fn cursor_moved(&mut self, device_id: DeviceId, position: PhysicalPosition<f64>) {
        self.cursors.entry(device_id).or_default().pos_px = Some((position.x, position.y));
    }
}

#[derive(Debug, Default)]
struct CursorState {
    /// If one of the cursor's button (say it's a mouse) is pressed, entered / exited works like
    /// before, but pos gets updated outside the window, too.
    entered: bool,
    pos_px: Option<(f64, f64)>,
}
