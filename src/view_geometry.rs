use crate::terminal::TerminalGeometry;

// euclid definitions

pub struct CellUnit;

pub type CellRect = euclid::Rect<usize, CellUnit>;
#[allow(unused)]
pub type CellPoint = euclid::Point2D<usize, CellUnit>;

pub struct PixelUnit;

/// A point on a pixel coordinate system expressed in floats.
pub type PixelPoint = euclid::Point2D<f64, PixelUnit>;

#[derive(Debug)]
pub struct ViewGeometry {
    _scale_factor: f64,
    inner_size_px: (u32, u32),

    /// Padding around the terminal in physical pixels.
    padding_px: u32,
}

impl ViewGeometry {
    pub fn from_terminal_geometry(
        terminal_geometry: &TerminalGeometry,
        scale_factor: f64,
        padding_px: u32,
    ) -> Self {
        let (width, height) = terminal_geometry.size_px();
        let padding_2 = padding_px * 2;
        let inner_size_px = (width + padding_2, height + padding_2);

        Self {
            _scale_factor: scale_factor,
            inner_size_px,
            padding_px,
        }
    }

    pub fn inner_size_px(&self) -> (u32, u32) {
        self.inner_size_px
    }

    /// Returns the terminal inner size in pixel.
    pub fn resize(&mut self, new_inner_size_px: (u32, u32)) -> (u32, u32) {
        let padding_2 = self.padding_px * 2;
        let terminal_inner_size = (
            new_inner_size_px.0.saturating_sub(padding_2),
            new_inner_size_px.1.saturating_sub(padding_2),
        );
        self.inner_size_px = new_inner_size_px;
        terminal_inner_size
    }
}
