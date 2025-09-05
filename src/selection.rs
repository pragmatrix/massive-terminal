use wezterm_term::StableRowIndex;

#[derive(Debug, Default)]
pub struct Selection {
    state: SelectionState,
}

#[derive(Debug)]
pub struct SelectionPos {
    left: usize,
    top: StableRowIndex,
}

#[derive(Debug, Default)]
pub enum SelectionState {
    #[default]
    Unselected,
    Selecting {
        start: SelectionPos,
        end: SelectionPos,
    },
    Selected {
        begin: SelectionPos,
        end: SelectionPos,
    },
}
