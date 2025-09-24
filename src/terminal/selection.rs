use std::{cmp::Ordering, ops::Range};

use derive_more::Deref;
use log::error;
use wezterm_term::StableRowIndex;

use crate::{
    range_ops::RangeOps,
    window_geometry::{CellPoint, PixelPoint},
};

#[derive(Debug, Default)]
pub enum Selection {
    #[default]
    Unselected,
    Begun {
        pos: SelectionPos,
    },
    // We store the ending position as pixel point, because the selection might change when the view
    // is scrolled, but the starting point always needs to point on the cell originally selected.
    Selecting {
        from: SelectionPos,
        to: PixelPoint,
    },
    Selected {
        from: SelectionPos,
        to: SelectionPos,
    },
}

impl Selection {
    pub fn begin(&mut self, pos: SelectionPos) {
        *self = Self::Begun { pos }
    }

    pub fn can_progress(&self) -> bool {
        matches!(self, Self::Begun { .. } | Self::Selecting { .. })
    }

    pub fn progress(&mut self, end: PixelPoint) {
        *self = match &self {
            Self::Begun { pos } => Self::Selecting {
                from: *pos,
                to: end,
            },
            Self::Selecting { from: start, .. } => Self::Selecting {
                from: *start,
                to: end,
            },
            _ => {
                error!(
                    "Internal error: Selection is progressing, but state is {:?}",
                    self
                );
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

    pub fn end(&mut self, to: SelectionPos) {
        *self = match &self {
            Self::Begun { .. } => Self::Unselected,
            Self::Selecting { from, .. } => Self::Selected { from: *from, to },
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

impl SelectionPos {
    pub fn new(column: usize, row: StableRowIndex) -> Self {
        Self { column, row }
    }

    pub fn point(&self) -> CellPoint {
        assert!(self.row >= 0);
        (self.column, self.row as usize).into()
    }
}

/// Selection range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionRange {
    pub start: SelectionPos,
    pub end: SelectionPos,
}

impl SelectionRange {
    pub fn new(start: SelectionPos, end: SelectionPos) -> Self {
        Self { start, end }
    }

    pub fn normalized(&self) -> NormalizedSelectionRange {
        if self.end >= self.start {
            NormalizedSelectionRange(*self)
        } else {
            NormalizedSelectionRange(Self {
                start: self.end,
                end: self.start,
            })
        }
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
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Deref)]
pub struct NormalizedSelectionRange(SelectionRange);

impl NormalizedSelectionRange {
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
        Some(Self(SelectionRange { start, end }))
    }
}
