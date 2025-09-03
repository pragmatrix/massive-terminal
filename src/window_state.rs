use derive_more::Deref;
use winit::event::Modifiers;

use crate::geometry::WindowGeometry;

#[derive(Debug, Deref)]
pub struct WindowState {
    #[deref]
    pub geometry: WindowGeometry,
    pub focused: bool,
    pub keyboard_modifiers: Modifiers,
}

impl WindowState {
    pub fn new(geometry: WindowGeometry) -> Self {
        Self {
            geometry,
            focused: false,
            keyboard_modifiers: Modifiers::default(),
        }
    }
}
