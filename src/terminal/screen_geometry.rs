use std::ops::Range;

use wezterm_term::{Screen, StableRowIndex};

use crate::range_ops::WithLength;

#[derive(Debug)]
pub struct ScreenGeometry {
    /// The default stable range of the part of the terminal that covers the bottom extents of the
    /// terminal.
    ///
    /// This does not take scrolling into the scrollback buffer into account. It represents the
    /// screen area that should visible when the user is typing.
    pub default_input_area: Range<StableRowIndex>,

    // The range the terminal has line data for.
    pub buffer_area: Range<StableRowIndex>,

    #[allow(unused)]
    pub columns: usize,
}

impl ScreenGeometry {
    pub fn new(screen: &Screen) -> Self {
        let buffer_area = screen.phys_to_stable_row_index(0).with_len(
            screen.scrollback_rows(), /* does include the visible part */
        );

        let default_input_area = screen
            .visible_row_to_stable_row(0)
            .with_len(screen.physical_rows);

        Self {
            default_input_area,
            buffer_area,
            columns: screen.physical_cols,
        }
    }
}
