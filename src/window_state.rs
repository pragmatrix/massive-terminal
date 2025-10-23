use derive_more::Deref;
use winit::window::CursorIcon;

use crate::window_geometry::WindowGeometry;

#[derive(Debug, Deref)]
pub struct WindowState {
    #[deref]
    pub geometry: WindowGeometry,
    pub focused: bool,
}

impl WindowState {
    pub fn new(geometry: WindowGeometry) -> Self {
        Self {
            geometry,
            focused: false,
        }
    }
}
