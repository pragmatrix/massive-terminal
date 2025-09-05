use tracing::error;
use wezterm_term::StableRowIndex;

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
}

#[derive(Debug, Clone, Copy)]
pub struct SelectionPos {
    left: usize,
    top: StableRowIndex,
}

impl SelectionPos {
    pub fn new(left: usize, top: StableRowIndex) -> Self {
        Self { left, top }
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
