//! The state we need to store to properly detect changes in the wezterm Terminal instance and to update our Panel.

use anyhow::Result;

use crate::Panel;
use massive_scene::Scene;
use rangeset::RangeSet;
use termwiz::surface::SequenceNo;
use wezterm_term::{StableRowIndex, Terminal};

#[derive(Debug)]
pub struct TerminalState {
    pub last_rendered_seq_no: SequenceNo,
    // For scroll detection. Primary screen only.
    pub current_stable_top_primary: StableRowIndex,
}

impl TerminalState {
    pub fn new(last_rendered_seq_no: SequenceNo) -> Self {
        Self {
            last_rendered_seq_no,
            current_stable_top_primary: 0,
        }
    }

    /// Update the panel lines.
    pub fn update_lines(
        &mut self,
        terminal: &Terminal,
        panel: &mut Panel,
        scene: &Scene,
    ) -> Result<()> {
        let alt_screen_active = terminal.is_alt_screen_active();

        // Performance: No need to begin an update cycle if there are no visible changes
        let update_lines = {
            let current_seq_no = terminal.current_seqno();
            assert!(current_seq_no >= self.last_rendered_seq_no);
            current_seq_no > self.last_rendered_seq_no
        };

        if update_lines {
            let screen = terminal.screen();
            // Physical: 0: The first line at the beginning of the scrollback buffer. The first
            // line stored in the lines of the screen.
            //
            // Stable: 0: The first line of the initial output. A scrolling line stays at the
            // same index. Would be equal to physical if the scrollback buffer would be
            // infinite.
            //
            // Visible: 0: Top of the screen.

            let stable_top = screen.visible_row_to_stable_row(0);
            if !alt_screen_active && stable_top != self.current_stable_top_primary {
                panel.scroll(stable_top - self.current_stable_top_primary);
                self.current_stable_top_primary = stable_top;
            }

            let view_stable_range = stable_top..stable_top + screen.physical_rows as isize;

            // Production: Add a kind of view into the stable rows?
            let lines_changed_stable =
                screen.get_changed_stable_rows(view_stable_range, self.last_rendered_seq_no);

            let mut set = RangeSet::new();
            lines_changed_stable.into_iter().for_each(|l| set.add(l));

            for stable_range in set.iter() {
                let phys_range = screen.stable_range(stable_range);

                assert!(stable_range.start >= stable_top);
                let visible_range_start = stable_range.start - stable_top;

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

                let mut r = Ok(());

                screen.with_phys_lines(phys_range, |lines| {
                    // This is guaranteed to be called only once for all lines.
                    r = panel.update_lines(scene, visible_range_start as usize, lines);
                });

                r?;
            }

            // Commit

            self.last_rendered_seq_no = terminal.current_seqno();
        }
        Ok(())
    }

    /// Update the cursor of the panel to reflect to position in the terminal.
    // Architecture: Not sure where this belongs to.
    pub fn update_cursor(terminal: &Terminal, panel: &mut Panel, focused: bool, scene: &Scene) {
        let pos = terminal.cursor_pos();
        panel.update_cursor(scene, pos, focused);
    }
}
