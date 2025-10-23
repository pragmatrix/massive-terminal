use std::ops::Range;

use wezterm_term::{Screen, StableRowIndex};

use crate::range_ops::WithLength;

#[derive(Debug)]
pub struct ScreenGeometry {
    // The stable range of the visible part of the terminal.
    pub visible_range: Range<StableRowIndex>,
    // The range the terminal has line data for.
    pub buffer_range: Range<StableRowIndex>,
    pub columns: usize,
}

impl ScreenGeometry {
    pub fn new(screen: &Screen) -> Self {
        let visible_range = screen
            .visible_row_to_stable_row(0)
            .with_len(screen.physical_rows);

        let buffer_range = screen.phys_to_stable_row_index(0).with_len(
            screen.scrollback_rows(), /* does include the visible part */
        );

        Self {
            visible_range,
            buffer_range,
            columns: screen.physical_cols,
        }
    }
}
