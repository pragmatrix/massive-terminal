use std::{cmp::Ordering, ops::Range};

use log::{error, warn};
use wezterm_term::{DoubleClickRange, StableRowIndex, Terminal};

use crate::{
    range_ops::{RangeOps, WithLength},
    terminal::{CellPos, get_logical_lines},
    window_geometry::{CellPoint, PixelPoint},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionMode {
    Cell,
    Word,
}

#[derive(Debug, Default)]
pub enum Selection {
    #[default]
    Unselected,
    Begun {
        mode: SelectionMode,
        pos: SelectionPos,
        // Architecture: nest Selecting and Selected under Begun and rename Begun to Active, this
        // way we won't be duplicated mode and from?
    },
    // We store the ending position as pixel point, because the selection might change when the view
    // is scrolled, but the starting point always needs to point on the cell originally selected.
    Selecting {
        mode: SelectionMode,
        from: SelectionPos,
        to: PixelPoint,
    },
    Selected {
        mode: SelectionMode,
        from: SelectionPos,
        to: SelectionPos,
    },
}

impl Selection {
    pub fn begin(&mut self, mode: SelectionMode, pos: SelectionPos) {
        *self = Self::Begun { mode, pos }
    }

    pub fn can_progress(&self) -> bool {
        matches!(self, Self::Begun { .. } | Self::Selecting { .. })
    }

    #[must_use]
    pub fn progress(&mut self, end: PixelPoint) -> bool {
        *self = match &self {
            Self::Begun { mode, pos } => Self::Selecting {
                mode: *mode,
                from: *pos,
                to: end,
            },
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

        self.can_progress()
    }

    /// Ends the selection and returns the pixel point the cursor was last at.
    #[must_use]
    pub fn selecting_end(&self) -> Option<PixelPoint> {
        match self {
            Self::Selecting { to, .. } => Some(*to),
            _ => None,
        }
    }

    pub fn end(&mut self, to: SelectionPos) {
        *self = match &self {
            Self::Begun { .. } => Self::Unselected,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionPos {
    column: usize,
    row: StableRowIndex,
}

impl PartialOrd for SelectionPos {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some((self.row, self.column).cmp(&(other.row, other.column)))
    }
}

impl From<CellPos> for SelectionPos {
    fn from(value: CellPos) -> Self {
        SelectionPos::new(value.column.max(0).cast_unsigned(), value.stable_row)
    }
}

impl SelectionPos {
    pub fn new(column: usize, row: StableRowIndex) -> Self {
        Self { column, row }
    }

    pub fn point(&self) -> CellPoint {
        assert!(self.row >= 0);
        (self.column, self.row as usize).into()
    }
}

/// Selection range. Always normalized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectedRange {
    start: SelectionPos,
    end: SelectionPos,
}

impl SelectedRange {
    pub fn new(a: SelectionPos, b: SelectionPos) -> Self {
        if b >= a {
            Self { start: a, end: b }
        } else {
            Self { start: b, end: a }
        }
    }

    pub fn start(&self) -> &SelectionPos {
        &self.start
    }

    pub fn end(&self) -> &SelectionPos {
        &self.end
    }

    pub fn stable_rows(&self) -> Range<StableRowIndex> {
        self.start.row..self.end.row + 1
    }

    /// Yields a range representing the selected columns for the specified row.
    ///
    /// The range may include usize::max_value() for some rows; this indicates that the selection
    /// extends to the end of that row. Since this struct has no knowledge of line length, it cannot
    /// be more precise than that.
    pub fn cols_for_row(&self, row: StableRowIndex, rectangular: bool) -> Range<usize> {
        match () {
            _ if rectangular => {
                if row < self.start.row || row > self.end.row {
                    0..0
                } else {
                    Self::column_range(self.start.column, self.end.column)
                }
            }
            _ if row < self.start.row || row > self.end.row => 0..0,
            _ if self.start.row == self.end.row => {
                Self::column_range(self.start.column, self.end.column)
            }
            _ if row == self.end.row => 0..self.end.column + 1,
            _ if row == self.start.row => self.start.column..usize::MAX,
            _ => 0..usize::MAX,
        }
    }

    fn column_range(from: usize, to: usize) -> Range<usize> {
        if to >= from {
            from..to + 1
        } else {
            to..from + 1
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
            end.column = columns - 1;
        }
        if start.row == end.row && start.column > end.column {
            return None;
        }
        Some(SelectedRange::new(start, end))
    }
}

// Copied from wezterm-gui/src/selection.rs

/// Computes the selection range for the word around the specified coords
pub fn word_around(terminal: &Terminal, start: SelectionPos) -> Option<SelectedRange> {
    for logical in get_logical_lines(terminal, start.row.with_len(1)) {
        if !logical.contains_y(start.row) {
            continue;
        }

        let start_idx = logical.xy_to_logical_x(start.column, start.row);
        return match logical
            .logical
            .compute_double_click_range(start_idx, is_double_click_word)
        {
            DoubleClickRange::RangeWithWrap(click_range) | DoubleClickRange::Range(click_range) => {
                let (start_y, start_x) = logical.logical_x_to_physical_coord(click_range.start);
                let (end_y, end_x) = logical.logical_x_to_physical_coord(click_range.end - 1);

                Some(SelectedRange::new(
                    SelectionPos::new(start_x, start_y),
                    SelectionPos::new(end_x, end_y),
                ))
            }
        };
    }

    error!("word_around: logical line does not contain stable row.");
    None
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
