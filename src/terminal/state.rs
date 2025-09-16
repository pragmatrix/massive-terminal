//! The state we need to store to properly detect changes in the wezterm Terminal instance and to
//! update our view.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use derive_more::Debug;
use log::{debug, info};
use massive_shell::Scene;

use crate::{
    TerminalView, WindowState,
    range_tools::{RangeTools, WithLength},
    selection::{Selection, SelectionPos},
};
use massive_input::Progress;
use termwiz::surface::SequenceNo;
use wezterm_term::{CursorPosition, Line, StableRowIndex, Terminal};

#[derive(Debug)]
pub struct TerminalState {
    #[debug(skip)]
    #[allow(clippy::type_complexity)]
    view_gen: Box<dyn Fn(&Scene, StableRowIndex) -> TerminalView + Send>,
    pub last_rendered_seq_no: SequenceNo,
    temporary_line_buf: Vec<Line>,
    selection: Selection,
    view: TerminalView,
    alt_screen_active: bool,
}

impl TerminalState {
    pub fn new(
        view_gen: impl Fn(&Scene, StableRowIndex) -> TerminalView + Send + 'static,
        last_rendered_seq_no: SequenceNo,
        scene: &Scene,
    ) -> Self {
        let view = view_gen(scene, 0);
        Self {
            view_gen: Box::new(view_gen),
            last_rendered_seq_no,
            temporary_line_buf: Vec::new(),

            selection: Default::default(),
            view,
            alt_screen_active: false,
        }
    }

    /// Update the view lines, cursor, and selection.
    pub fn update(
        &mut self,
        terminal: &Arc<Mutex<Terminal>>,
        window_state: &WindowState,
        scene: &Scene,
    ) -> Result<()> {
        let view = &mut self.view;

        // Currently we need always apply view animations, otherwise the scroll matrix is not
        // in sync with the updated lines which results in flickering while scrolling (i.e.
        // lines disappearing too early when scrolling up).
        //
        // Architecture: This is a pointer to what's actually wrong with the ApplyAnimations
        // concept.
        view.apply_animations();

        let terminal = terminal.lock().unwrap();
        let screen = terminal.screen();

        // Switch between primary and alt screen.
        {
            let alt_screen_active = terminal.is_alt_screen_active();
            if alt_screen_active != self.alt_screen_active {
                // Switch
                let scroll_offset = screen.visible_row_to_stable_row(0);
                info!(
                    "Switching to {} view at scroll offset {scroll_offset}",
                    if alt_screen_active {
                        "alternate"
                    } else {
                        "primary"
                    }
                );
                *view = (self.view_gen)(scene, scroll_offset);
                self.alt_screen_active = alt_screen_active;
            }
        }

        // Performance: No need to begin an update cycle if there are no visible changes
        let current_seq_no = terminal.current_seqno();
        let terminal_updated = current_seq_no > self.last_rendered_seq_no;
        assert!(current_seq_no >= self.last_rendered_seq_no);

        // Physical: 0: The first line at the beginning of the scrollback buffer. The first
        // line stored in the lines of the screen.
        //
        // Stable: 0: The first line of the initial output. A scrolling line stays at the
        // same index. Would be equal to physical if the scrollback buffer would be
        // infinite.
        //
        // Visible: 0: Top of the screen.

        // We need to scroll first, so that the visible range is up to date (even though this
        // should not make a difference when the view is currently animating).
        let stable_top_in_screen_view = screen.visible_row_to_stable_row(0);
        view.scroll_to(stable_top_in_screen_view);

        // Get the stable view range from the view. It can't be computed here, because of the
        // animation range.
        let mut view_stable_range;
        {
            view_stable_range = view.view_range(screen.physical_rows);

            // If the view's stable range is out of range compared to the final view, it means that
            // scrolling lags behind at least one screen. In this case, reset scrolling and get a
            // new view_range.
            //
            // Detail: As a side effect, this also makes sure that the terminal always returns a
            // correct range of updated lines (which it doesn't if lines are requested outside of
            // its scrollback buffer).

            let current_terminal_stable_phys_range =
                stable_top_in_screen_view.with_len(screen.physical_rows);

            if !current_terminal_stable_phys_range.intersects(&view_stable_range) {
                debug!("Resetting scrolling animation (terminal view is far away from ours)");
                view.reset_animations();
                view_stable_range = view.view_range(screen.physical_rows)
            }

            info!(
                "View's stable range: {view_stable_range:?}, current top: {}",
                view.scroll_offset()
            );
        }

        // Set up the lines to update with the ones the view requests explicitly (For example caused
        // through scrolling).
        let mut lines_to_update = view.update_view_range(scene, view_stable_range.clone());

        // Extend the range by the lines that have actually changed in the view range.
        let lines_changed_stable = if terminal_updated {
            screen.get_changed_stable_rows(view_stable_range.clone(), self.last_rendered_seq_no)
        } else {
            Vec::new()
        };

        lines_changed_stable.into_iter().for_each(|l| {
            debug_assert!(view_stable_range.contains(&l));
            lines_to_update.add(l)
        });

        for stable_range in lines_to_update.iter() {
            let phys_range = screen.stable_range(stable_range);
            // Performance: After a terminal `clear`, _all_ lines below the cursor are
            // invalidated for some reason (there _is_ a `SequenceNo` for every line, may be
            // there is a way to find out if the lines actually have changed).

            screen.with_phys_lines(phys_range, |lines| {
                // This is guaranteed to be called only once for all lines.
                self.temporary_line_buf
                    .extend(lines.iter().copied().cloned());
            });
        }

        let cursor_pos = terminal.cursor_pos();

        // ADR: Decided to keep the time we lock the Terminal as short as possible, so that terminal
        // changes can be produced as fast as possible.
        drop(terminal);

        // Push the lines to the view.
        let mut lines_index = 0;
        for stable_range in lines_to_update.iter() {
            let lines_count = stable_range.len();

            view.update_lines(
                stable_range.start,
                &self.temporary_line_buf[lines_index.with_len(lines_count)],
            )?;

            lines_index += lines_count;
        }
        self.temporary_line_buf.clear();

        // Update cursor and selection

        Self::update_cursor(cursor_pos, view, window_state.focused, scene);

        view.update_selection(
            scene,
            self.selection.range(),
            &window_state.terminal_geometry,
        );

        // Commit

        self.last_rendered_seq_no = current_seq_no;

        Ok(())
    }

    /// Update the cursor of the view to reflect to position in the terminal.
    // Architecture: Not sure where this belongs to.
    pub fn update_cursor(
        cursor_pos: CursorPosition,
        view: &mut TerminalView,
        focused: bool,
        scene: &Scene,
    ) {
        view.update_cursor(scene, cursor_pos, focused);
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
        SelectionPos::new(vis_cell.0, vis_cell.1 as isize + self.view.scroll_offset())
    }
}
