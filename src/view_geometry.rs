use massive_geometry::{PixelUnit, SizePx, prelude::SaturatingSub};

use crate::terminal::{CellUnit, TerminalGeometry};

// euclid definitions

pub type CellRect = euclid::Rect<usize, CellUnit>;
#[allow(unused)]
pub type CellPoint = euclid::Point2D<usize, CellUnit>;

/// A point on a pixel coordinate system expressed in floats.
pub type PixelPoint = euclid::Point2D<f64, PixelUnit>;

#[derive(Debug)]
pub struct ViewGeometry {
    _scale_factor: f64,
    inner_size_px: SizePx,

    /// Padding around the terminal in physical pixels.
    padding_px: u32,
}

impl ViewGeometry {
    pub fn from_terminal_geometry(
        terminal_geometry: &TerminalGeometry,
        scale_factor: f64,
        padding_px: u32,
    ) -> Self {
        let size = terminal_geometry.size_px();
        let inner_size_px = size + SizePx::new(padding_px * 2, padding_px * 2);

        Self {
            _scale_factor: scale_factor,
            inner_size_px,
            padding_px,
        }
    }

    pub fn inner_size_px(&self) -> SizePx {
        self.inner_size_px
    }

    /// Returns the terminal inner size in pixel.
    pub fn resize(&mut self, new_inner_size_px: SizePx) -> SizePx {
        let padding_2 = self.padding_px * 2;
        let terminal_inner_size = new_inner_size_px.saturating_sub((padding_2, padding_2).into());
        self.inner_size_px = new_inner_size_px;
        terminal_inner_size
    }
}
