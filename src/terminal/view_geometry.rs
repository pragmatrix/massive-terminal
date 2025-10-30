#![allow(unused)]
use std::ops::Range;

use wezterm_term::{Cell, Screen, StableRowIndex, Terminal};

use crate::{
    range_ops::WithLength,
    terminal::{ScreenGeometry, SelectedRange, Selection, SelectionMode, TerminalGeometry},
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

    /// Returns the currently selected user range.
    ///
    /// User range: _not_ extended by word / line boundaries, that area that was actually selected
    /// using the mouse coordinates directly.
    pub fn selected_user_range(&self, selection: &Selection) -> Option<SelectedRange> {
        match *selection {
            Selection::Unselected => None,
            Selection::Selecting { mode, from, to, .. } => {
                let to = self.hit_test_cell(to).into();
                Some(SelectedRange::new(from, to))
            }
            Selection::Selected { mode, from, to, .. } => Some(SelectedRange::new(from, to)),
        }
    }

    /// Hit tests a pixel point on the view resulting in a column and a row.
    pub fn hit_test_cell(&self, view_px: PixelPoint) -> CellPos {
        let (x, mut y) = view_px.into();

        let column = (x / self.terminal.cell_size_px.0 as f64).floor() as isize;

        y -= self.stable_range_ascend_px as f64;
        let row = (y / self.terminal.cell_size_px.1 as f64).floor() as isize;

        CellPos {
            column,
            stable_row: row + self.stable_range.start,
        }
    }

    pub fn get_cell<'s>(&self, cell: CellPos, screen: &'s mut Screen) -> Option<&'s Cell> {
        let visible_start = screen.visible_row_to_stable_row(0);
        // Visible on our view.
        if self.stable_range.contains(&cell.stable_row) && cell.column >= 0 {
            let visible_y = cell.stable_row - visible_start;
            return screen
                // Correctness: Does this actually hit on the column, may need to use visible_cells in Line instead?
                .get_cell(cell.column.cast_unsigned(), visible_y as i64);
        }

        None
    }
}

/// A cell position.
///
/// Both values might be outside of the view's visibility or range.
#[derive(Debug, Copy, Clone)]
pub struct CellPos {
    pub column: isize,
    pub stable_row: StableRowIndex,
}
