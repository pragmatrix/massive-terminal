//! The state we need to store to properly detect changes in the wezterm Terminal instance and to update our Panel.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use massive_input::Progress;

use crate::{
    Panel, WindowState,
    selection::{Selection, SelectionPos},
};
use massive_scene::Scene;
use rangeset::RangeSet;
use termwiz::surface::SequenceNo;
use wezterm_term::{CursorPosition, Line, StableRowIndex, Terminal};

#[derive(Debug)]
pub struct TerminalState {
    pub last_rendered_seq_no: SequenceNo,
    // For scroll detection. Primary screen only.
    pub current_stable_top_primary: StableRowIndex,
    line_buf: Vec<Line>,
    selection: Selection,
}

impl TerminalState {
    pub fn new(last_rendered_seq_no: SequenceNo) -> Self {
        Self {
            last_rendered_seq_no,
            current_stable_top_primary: 0,
            line_buf: Vec::new(),
            selection: Default::default(),
        }
    }

    /// Update the panel lines, cursor, and selection.
    pub fn update(
        &mut self,
        terminal: &Arc<Mutex<Terminal>>,
        window_state: &WindowState,
        panel: &mut Panel,
        scene: &Scene,
    ) -> Result<()> {
        let terminal = terminal.lock().unwrap();
        let alt_screen_active = terminal.is_alt_screen_active();

        let screen = terminal.screen();

        // Performance: No need to begin an update cycle if there are no visible changes
        let current_seq_no = terminal.current_seqno();
        let terminal_updated = current_seq_no > self.last_rendered_seq_no;
        assert!(current_seq_no >= self.last_rendered_seq_no);
        let stable_top = screen.visible_row_to_stable_row(0);
        // The stable row indices of the lines that need updating.
        let mut lines_to_update = RangeSet::new();

        // Physical: 0: The first line at the beginning of the scrollback buffer. The first
        // line stored in the lines of the screen.
        //
        // Stable: 0: The first line of the initial output. A scrolling line stays at the
        // same index. Would be equal to physical if the scrollback buffer would be
        // infinite.
        //
        // Visible: 0: Top of the screen.

        let mut scroll_amount = 0;

        if !alt_screen_active && stable_top != self.current_stable_top_primary {
            scroll_amount = stable_top - self.current_stable_top_primary;
            self.current_stable_top_primary = stable_top;
        }

        // We need to scroll first, so that the visible range is up to date (even though this
        // should not make a difference when the panel is currently animating).

        if scroll_amount != 0 {
            panel.scroll(scroll_amount);
        }

        // Get the stable view range from the panel. It can't be computed here, because of the
        // animation range.
        let view_stable_range = panel.view_range(screen.physical_rows);

        // Set up the lines to update with the ones the panel requests explicitly (For example
        // caused through scrolling in new lines).
        lines_to_update = panel.update_view_range(scene, view_stable_range.clone());

        // Extend the range by the lines that have actually changed in the view range.
        let lines_changed_stable = if terminal_updated {
            screen.get_changed_stable_rows(view_stable_range, self.last_rendered_seq_no)
        } else {
            Vec::new()
        };

        lines_changed_stable
            .into_iter()
            .for_each(|l| lines_to_update.add(l));

        for stable_range in lines_to_update.iter() {
            let phys_range = screen.stable_range(stable_range);
            assert!(stable_range.start >= stable_top);

            // Performance: After a terminal `clear`, _all_ lines below the cursor are
            // invalidated for some reason (there _is_ a `SequenceNo` for every line, may be
            // there is a way to find out if the lines actually have changed).

            screen.with_phys_lines(phys_range, |lines| {
                // This is guaranteed to be called only once for all lines.
                self.line_buf.extend(lines.iter().copied().cloned());
            });
        }

        let cursor_pos = terminal.cursor_pos();

        // ADR: Decided to keep the time we lock the Terminal as short as possible, so that terminal
        // changes can be produced as fast as possible.
        drop(terminal);

        // Push the lines to the panel.
        let mut lines_index = 0;
        for stable_range in lines_to_update.iter() {
            let lines_count = stable_range.len();

            panel.update_lines(
                stable_range.start,
                &self.line_buf[lines_index..lines_index + lines_count],
            )?;

            lines_index += lines_count;
        }
        self.line_buf.clear();

        // Update cursor and selection

        Self::update_cursor(cursor_pos, panel, window_state.focused, scene);

        panel.update_selection(
            scene,
            self.selection.range(),
            &window_state.terminal_geometry,
        );

        // Commit

        self.last_rendered_seq_no = current_seq_no;

        Ok(())
    }

    /// Update the cursor of the panel to reflect to position in the terminal.
    // Architecture: Not sure where this belongs to.
    pub fn update_cursor(
        cursor_pos: CursorPosition,
        panel: &mut Panel,
        focused: bool,
        scene: &Scene,
    ) {
        panel.update_cursor(scene, cursor_pos, focused);
    }

    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    pub fn selection_begin(&mut self, vis_cell: (usize, usize)) {
        let pos = self.visible_cell_to_selection_pos(vis_cell);
        self.selection.begin(pos);
    }

    pub fn selection_can_progress(&self) -> bool {
        self.selection.can_progress()
    }

    pub fn selection_progress(&mut self, progress: Progress<(usize, usize)>) {
        match progress {
            Progress::Proceed(cell_hit) => {
                let pos = self.visible_cell_to_selection_pos(cell_hit);
                self.selection.progress(pos);
            }
            Progress::Commit => self.selection.end(),
            Progress::Cancel => self.selection.reset(),
        }
    }

    pub fn visible_cell_to_selection_pos(&self, vis_cell: (usize, usize)) -> SelectionPos {
        // Bug: What about secondary screen?
        SelectionPos::new(
            vis_cell.0,
            vis_cell.1 as isize + self.current_stable_top_primary,
        )
    }
}
