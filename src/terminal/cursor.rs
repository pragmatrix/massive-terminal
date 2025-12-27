// Cursor

use termwiz::surface::CursorVisibility;
use wezterm_term::{CursorPosition, StableRowIndex, Terminal};

use crate::{terminal::ScreenGeometry, view_state::ViewState};

#[derive(Debug, Clone)]
pub struct CursorMetrics {
    pub pos: CursorPosition,
    pub stable_y: StableRowIndex,
    pub width: usize,
    pub focused: bool,
}

impl CursorMetrics {
    pub fn new(
        terminal: &mut Terminal,
        screen_geometry: &ScreenGeometry,
        window_state: &ViewState,
    ) -> Option<Self> {
        let pos = terminal.cursor_pos();
        if pos.visibility == CursorVisibility::Hidden {
            return None;
        }

        let screen = terminal.screen_mut();

        let stable_y = screen_geometry.default_input_area.start + pos.y as StableRowIndex;
        let phys_y = screen.phys_row(pos.y);
        // Detail: This uses `visible_cells()`.
        let width = screen
            .line_mut(phys_y)
            .get_cell(pos.x)
            .map(|c| c.width())
            .unwrap_or(1);

        Some(Self {
            pos,
            stable_y,
            width,
            focused: window_state.focused,
        })
    }
}
