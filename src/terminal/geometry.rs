use std::ops::Range;

use portable_pty::PtySize;
use wezterm_term::StableRowIndex;

use massive_geometry::{SizePx, prelude::*};

use crate::view_geometry::PixelPoint;

pub struct CellUnit;

pub type SizeCell = euclid::Size2D<usize, CellUnit>;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct TerminalGeometry {
    /// Cell size in physical pixels.
    pub cell_size_px: SizePx,

    /// Terminal size in cells (columns, rows).
    pub terminal_size: SizeCell,
}

impl TerminalGeometry {
    pub fn new(cell_size: SizePx, terminal_cells: SizeCell) -> Self {
        Self {
            cell_size_px: cell_size,
            terminal_size: terminal_cells,
        }
    }

    pub fn resize_px(&mut self, terminal_inner_size: SizePx) {
        let terminal_cells = terminal_inner_size.component_div(self.cell_size_px);
        let terminal_cells = terminal_cells.max((1, 1).into());
        self.terminal_size = terminal_cells;
    }

    pub fn columns(&self) -> usize {
        self.terminal_size.width
    }

    pub fn rows(&self) -> usize {
        self.terminal_size.height
    }

    /// Given a stable range of all the content and a pixel offset, clip the offset so that the
    /// terminal's view does not move the view into the terminal out of range.
    pub fn clamp_px_offset(&self, content_stable_range: Range<StableRowIndex>, px: f64) -> f64 {
        let min = self.stable_px_offset(content_stable_range.start);
        let max = self.stable_px_offset(content_stable_range.end - self.rows().cast_signed());
        assert!(max >= min);
        px.clamp(min as f64, max as f64)
    }

    /// Returns the unclamped px offset of a stable row index.
    pub fn stable_px_offset(&self, stable: StableRowIndex) -> i64 {
        stable as i64 * self.line_height_px() as i64
    }

    pub fn line_height_px(&self) -> u32 {
        self.cell_size_px.height
    }

    pub fn size_px(&self) -> SizePx {
        self.cell_size_px.component_mul(self.terminal_size)
    }

    pub fn pty_size(&self) -> PtySize {
        PtySize {
            rows: self.rows() as _,
            cols: self.columns() as _,
            // Robustness: is this physical or logical size, and what does a terminal actually do with it?
            pixel_width: self.cell_size_px.width as _,
            pixel_height: self.cell_size_px.height as _,
        }
    }

    pub fn wezterm_terminal_size(&self) -> wezterm_term::TerminalSize {
        wezterm_term::TerminalSize {
            rows: self.rows(),
            cols: self.columns(),
            pixel_width: self.cell_size_px.width as usize,
            pixel_height: self.cell_size_px.height as usize,
            // Production: Set dpi
            ..wezterm_term::TerminalSize::default()
        }
    }

    /// Decide if scrolling is needed and how many pixels the hit position lies away from.
    ///
    /// Negative values scroll up, positives, scroll down.
    pub fn scroll_distance_px(&self, view_hit: PixelPoint) -> Option<f64> {
        let hit_y = view_hit.y;

        if hit_y < 0.0 {
            return Some(view_hit.y);
        }

        let height = self.size_px().height as f64;
        if hit_y > height {
            return Some(hit_y - height);
        }

        None
    }
}
