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
    size: SizePx,

    /// Padding around the terminal in physical pixels.
    padding_px: u32,
}

impl ViewGeometry {
    pub fn from_terminal_geometry(
        terminal_geometry: &TerminalGeometry,
        scale_factor: f64,
        padding_px: u32,
    ) -> Self {
        let terminal_size = terminal_geometry.size_px();
        let size_px = terminal_size + SizePx::new(padding_px * 2, padding_px * 2);

        Self {
            _scale_factor: scale_factor,
            size: size_px,
            padding_px,
        }
    }

    pub fn inner_size_px(&self) -> SizePx {
        self.size
    }

    /// Returns the terminal size in pixel.
    pub fn resize(&mut self, size_px: SizePx) -> SizePx {
        let padding_2 = self.padding_px * 2;
        let terminal_inner_size = size_px.saturating_sub((padding_2, padding_2).into());
        self.size = size_px;
        terminal_inner_size
    }
}
