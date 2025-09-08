//! The state we need to store to properly detect changes in the wezterm Terminal instance and to update our Panel.

use std::sync::{Arc, Mutex};

use anyhow::Result;

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

    /// Update the panel lines and the cursor.
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
        let mut scroll_amount = 0;
        let stable_top = screen.visible_row_to_stable_row(0);
        let mut set = RangeSet::new();

        if terminal_updated {
            // Physical: 0: The first line at the beginning of the scrollback buffer. The first
            // line stored in the lines of the screen.
            //
            // Stable: 0: The first line of the initial output. A scrolling line stays at the
            // same index. Would be equal to physical if the scrollback buffer would be
            // infinite.
            //
            // Visible: 0: Top of the screen.

            if !alt_screen_active && stable_top != self.current_stable_top_primary {
                scroll_amount = stable_top - self.current_stable_top_primary;
                self.current_stable_top_primary = stable_top;
            }

            let view_stable_range = stable_top..stable_top + screen.physical_rows as isize;

            // Production: Add a kind of view into the stable rows?
            let lines_changed_stable =
                screen.get_changed_stable_rows(view_stable_range, self.last_rendered_seq_no);

            lines_changed_stable.into_iter().for_each(|l| set.add(l));

            // ADR: Decided to keep the time we lock the Terminal as short as possible, so that new data
            // can be fed in as fast as possible.

            for stable_range in set.iter() {
                let phys_range = screen.stable_range(stable_range);
                assert!(stable_range.start >= stable_top);
                // let visible_range_start = stable_range.start - stable_top;

                // Architecture: Going through building a set for accessing each changed line
                // individually does not actually make sense when we just need to access Line
                // references, but we can't access them directly.
                //
                // **Update**: Currently, it does make sense because of locking FontSystem only once
                // (but hey, this could also be bad).
                //
                // Performance: After a terminal `clear`, all lines below the cursor are
                // invalidated, too for some reason (there _is_ a `SequenceNo` for every line, may
                // be there is a way to find out if the lines actually have changed).

                screen.with_phys_lines(phys_range, |lines| {
                    // This is guaranteed to be called only once for all lines.
                    self.line_buf.extend(lines.iter().copied().cloned());
                    // r = panel.update_lines(scene, visible_range_start as usize, lines);
                });
            }
        }

        let cursor_pos = terminal.cursor_pos();

        // Release the terminal lock.
        drop(terminal);

        if scroll_amount != 0 {
            panel.scroll(scroll_amount);
        }

        // Push the lines to the panel.

        let mut lines_index = 0;
        for stable_range in set.iter() {
            let visible_range_start = stable_range.start - stable_top;
            let lines_count = stable_range.len();

            panel.update_lines(
                scene,
                visible_range_start as usize,
                &self.line_buf[lines_index..lines_index + lines_count],
            )?;

            lines_index += lines_count;
        }
        self.line_buf.clear();

        Self::update_cursor(cursor_pos, panel, window_state.focused, scene);

        panel.update_selection(scene, self.selection.range(), &window_state.terminal);

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
        println!("{:?}", self.selection);
    }

    pub fn selection_can_progress(&self) -> bool {
        self.selection.can_progress()
    }

    pub fn selection_progress(&mut self, visible_cell: (usize, usize)) {
        let pos = self.visible_cell_to_selection_pos(visible_cell);
        self.selection.progress(pos);
        println!("{:?}", self.selection);
    }

    pub fn selection_end(&mut self) {
        self.selection.end();
        println!("{:?}", self.selection);
    }

    pub fn visible_cell_to_selection_pos(&self, vis_cell: (usize, usize)) -> SelectionPos {
        // Bug: What about secondary screen?
        SelectionPos::new(
            vis_cell.0,
            vis_cell.1 as isize + self.current_stable_top_primary,
        )
    }
}
