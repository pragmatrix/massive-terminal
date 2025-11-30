use derive_more::Deref;

use crate::view_geometry::ViewGeometry;

#[derive(Debug, Deref)]
pub struct ViewState {
    #[deref]
    pub geometry: ViewGeometry,
    pub focused: bool,
}

impl ViewState {
    pub fn new(geometry: ViewGeometry) -> Self {
        Self {
            geometry,
            focused: false,
        }
    }
}
