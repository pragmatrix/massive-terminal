use std::cmp::Ordering;
use tracing::error;
use wezterm_term::StableRowIndex;

use crate::geometry::CellPoint;

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
            SelectionState::Begun { pos } => SelectionState::Selecting {
                start: *pos,
                end: ending_pos,
            },
            SelectionState::Selecting { start, .. } => SelectionState::Selecting {
                start: *start,
                end: ending_pos,
            },
            _ => {
                error!(
                    "Internal erorr: Selection is progressing, but state is {:?}",
                    self.state
                );
                SelectionState::Unselected
            }
        };
    }

    pub fn end(&mut self) {
        self.state = match &self.state {
            SelectionState::Begun { .. } => SelectionState::Unselected,
            SelectionState::Selecting { start, end } => SelectionState::Selected {
                start: *start,
                end: *end,
            },
            _ => {
                error!(
                    "Internal erorr: Selection is ending, but state is {:?}",
                    self.state
                );
                SelectionState::Unselected
            }
        }
    }

    // Normalized selection range
    pub fn range(&self) -> Option<SelectionRange> {
        if let SelectionState::Selecting { start, end } | SelectionState::Selected { start, end } =
            self.state
            && start != end
        {
            return Some(SelectionRange::new(start, end));
        }
        None
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
    Selecting {
        start: SelectionPos,
        end: SelectionPos,
    },
    Selected {
        start: SelectionPos,
        end: SelectionPos,
    },
}

/// Normalized selection range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionRange {
    pub start: SelectionPos,
    pub end: SelectionPos,
}

impl SelectionRange {
    pub fn new(start: SelectionPos, end: SelectionPos) -> Self {
        if end >= start {
            Self { start, end }
        } else {
            Self {
                start: end,
                end: start,
            }
        }
    }
}
