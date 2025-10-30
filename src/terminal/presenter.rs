use std::{ops::Range, sync::Arc};

use anyhow::Result;
use derive_more::Debug;
use log::{debug, info, trace, warn};

use massive_animation::TimeScale;
use parking_lot::Mutex;
use rangeset::RangeSet;
use termwiz::surface::SequenceNo;
use wezterm_term::{Hyperlink, Line, Screen, StableRowIndex, Terminal};

use crate::{
    TerminalView, WindowState,
    range_ops::{RangeOps, WithLength},
    terminal::{
        ScreenGeometry, SelectedRange, Selection, SelectionMode, TerminalGeometry,
        TerminalViewParams, ViewGeometry,
    },
    window_geometry::PixelPoint,
};
use massive_input::Progress;
use massive_shell::Scene;

/// The presentation logic and state we need to store to properly detect changes in the wezterm
/// Terminal instance and to update our view.
#[derive(Debug)]
pub struct TerminalPresenter {
    geometry: TerminalGeometry,
    // Architecture: The presenter should probably act as a facade to the underlying terminal and
    // not use a `Arc<Mutex<>>` here.
    #[debug(skip)]
    pub terminal: Arc<Mutex<Terminal>>,

    // Architecture: We might need to move selection and scroll state to the view, to keep the alt
    // screen completely separate? Look at the code that resets both when the view.
    scroll_state: ScrollState,

    selection: Selection,

    /// The currently underlined hyperlink, updated in update based on `mouse_pointer`.
    ///
    /// This needs to be stored to update the lines that cover it when its highlighting state
    /// changes.
    underlined_hyperlink: Option<HighlightedHyperlink>,
    pub last_rendered_seq_no: SequenceNo,
    temporary_line_buf: Vec<Line>,

    view: TerminalView,
}

impl TerminalPresenter {
    pub fn new(
        geometry: TerminalGeometry,
        terminal: Terminal,
        view_params: TerminalViewParams,
        last_rendered_seq_no: SequenceNo,
        scene: &Scene,
    ) -> Self {
        let view = TerminalView::new(view_params.clone(), false, scene, 0);
        Self {
            geometry,
            terminal: Mutex::new(terminal).into(),

            scroll_state: Default::default(),
            selection: Default::default(),

            underlined_hyperlink: None,
            last_rendered_seq_no,
            temporary_line_buf: Vec::new(),

            view,
        }
    }

    pub fn is_hyperlink_underlined_under_mouse(&self) -> bool {
        self.underlined_hyperlink.is_some()
    }

    pub fn geometry(&self) -> &TerminalGeometry {
        &self.geometry
    }

    pub fn enable_autoscroll(&mut self) {
        self.scroll_state = ScrollState::Auto;
    }

    // Returns `true` if the terminal size in cells changed.
    pub fn resize(&mut self, new_size_px: (u32, u32)) -> Result<bool> {
        let mut new_geometry = self.geometry;
        new_geometry.resize_px(new_size_px);
        if new_geometry == self.geometry {
            return Ok(false);
        }

        self.terminal
            .lock()
            .resize(new_geometry.wezterm_terminal_size());
        // Commit
        self.geometry = new_geometry;
        Ok(true)
    }

    pub fn scroll_delta_px(&mut self, delta: f64) {
        let current = self.view.final_scroll_offset_px();
        self.scroll_state = ScrollState::RestingPixel(current + delta);
    }

    /// Update the view lines, cursor, and selection.
    ///
    /// WezTerm terms:
    ///
    /// - Physical 0: The first line at the beginning of the scrollback buffer. The first line
    ///   stored in the lines of the screen.
    ///
    /// - Stable 0: The first line of the initial output. A scrolling line stays at the same index.
    ///   Would be equal to physical if the scrollback buffer would be infinite.
    ///
    /// - Visible 0: Top of the screen.
    pub fn update(
        &mut self,
        window_state: &WindowState,
        scene: &Scene,
        // View relative mouse pointer coordinates.
        // Architecture: Shouldn't this come via `window_state`?
        mouse_pointer: Option<PixelPoint>,
    ) -> Result<()> {
        // Currently we need always apply view animations, otherwise the scroll matrix is not
        // in sync with the updated lines which results in flickering while scrolling (i.e.
        // lines disappearing too early when scrolling up).
        //
        // Architecture: This is an indication of what's actually wrong with the ApplyAnimations
        // concept.
        self.view.apply_animations();

        let mut terminal = self.terminal.lock();

        if Self::sync_alt_screen(&terminal, &mut self.view, scene) {
            self.selection = Selection::Unselected;
            self.scroll_state = ScrollState::Auto;
        }

        // Performance: May be there is need to lock the terminal if there are no visible changes
        let current_seq_no = terminal.current_seqno();
        let terminal_updated = current_seq_no > self.last_rendered_seq_no;
        assert!(current_seq_no >= self.last_rendered_seq_no);

        let screen_geometry = ScreenGeometry::new(terminal.screen());

        let view = &mut self.view;

        let view_geometry = view.geometry(&self.geometry);
        let view_visible_range = view_geometry.stable_range.clone();

        // Update hyperlink

        let mut hyperlink_changed_lines = RangeSet::new();

        {
            let mut new_hyperlink = None;
            // Architecture: pass mouse pointer pos in update?
            if let Some(mouse_pointer) = mouse_pointer {
                let cell_pos = view_geometry.hit_test_cell(mouse_pointer);
                let cell = view_geometry.get_cell(cell_pos, terminal.screen_mut());
                new_hyperlink = cell
                    .and_then(|cell| cell.attrs().hyperlink())
                    .map(|hyperlink| HighlightedHyperlink {
                        hyperlink: hyperlink.clone(),
                        row: cell_pos.row,
                    });
            }

            let screen = terminal.screen();

            if self.underlined_hyperlink != new_hyperlink {
                if let Some(hyperlink) = &self.underlined_hyperlink {
                    hyperlink_changed_lines.add_range(hyperlink.line_coverage(screen));
                }
                if let Some(hyperlink) = &new_hyperlink {
                    hyperlink_changed_lines.add_range(hyperlink.line_coverage(screen));
                }

                self.underlined_hyperlink = new_hyperlink;
            }
        }

        // Be sure the changed lines are not outside the currently visible range of the view.
        hyperlink_changed_lines =
            hyperlink_changed_lines.intersection_with_range(view_visible_range.clone());

        // This is now temporarily disabled. It may start flickering at situations we go past
        // the scrollback buffer, but otherwise it reduces the animation smoothness.
        #[cfg(false)]
        {
            // If the view's stable range is out of range compared to the terminal's current
            // physical range (it's visible area in a regular terminal), it means that scrolling
            // lags behind at least one screen. In this case, reset scrolling and get a new
            // view_range.
            if !terminal_visible_stable_range.intersects(&view_stable_range) {
                debug!("Finalizing scrolling animation (terminal view is far away from ours)");
                view.finalize_animations();
                view_stable_range = view.view_range(screen.physical_rows)
            }

            trace!(
                "View's stable range: {view_stable_range:?}, current top: {}",
                view.scroll_offset()
            );
        }

        // Set up the lines to update with the ones the view requests explicitly (For example caused
        // through scrolling).
        let (mut view_update, mut lines_requested) = view.begin_update(
            scene,
            view_visible_range.clone(),
            terminal.get_reverse_video(),
        );

        // Add the hyperlink changed lines first.
        lines_requested.add_set(&hyperlink_changed_lines);

        // The range of existing lines in the terminal that intersect with the view_stable_range.
        let terminal_view_lines = screen_geometry.buffer_range.intersect(&view_visible_range);

        let screen = terminal.screen();

        // Extend the lines_requested range by the lines that have actually changed in the view
        // range.
        //
        // Detail: Need to pass a valid terminal range, passing a larger range would return lines
        // outside of the requested range because of internal alignment rules.
        //
        // Architecture: Changed lines should probably be a range set (see it's later use in the
        // selection part)?
        let mut changed_lines = Vec::new();
        if terminal_updated && let Some(terminal_range) = terminal_view_lines {
            changed_lines =
                screen.get_changed_stable_rows(terminal_range, self.last_rendered_seq_no);

            changed_lines.iter().for_each(|l| {
                debug_assert!(view_visible_range.contains(l));
                lines_requested.add(*l)
            })
        }

        // Now the updated lines are known, but some of them might not be inside the terminal's
        // buffer range. Split them between terminal lines and empty ones.
        //
        // Performance: Only lines_requested could be out of the full stable range.
        let terminal_lines_requested =
            lines_requested.intersection_with_range(screen_geometry.buffer_range.clone());

        let out_of_terminal_range_requested = {
            lines_requested.remove_set(&terminal_lines_requested);
            lines_requested
        };

        trace!("Updating lines: {terminal_lines_requested:?}");

        for stable_range in terminal_lines_requested.iter() {
            // Detail: This function returns bogus (wraps) if stable range is out of range, so we
            // must be sure to not request lines outside of the stable bounds.
            debug_assert!(stable_range.is_inside(&screen_geometry.buffer_range));
            let phys_range = screen.stable_range(stable_range);

            // Performance: After a terminal `clear`, _all_ lines below the cursor are
            // invalidated for some reason (there _is_ a `SequenceNo` for every line, may be
            // there is a way to find out if the lines actually have changed).
            screen.with_phys_lines(phys_range.clone(), |lines| {
                // Detail: guaranteed to be called only once for all lines.
                debug_assert_eq!(lines.len(), phys_range.len());
                self.temporary_line_buf
                    .extend(lines.iter().copied().cloned());
            });
        }

        // Gather everything from the terminal we need later.

        let cursor_pos = terminal.cursor_pos();
        let cursor_stable_y = screen_geometry.visible_range.start + cursor_pos.y as StableRowIndex;
        let selected_range = view_geometry.selected_user_range(&self.selection);
        let selected_range =
            selected_range.and_then(|r| r.extend(self.selection.mode().unwrap(), &terminal));

        // ADR: Need to keep the time we lock the Terminal as short as possible, so that terminal
        // changes can be pushed to it as fast as possible.
        drop(terminal);

        let hyperlink = self.underlined_hyperlink.as_ref().map(|h| &h.hyperlink);

        // Push the lines to the view.
        {
            let mut lines_index = 0;
            for stable_range in terminal_lines_requested.iter() {
                let lines_count = stable_range.len();

                view_update.lines(
                    stable_range.start,
                    &self.temporary_line_buf[lines_index.with_len(lines_count)],
                    hyperlink,
                )?;

                lines_index += lines_count;
            }
            self.temporary_line_buf.clear();
        }

        // Push the lines that were requested, but were out of range.
        {
            for stable_range in out_of_terminal_range_requested.iter() {
                let len = stable_range.len();
                if len > self.temporary_line_buf.len() {
                    self.temporary_line_buf
                        .resize_with(len, || Line::new(current_seq_no));
                }

                view_update.lines(stable_range.start, &self.temporary_line_buf[0..len], None)?;
            }
            self.temporary_line_buf.clear();
        }

        // Update cursor

        view_update.cursor(cursor_pos, cursor_stable_y, window_state.focused);

        // Update selection
        {
            let selection_rows = selected_range.map(|s| s.stable_rows()).unwrap_or_default();
            let changes_intersect_with_selection =
                changed_lines.iter().any(|l| selection_rows.contains(l));

            // Clear the selection if changes intersect it and the user does not interact with it.
            if changes_intersect_with_selection && !self.selection.can_progress() {
                self.selection.reset();
            }
            view_update.selection(
                selected_range
                    // The clamping is needed, otherwise we could keep too many matrix locations.
                    // Architecture: The clamping should happen in the view (there where the problem arises)
                    .and_then(|range| {
                        range.clamp_to_rows(
                            screen_geometry.buffer_range.clone(),
                            screen_geometry.columns,
                        )
                    }),
                &self.geometry,
            );
        }

        drop(view_update);

        // Commit
        //
        // Architecture: May be last_rendered_seq_no belongs into the view?

        self.last_rendered_seq_no = current_seq_no;

        // Now that we've done all updates. We apply the current scroll state to the view.
        //
        // Applying the scroll state has no immediate effect. Only at the time apply_animations is
        // called.
        self.scroll_state
            .apply_to_view(view, &self.geometry, &screen_geometry);

        Ok(())
    }

    // Returns true if the terminal view was changed.
    fn sync_alt_screen(terminal: &Terminal, view: &mut TerminalView, scene: &Scene) -> bool {
        // Switch between primary and alt screen.
        //
        // Architecture: If we do switch here, we overwrite all scrolling / apply animations done
        // above, this seems broken. I.e. animations do not need to be applied in this case.
        // And what if scrolling later interferes with a switch?
        let alt_screen_active = terminal.is_alt_screen_active();
        if alt_screen_active == view.alt_screen {
            return false;
        }

        // Switch
        let scroll_offset = terminal.screen().visible_row_to_stable_row(0);
        info!(
            "Switching to {} view at scroll offset {scroll_offset}",
            if alt_screen_active {
                "alternate"
            } else {
                "primary"
            }
        );
        let params = view.params.clone();
        *view = TerminalView::new(params, alt_screen_active, scene, scroll_offset);
        true
    }
}

// Selection

impl TerminalPresenter {
    /// This way we can upgrade to a triple click / word selection.
    ///
    /// Architecture: I don't like this here, and should triple clicks be a mouse gesture, and not
    /// detected by two double clicks in a row?
    pub fn selection_in_word_mode_and_selected(&self) -> bool {
        matches!(
            self.selection,
            Selection::Selected {
                mode: SelectionMode::Word,
                ..
            }
        )
    }

    pub fn selection_begin(&mut self, mode: SelectionMode, hit: PixelPoint) {
        if self.selection != Selection::Unselected {
            warn!(
                "Selection begins with active selection {:?}",
                self.selection
            );
        }
        self.selection_clear();
        self.selection
            .begin(mode, hit, self.view_geometry().hit_test_cell(hit));
    }

    pub fn selection_clear(&mut self) {
        self.clear_selection_scroller();
        self.selection.reset();
    }

    const PIXEL_TO_SCROLL_VELOCITY_PER_SECOND: f64 = 16.0;

    pub fn selection_can_progress(&self) -> bool {
        self.selection.can_progress()
    }

    /// Returns false if selection can not progress (does not expect any).
    pub fn selection_progress(&mut self, scene: &Scene, progress: Progress<PixelPoint>) {
        assert!(self.selection.can_progress());

        match progress {
            Progress::Proceed(view_hit) => {
                // Scroll?
                let pixel_velocity = self.geometry().scroll_distance_px(view_hit);
                if let Some(velocity) = pixel_velocity {
                    self.scroll_selection(
                        scene,
                        velocity * Self::PIXEL_TO_SCROLL_VELOCITY_PER_SECOND,
                    )
                } else {
                    self.clear_selection_scroller()
                }

                self.selection.progress(view_hit);
            }
            Progress::Commit => {
                self.clear_selection_scroller();
                if let Some(end) = self.selection.selecting_end() {
                    let pos = self.view_geometry().hit_test_cell(end);
                    self.selection.end(pos)
                }
            }
            Progress::Cancel => {
                self.clear_selection_scroller();
                self.selection.reset();
            }
        };
    }

    pub fn selected_range(&self) -> Option<SelectedRange> {
        // Architecture: May be a SelectedUserRange can transport SelectionMode?
        let range = self.view_geometry().selected_user_range(&self.selection);
        range.and_then(|r| r.extend(self.selection.mode().unwrap(), &self.terminal.lock()))
    }

    pub fn view_geometry(&self) -> ViewGeometry {
        self.view.geometry(self.geometry())
    }
}

// Selection Scrolling

impl TerminalPresenter {
    fn scroll_selection(&mut self, scene: &Scene, velocity: f64) {
        match &mut self.scroll_state {
            ScrollState::SelectionScroll(scroller) => scroller.velocity = velocity,
            state => {
                *state = ScrollState::SelectionScroll(SelectionScroller {
                    velocity,
                    time_scale: scene.time_scale(),
                })
            }
        }
    }

    fn clear_selection_scroller(&mut self) {
        if let ScrollState::SelectionScroll(SelectionScroller { velocity, .. }) = self.scroll_state
        {
            // Ergonomics: This scroll direction movement detection does only matter when velocity
            // is slow, otherwise it seems that the velocity animation gets redirected anyways even if
            // `view.apply_animations()` is called.
            let prefer_to_scroll_up = velocity < 0.;

            let geometry = self.view.geometry(&self.geometry);

            let resting_row = if geometry.stable_range_ascend_px == 0 || prefer_to_scroll_up {
                geometry.stable_range.start.max(0)
            } else {
                (geometry.stable_range.start + 1).max(0)
            };

            self.scroll_state =
                ScrollState::RestingPixel(self.geometry.stable_px_offset(resting_row) as f64);
        }
    }
}

#[derive(Debug, Default)]
enum ScrollState {
    /// Automatically scroll to the cursor position / last line.
    #[default]
    Auto,
    /// We are at a stable resting pixel position. This is used for mouse wheel scrolling and when
    /// the selection scrolling stops.
    RestingPixel(f64),
    /// The selection is currently controlling the scrolling with a particular velocity.
    SelectionScroll(SelectionScroller),
}

#[derive(Debug)]
struct SelectionScroller {
    velocity: f64,
    time_scale: TimeScale,
}

impl ScrollState {
    fn apply_to_view(
        &mut self,
        view: &mut TerminalView,
        geometry: &TerminalGeometry,
        screen_geometry: &ScreenGeometry,
    ) {
        match self {
            ScrollState::Auto => {
                view.scroll_to_stable(screen_geometry.visible_range.start);
            }
            ScrollState::RestingPixel(pixel) => {
                let scroll_offset_px =
                    geometry.clamp_px_offset(screen_geometry.buffer_range.clone(), *pixel);
                view.scroll_to_px(scroll_offset_px);
            }
            ScrollState::SelectionScroll(scroller) => {
                let current_px_offset = view.current_scroll_offset_px();
                let scaled_velocity = scroller.velocity * scroller.time_scale.scale_seconds();
                let final_px_offset = current_px_offset + scaled_velocity;
                let final_px_offset_clamped =
                    geometry.clamp_px_offset(screen_geometry.buffer_range.clone(), final_px_offset);
                view.scroll_to_px(final_px_offset_clamped);
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct HighlightedHyperlink {
    hyperlink: Arc<Hyperlink>,
    row: StableRowIndex,
}

impl HighlightedHyperlink {
    fn line_coverage(&self, screen: &Screen) -> Range<StableRowIndex> {
        // Performance: We may return only the physical range that actually contain the hyperlink.

        let mut range = self.row.with_len(1);

        screen.for_each_logical_line_in_stable_range(
            range.clone(),
            |physical_lines_covering_logical, _| {
                range = physical_lines_covering_logical;
                // Need to pick only on one line.
                false // Don't continue
            },
        );

        range
    }
}
