use std::{cmp::Ordering, ops::Range};

use derive_more::Deref;
use log::error;
use wezterm_term::StableRowIndex;

use crate::window_geometry::CellPoint;

#[derive(Debug, Default)]
pub struct Selection {
    state: SelectionState,
}

impl Selection {
    pub fn begin(&mut self, pos: SelectionPos) {
        self.state = SelectionState::Begun { pos }
    }

    pub fn can_progress(&self) -> bool {
        matches!(
            self.state,
            SelectionState::Begun { .. } | SelectionState::Selecting { .. }
        )
    }

    pub fn progress(&mut self, ending_pos: SelectionPos) {
        self.state = match &self.state {
            SelectionState::Begun { pos } => {
                SelectionState::Selecting(SelectionRange::new(*pos, ending_pos))
            }
            SelectionState::Selecting(SelectionRange { start, .. }) => {
                SelectionState::Selecting(SelectionRange::new(*start, ending_pos))
            }
            _ => {
                error!(
                    "Internal error: Selection is progressing, but state is {:?}",
                    self.state
                );
                SelectionState::Unselected
            }
        };
    }

    pub fn end(&mut self) {
        self.state = match &self.state {
            SelectionState::Begun { .. } => SelectionState::Unselected,
            SelectionState::Selecting(range) => SelectionState::Selected(range.normalized()),
            _ => {
                error!(
                    "Internal error: Selection is ending, but state is {:?}",
                    self.state
                );
                SelectionState::Unselected
            }
        }
    }

    pub fn reset(&mut self) {
        self.state = SelectionState::Unselected;
    }

    // Normalized selection range
    pub fn range(&self) -> Option<NormalizedSelectionRange> {
        match self.state {
            SelectionState::Selecting(range) => Some(range.normalized()),
            SelectionState::Selected(range) => Some(range),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionPos {
    left: usize,
    top: StableRowIndex,
}

impl PartialOrd for SelectionPos {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some((self.top, self.left).cmp(&(other.top, other.left)))
    }
}

impl SelectionPos {
    pub fn new(left: usize, top: StableRowIndex) -> Self {
        Self { left, top }
    }

    pub fn point(&self) -> CellPoint {
        assert!(self.top >= 0);
        (self.left, self.top as usize).into()
    }
}

#[derive(Debug, Default)]
pub enum SelectionState {
    #[default]
    Unselected,
    Begun {
        pos: SelectionPos,
    },
    Selecting(SelectionRange),
    Selected(NormalizedSelectionRange),
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

    /// Yields a range representing the row indices.
    pub fn rows(&self) -> Range<StableRowIndex> {
        self.start.top..self.end.top + 1
    }

    /// Yields a range representing the selected columns for the specified row.
    ///
    /// The range may include usize::max_value() for some rows; this indicates that the selection
    /// extends to the end of that row. Since this struct has no knowledge of line length, it cannot
    /// be more precise than that.
    pub fn cols_for_row(&self, row: StableRowIndex, rectangular: bool) -> Range<usize> {
        match () {
            _ if rectangular => {
                if row < self.start.top || row > self.end.top {
                    0..0
                } else {
                    column_range(self.start.left, self.end.left)
                }
            }
            _ if row < self.start.top || row > self.end.top => 0..0,
            _ if self.start.top == self.end.top => column_range(self.start.left, self.end.left),
            _ if row == self.end.top => 0..self.end.left + 1,
            _ if row == self.start.top => self.start.left..usize::MAX,
            _ => 0..usize::MAX,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Deref)]
pub struct NormalizedSelectionRange(SelectionRange);

fn column_range(from: usize, to: usize) -> Range<usize> {
    if to >= from {
        from..to + 1
    } else {
        to..from + 1
    }
}
