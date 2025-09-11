use derive_more::Deref;

use crate::geometry::WindowGeometry;

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
