use std::ops::Range;

use log::{error, warn};
use wezterm_term::{DoubleClickRange, StableRowIndex, Terminal};

use crate::{
    range_ops::{RangeOps, WithLength},
    terminal::{CellPos, LogicalLine, get_logical_lines},
    view_geometry::PixelPoint,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionMode {
    Cell,
    Word,
    Line,
}

#[derive(Debug, Default, PartialEq)]
pub enum Selection {
    #[default]
    Unselected,
    // We store the ending position as pixel point, because the selection might change when the view
    // is scrolled, but the starting point always needs to point on the cell originally selected.
    Selecting {
        mode: SelectionMode,
        from: CellPos,
        to: PixelPoint,
    },
    Selected {
        mode: SelectionMode,
        from: CellPos,
        to: CellPos,
    },
}

impl Selection {
    pub fn mode(&self) -> Option<SelectionMode> {
        match *self {
            Self::Unselected => None,
            Self::Selecting { mode, .. } => Some(mode),
            Self::Selected { mode, .. } => Some(mode),
        }
    }

    pub fn begin(&mut self, mode: SelectionMode, hit: PixelPoint, pos: CellPos) {
        *self = Self::Selecting {
            mode,
            from: pos,
            to: hit,
        }
    }

    pub fn can_progress(&self) -> bool {
        matches!(self, Self::Selecting { .. })
    }

    pub fn progress(&mut self, end: PixelPoint) {
        *self = match &self {
            Self::Selecting {
                mode, from: start, ..
            } => Self::Selecting {
                mode: *mode,
                from: *start,
                to: end,
            },
            _ => {
                // This happens when the selection is cleared, but clients continue to progress.
                warn!("Selection is progressing, but state is {:?}", self);
                Self::Unselected
            }
        };
    }

    /// Ends the selection and returns the pixel point the cursor was last at.
    #[must_use]
    pub fn selecting_end(&self) -> Option<PixelPoint> {
        match self {
            Self::Selecting { to, .. } => Some(*to),
            _ => None,
        }
    }

    pub fn end(&mut self, to: CellPos) {
        *self = match &self {
            Self::Selecting { mode, from, .. } => Self::Selected {
                mode: *mode,
                from: *from,
                to,
            },
            _ => {
                error!(
                    "Internal error: Selection is ending, but state is {:?}",
                    self
                );
                Self::Unselected
            }
        }
    }

    pub fn reset(&mut self) {
        *self = Self::Unselected;
    }
}

/// Selection range.
///
/// Always end >= start and as a closed interval in both directions (this way it's never empty).
///
/// `start` and `end` may be completely out of range and represent the intent of user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectedRange {
    start: CellPos,
    end: CellPos,
}

impl From<CellPos> for SelectedRange {
    fn from(pos: CellPos) -> Self {
        SelectedRange::new(pos, pos)
    }
}

impl SelectedRange {
    pub fn boundary(a: Self, b: Self) -> Self {
        let start = a.start.min(b.start);
        let end = a.end.max(b.end);
        Self { start, end }
    }

    pub fn new(a: CellPos, b: CellPos) -> Self {
        if b >= a {
            Self { start: a, end: b }
        } else {
            Self { start: b, end: a }
        }
    }

    pub fn extend(self, mode: SelectionMode, terminal: &Terminal) -> Self {
        match mode {
            SelectionMode::Cell => {
                let range_a = cell_around(self.start, terminal);
                let range_b = cell_around(self.end, terminal);
                Self::boundary(range_a, range_b)
            }
            SelectionMode::Word => {
                let range_a = word_around(self.start, terminal);
                let range_b = word_around(self.end, terminal);
                Self::boundary(range_a, range_b)
            }
            SelectionMode::Line => {
                let range_a = line_around(self.start, terminal);
                let range_b = line_around(self.end, terminal);
                Self::boundary(range_a, range_b)
            }
        }
    }

    pub fn start(&self) -> &CellPos {
        &self.start
    }

    pub fn end(&self) -> &CellPos {
        &self.end
    }

    pub fn stable_rows(&self) -> Range<StableRowIndex> {
        self.start.row..self.end.row.saturating_add(1)
    }

    /// Yields a range representing the selected columns for the specified row.
    ///
    /// The range may include isize::MAX for some rows; this indicates that the selection extends to
    /// the end of that row.
    ///
    /// Since this struct has no knowledge of line length, it cannot be more precise than that.
    ///
    /// Architecture: This is conceptually similar to the computation of the actually visible Cell
    /// rectangles of the selection.
    pub fn cols_for_row(&self, row: StableRowIndex, rectangular: bool) -> Range<usize> {
        match () {
            _ if rectangular => {
                if !self.stable_rows().contains(&row) {
                    0..0
                } else {
                    Self::column_range(self.start.column, self.end.column)
                }
            }
            _ if !self.stable_rows().contains(&row) => 0..0,
            _ if self.start.row == self.end.row => {
                Self::column_range(self.start.column, self.end.column)
            }
            _ if row == self.end.row => Self::column_range(0, self.end.column),
            _ if row == self.start.row => Self::column_range(self.start.column, isize::MAX),
            _ => 0..usize::MAX,
        }
    }

    fn column_range(from: isize, to: isize) -> Range<usize> {
        // Saturating add because input ranges might contain isize::MAX (because of line selection)
        let signed_range = if to >= from {
            from..to.saturating_add(1)
        } else {
            to..from.saturating_add(1)
        };

        // Convert to usize ranges.

        Range {
            start: signed_range.start.max(0) as usize,
            end: signed_range.end.max(0) as usize,
        }
    }

    pub fn clamp_to_rows(self, rows: Range<StableRowIndex>, columns: usize) -> Option<Self> {
        if !self.stable_rows().intersects(&rows) {
            return None;
        }

        let mut start = self.start;
        let mut end = self.end;
        if rows.start > start.row {
            start.row = rows.start;
            start.column = 0;
        }
        if rows.end <= end.row {
            end.row = rows.end - 1;
            end.column = columns.cast_signed() - 1;
        }
        if start.row == end.row && start.column > end.column {
            return None;
        }
        Some(Self::new(start, end))
    }
}

pub fn cell_around(pos: CellPos, terminal: &Terminal) -> SelectedRange {
    // Performance: I am not sure if going through the logical line is needed just to find out if
    // the cell at pos or one before is a double-width cell.
    for logical in get_logical_lines(terminal, pos.row.with_len(1)) {
        if !logical.contains_y(pos.row) {
            continue;
        }

        let start_idx = logical.xy_to_logical_x(pos.column.max(0).cast_unsigned(), pos.row);

        for cell in logical.logical.visible_cells() {
            let click_range = cell.cell_index().with_len(cell.width());
            if click_range.contains(&start_idx) {
                return click_range_to_selected_range(&logical, click_range);
            }
        }
    }

    pos.into()
}

// Mostly copied from wezterm-gui/src/selection.rs

/// Computes the selection range for the word around the specified coords
pub fn word_around(pos: CellPos, terminal: &Terminal) -> SelectedRange {
    for logical in get_logical_lines(terminal, pos.row.with_len(1)) {
        if !logical.contains_y(pos.row) {
            continue;
        }

        let start_idx = logical.xy_to_logical_x(pos.column.max(0).cast_unsigned(), pos.row);
        return match logical
            .logical
            .compute_double_click_range(start_idx, is_double_click_word)
        {
            DoubleClickRange::RangeWithWrap(click_range) | DoubleClickRange::Range(click_range) => {
                click_range_to_selected_range(&logical, click_range)
            }
        };
    }

    pos.into()
}

fn click_range_to_selected_range(
    logical: &LogicalLine,
    click_range: Range<usize>,
) -> SelectedRange {
    let (start_y, start_x) = logical.logical_x_to_physical_coord(click_range.start);
    // Detail: Click_ranges are half-open, but we need to return closed ranges.
    let (end_y, end_x) = if !click_range.is_empty() {
        logical.logical_x_to_physical_coord(click_range.end - 1)
    } else {
        (start_y, start_x)
    };

    SelectedRange::new(
        CellPos::new(start_x.cast_signed(), start_y),
        CellPos::new(end_x.cast_signed(), end_y),
    )
}

/// Computes the selection range for the line around the specified coords
pub fn line_around(pos: CellPos, terminal: &Terminal) -> SelectedRange {
    for logical in get_logical_lines(terminal, pos.row.with_len(1)) {
        if logical.contains_y(pos.row) {
            return SelectedRange::new(
                CellPos::new(0, logical.first_row),
                CellPos::new(
                    isize::MAX,
                    logical.first_row + (logical.physical_lines.len() - 1) as StableRowIndex,
                ),
            );
        }
    }

    pos.into()
}

fn is_double_click_word(s: &str) -> bool {
    match s.chars().count() {
        1 => !DEFAULT_WORD_BOUNDARY.contains(s),
        0 => false,
        _ => true,
    }
}

// Feature: Make this configurable
// Precision: Use the help of `unicode_segmentation`?
const DEFAULT_WORD_BOUNDARY: &str = " \t\n{[}]()\"'`";
