#![allow(unused)]
use std::ops::Range;

use wezterm_term::StableRowIndex;

use crate::{
    range_ops::WithLength,
    terminal::{NormalizedSelectionRange, Selection, SelectionRange, TerminalGeometry},
    window_geometry::PixelPoint,
};

#[derive(Debug, PartialEq, Eq)]
pub struct ViewGeometry {
    pub terminal: TerminalGeometry,

    /// The number of pixels the first stable row shoots over the top of the view.
    pub stable_range_ascend_px: u32,

    /// The range of all the stable lines that are visible inside the view (may be partly because of
    /// scrolling).
    ///
    /// Detail: Indices might start negative. if the view is scrolled up above the terminal's top position.
    pub stable_range: Range<StableRowIndex>,
}

impl ViewGeometry {
    /// The vertical pixel span all the lines are covering, top might be negative.
    pub fn lines_vertical_pixel_span(&self) -> Range<i32> {
        (-(self.stable_range_ascend_px as i32))
            .with_len(self.stable_range.len() as u32 * self.line_height_px())
    }

    /// The view's vertical pixel span (not including the overshoots).
    ///
    /// Always starts at 0.
    pub fn vertical_pixel_span(&self) -> Range<u32> {
        0..self.terminal.rows() as u32 * self.line_height_px()
    }

    pub fn line_height_px(&self) -> u32 {
        self.terminal.line_height_px()
    }

    pub fn terminal_size(&self) -> (usize, usize) {
        self.terminal.terminal_size
    }

    /// Computes the normalized selection range.
    pub fn selection_range(&self, selection: &Selection) -> Option<NormalizedSelectionRange> {
        match *selection {
            Selection::Unselected => None,
            Selection::Begun { .. } => None,
            Selection::Selecting { from, to } => {
                let to = self.hit_test_cell(to).into();
                Some(SelectionRange::new(from, to).normalized())
            }
            Selection::Selected { from, to } => Some(SelectionRange::new(from, to).normalized()),
        }
    }

    /// Hit tests a pixel point on the view resulting in a column and a row.
    pub fn hit_test_cell(&self, view_px: PixelPoint) -> CellHit {
        let (x, mut y) = view_px.into();

        let column = (x / self.terminal.cell_size_px.0 as f64).floor() as isize;

        y -= self.stable_range_ascend_px as f64;
        let row = (y / self.terminal.cell_size_px.1 as f64).floor() as isize;

        CellHit {
            column,
            row: row + self.stable_range.start,
        }
    }
}

/// A hit on a cell. Both values might be out of their valid range.
#[derive(Debug)]
pub struct CellHit {
    pub column: isize,
    pub row: StableRowIndex,
}
