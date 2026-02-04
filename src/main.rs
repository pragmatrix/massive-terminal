use std::{
    io::{self, ErrorKind},
    sync::{Arc, Weak},
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use arboard::Clipboard;
use derive_more::Debug;
use log::{debug, info, trace, warn};
use parking_lot::Mutex;
use serde::Deserialize;
use tokio::{pin, select, sync::Notify, task};
use url::Url;
use winit::{
    event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase},
    window::CursorIcon,
};

use portable_pty::{CommandBuilder, PtyPair, native_pty_system};
use wezterm_term::{
    KeyCode, KeyModifiers, Line, MouseEvent, StableRowIndex, Terminal, TerminalConfiguration, color,
};

use massive_applications::{InstanceContext, InstanceEvent, View, ViewEvent, ViewId};
use massive_desktop::{Application, Desktop, DesktopEnvironment};
use massive_geometry::{Color, Point, SizePx};
use massive_input::{Event, EventManager, ExternalEvent, MouseGesture, Movement};
use massive_renderer::FontWeight;
use massive_shell::{ApplicationContext, shell};

mod input;
mod range_ops;
mod terminal;
mod view_geometry;
mod view_state;

use crate::{
    input::termwiz::{convert_modifiers, convert_mouse_event_from_view},
    range_ops::WithLength,
    terminal::*,
    view_geometry::{PixelPoint, ViewGeometry},
    view_state::ViewState,
};

const TERMINAL_NAME: &str = "MassiveTerminal";
/// Production: Extract from the build.
const TERMINAL_VERSION: &str = "1.0";
const DEFAULT_FONT_SIZE: f32 = 13.;
const DEFAULT_TERMINAL_SIZE: (usize, usize) = (80 * 2, 24 * 2);
const APPLICATION_NAME: &str = "Massive Terminal";

#[tokio::main]
async fn main() -> Result<()> {
    shell::run(run)
}

async fn run(context: ApplicationContext) -> Result<()> {
    let applications = vec![Application::new(APPLICATION_NAME, terminal_instance)];
    let desktop_env = DesktopEnvironment::new(applications);
    let desktop = Desktop::new(desktop_env, context).await?;
    desktop.run().await
}

async fn terminal_instance(mut ctx: InstanceContext) -> Result<()> {
    MassiveTerminal::new(&mut ctx).await?.run(&mut ctx).await
}

#[derive(Debug)]
struct MassiveTerminal {
    #[debug(skip)]
    pty_pair: PtyPair,

    view: View,

    event_manager: EventManager<ViewEvent>,

    view_state: ViewState,
    presenter: TerminalPresenter,
    // Architecture: This may belong into TerminalState or even TerminalView?
    terminal_scroller: TerminalScroller,

    // User state
    //
    // Architecture: The movement tracking here and the selection tracking in the presenter should
    // probably be combined.
    selecting: Option<Movement>,

    #[debug(skip)]
    clipboard: Clipboard,
}

#[derive(Debug, Deserialize)]
struct Parameters {
    command: Option<String>,
}

enum RunMode {
    // Input is active, the shell is running.
    Active,
    /// Output of the shell stopped. We are just looking at the results.
    Passive,
}

impl MassiveTerminal {
    async fn new(ctx: &mut InstanceContext) -> Result<Self> {
        // Use the shared FontManager from the context
        let fonts = ctx.fonts();
        // Don't load system fonts for now, this way we get the same result on wasm and local runs.

        const JETBRAINS_MONO: &[u8] =
            include_bytes!("fonts/JetBrainsMono-2.304/fonts/variable/JetBrainsMono[wght].ttf");

        let font_ids = fonts.load_font(JETBRAINS_MONO);

        // This font is only used for measuring the size of the terminal upfront.
        let font = fonts.get_font(font_ids[0], FontWeight::NORMAL).unwrap();

        let scale_factor = ctx.primary_monitor_scale_factor();
        let font_size = DEFAULT_FONT_SIZE * scale_factor as f32;

        let terminal_font = TerminalFont::from_cosmic_text(font, font_size)?;

        let terminal_size: SizeCell = DEFAULT_TERMINAL_SIZE.into();

        let padding_px = terminal_font.cell_size_px().width / 2;

        let terminal_geometry = TerminalGeometry::new(terminal_font.cell_size_px(), terminal_size);

        let view_geometry =
            ViewGeometry::from_terminal_geometry(&terminal_geometry, scale_factor, padding_px);

        let view_size_px = view_geometry.inner_size_px();

        // Use the native pty implementation for the system
        let pty_system = native_pty_system();

        // Create a new pty
        let pty_pair = pty_system.openpty(terminal_geometry.pty_size())?;

        // Setup Command builder
        let shell = CommandBuilder::new_default_prog().get_shell();
        let mut cmd = CommandBuilder::new(&shell);

        // Deserialize parameters
        if let Some(parameters) = ctx.parameters() {
            let p: Parameters = serde_json::from_value(parameters.clone().into())?;
            if let Some(command) = p.command {
                cmd.arg("-c");
                cmd.arg(command);
            }
        }

        let _child = pty_pair.slave.spawn_command(cmd)?;

        // I don't how what and when anything blocks, so create two channels for writing and on for
        // reading from the pty. Send data to the pty by writing to the master
        let writer = pty_pair.master.take_writer()?;

        let configuration = MassiveTerminalConfiguration {};

        let terminal = Terminal::new(
            terminal_geometry.wezterm_terminal_size(),
            Arc::new(configuration),
            TERMINAL_NAME,
            TERMINAL_VERSION,
            writer,
        );
        let last_rendered_seq_no = terminal.current_seqno();

        // Create the view first so we can present terminal content.
        let view = ctx
            .view(view_size_px)
            .with_background_color(Color::BLACK)
            .build()?;

        let view_params = TerminalViewParams {
            fonts: fonts.clone(),
            font: terminal_font.clone(),
            location: view.location().clone(),
        };

        let scene = view.scene();

        let terminal_scroller =
            TerminalScroller::new(scene, Duration::from_secs(1), Duration::from_secs(1));

        let presenter = TerminalPresenter::new(
            terminal_geometry,
            terminal,
            view_params,
            last_rendered_seq_no,
            scene,
        );

        // Set initial title
        view.set_title(APPLICATION_NAME)?;

        Ok(Self {
            pty_pair,
            view,
            event_manager: EventManager::default(),
            view_state: ViewState::new(view_geometry),
            presenter,
            terminal_scroller,
            selecting: None,
            clipboard: Clipboard::new()?,
        })
    }

    fn terminal(&self) -> &Arc<Mutex<Terminal>> {
        &self.presenter.terminal
    }

    async fn run(&mut self, ctx: &mut InstanceContext) -> Result<()> {
        let notify = Arc::new(Notify::new());
        // Read and parse output from the pty with reader
        let reader = self.pty_pair.master.try_clone_reader()?;

        let output_dispatcher =
            dispatch_output_to_terminal(reader, Arc::downgrade(self.terminal()), notify.clone());

        pin!(output_dispatcher);

        // Architecture: This is wrong. Need some way to query the current mouse pointer (from the
        // `WindowState`). Not only from events coming in.
        let mut mouse_pointer_on_view = None;

        // Architecture: This does not belong here.
        let min_movement_distance =
            self.min_pixel_distance_considered_movement(ctx.primary_monitor_scale_factor());

        let mut mode = RunMode::Active;

        loop {
            let instance_event_opt = match mode {
                RunMode::Active => {
                    select! {
                        r = &mut output_dispatcher => {
                            info!("Shell output ended. Entering passive mode: {r:?}");
                            mode = RunMode::Passive;
                            None
                        }
                        _ = notify.notified() => {
                            None
                        }
                        instance_event = ctx.wait_for_event() => {
                            Some(instance_event?)
                        }
                    }
                }
                RunMode::Passive => {
                    select! {
                        _ = notify.notified() => {
                            None
                        }
                        instance_event = ctx.wait_for_event() => {
                            Some(instance_event?)
                        }
                    }
                }
            };

            // We have to process view events before going into the update cycle for now because
            // of the borrow checker.
            //
            // Detail: Animations starting here _are_ considered, but not updates.
            if let Some(InstanceEvent::View(view_id, view_event)) = &instance_event_opt {
                self.process_view_event(
                    *view_id,
                    view_event,
                    &mut mouse_pointer_on_view,
                    min_movement_distance,
                )?;
            }

            // Handle shutdown
            if matches!(instance_event_opt, Some(InstanceEvent::Shutdown)) {
                info!("Shutdown requested. Exiting.");
                return Ok(());
            }

            // Handle animations
            if matches!(instance_event_opt, Some(InstanceEvent::ApplyAnimations)) {
                trace!("Applying animations");
                self.terminal_scroller.proceed();
            }

            {
                // Currently we need always apply view animations, otherwise the scroll locations
                // are not in sync with the updated lines which results in flickering (or even
                // staying a black screen for a while) while scrolling when the terminal moves too
                // fast. Perhaps increasing the number of lines in the scrollback buffer may help.
                self.presenter.apply_animations();

                // Update lines, selection, and cursor.
                self.presenter.update(
                    &self.view_state,
                    self.view.scene(),
                    mouse_pointer_on_view,
                )?;
            }

            // Render
            self.view.render()?;

            // Update mouse cursor shape.
            {
                let cursor_icon = if self.presenter.is_hyperlink_underlined_under_mouse() {
                    CursorIcon::Pointer
                } else {
                    CursorIcon::Default
                };
                self.view.set_cursor(cursor_icon)?;
            }

            // Rationale: In debug runs, an instance is capable of starving the desktop completely.
            // This is a basic limitation of tokio for which there does not seem to be a workaround.
            //
            // Architecture: In the long run, we need to run instances in an isolated context. At
            // least in a separate thread. They can then power up a separate tokio runtime.
            tokio::task::yield_now().await;
        }
    }

    // Robustness: May not end the terminal when this returns an error?
    // Architecture: Think about a general strategy about how to handle recoverable errors.
    // Correctness: There are a number of locks on the terminal here, may lock only once.
    fn process_view_event(
        &mut self,
        view_id: ViewId,
        view_event: &ViewEvent,
        mouse_pointer_on_view: &mut Option<PixelPoint>,
        min_movement_distance: f64,
    ) -> Result<()> {
        let now = Instant::now();
        let Some(ev) = self.event_manager.add_event(ExternalEvent {
            scope: view_id,
            event: view_event.clone(),
            time: now,
        }) else {
            // Event is redundant. If we would process them, Clicks in `mc` for example would not
            // work on the first try, because `mc` gets confused by winit's behavior to send a
            // redundant CursorMoved event before ever mouse press / release event.
            return Ok(());
        };

        let modifiers = ev.states().keyboard_modifiers();

        // View-local coordinates are already provided by ViewEvent, identity transform
        //
        // Robustness: What about padding, etc?
        let view_pos_to_terminal_view =
            |p: massive_geometry::Point| -> Option<PixelPoint> { Some((p.x, p.y).into()) };

        {
            let presenter = &mut self.presenter;
            let view_geometry = presenter.view_geometry();
            // Architecture: While we are locking the terminal, calling into presenter might lock it
            // there, too. There must be some better way here (pull out the selection, or the
            // terminal).
            let terminal = presenter.terminal.clone();
            let mut terminal = terminal.lock();

            // Process mouse pointer movements
            if let Some(pos) = ev.pos() {
                let mouse_pointer_pos = view_pos_to_terminal_view(pos);
                *mouse_pointer_on_view = mouse_pointer_pos;

                // Refresh implicit hyperlinks.
                //
                // Precision: This is asynchronous. The hit pos may be out of range, or somewhere else.
                // But good enough for now.
                if let Some(current_mouse_pos) = mouse_pointer_pos {
                    let cell_pos = view_geometry.hit_test_cell(current_mouse_pos);
                    terminal
                        .screen_mut()
                        .for_each_logical_line_in_stable_range_mut(
                            cell_pos.row.with_len(1),
                            |_, lines| {
                                Line::apply_hyperlink_rules(
                                    &config::DEFAULT_HYPERLINK_RULES,
                                    lines,
                                );
                                true
                            },
                        );
                }
            }

            // Process events that need to be forwarded to the terminal when mouse reporting is on.
            if terminal.is_mouse_grabbed() {
                Self::may_forward_event_to_terminal(
                    &ev,
                    &mut terminal,
                    &view_geometry,
                    view_pos_to_terminal_view,
                );

                self.presenter.selection_clear();
                self.selecting = None;
            } else {
                // Process selecting user state

                match &mut self.selecting {
                    None => match ev.detect_mouse_gesture(MouseButton::Left, min_movement_distance)
                    {
                        // WezTerm reacts on Click, macOS term on Clicked.
                        Some(MouseGesture::Clicked(point)) => {
                            if let Some(view_px) = view_pos_to_terminal_view(point) {
                                let cell_pos = view_geometry.hit_test_cell(view_px);
                                if let Some(cell) =
                                    view_geometry.get_cell(cell_pos, terminal.screen_mut())
                                    && let Some(hyperlink) = cell.attrs().hyperlink()
                                    && let Err(e) = open_file_http_or_mailto_url(hyperlink.uri())
                                {
                                    warn!("{e:?}");
                                }
                            }

                            presenter.selection_clear();
                        }
                        Some(MouseGesture::DoubleClick(point)) => {
                            if let Some(hit) = view_pos_to_terminal_view(point) {
                                // Architecture: May create a MouseGesture::TripleClick instead?
                                let mode = if self.presenter.selection_in_word_mode_and_selected() {
                                    // This implicitly detects a triple click and then uses line selection.
                                    SelectionMode::Line
                                } else {
                                    SelectionMode::Word
                                };

                                self.presenter.selection_begin(mode, hit);
                                self.selecting = Some(ev.track_movement()
                                    .expect("Internal error: double click gesture triggered without a mouse button event"));
                            }
                        }
                        Some(MouseGesture::Movement(movement)) => {
                            if let Some(hit) = view_pos_to_terminal_view(movement.from) {
                                self.presenter.selection_begin(SelectionMode::Cell, hit);
                            }
                            self.selecting = Some(movement);
                        }
                        _ => {}
                    },
                    Some(movement) => {
                        if let Some(progress) = movement.track_to(&ev) {
                            let progress = progress.try_map_or_cancel(view_pos_to_terminal_view);

                            self.presenter
                                .selection_progress(self.view.scene(), progress);
                            if !self.presenter.selection_can_progress() {
                                self.selecting = None;
                            }
                        }
                    }
                }
            }
        }

        // Process remaining events
        match view_event {
            ViewEvent::Resized(size) => {
                self.resize(*size)?;
            }
            ViewEvent::Focused(focused) => {
                // Architecture: Should we track the focused state of the window in the EventAggregator?
                // Architecture: Move this to the part where the terminal is locked above.
                self.view_state.focused = *focused;
                self.terminal().lock().focus_changed(*focused);
            }
            ViewEvent::MouseWheel {
                delta,
                phase: TouchPhase::Moved,
                ..
            } => {
                let delta_px = match delta {
                    MouseScrollDelta::LineDelta(_, delta) => {
                        (*delta as f64) * self.presenter.geometry().line_height_px() as f64
                    }
                    MouseScrollDelta::PixelDelta(physical_position) => physical_position.y,
                };

                self.presenter.scroll_delta_px(-delta_px)
            }
            ViewEvent::KeyboardInput { event, .. } => {
                if let Some((key, key_modifiers)) =
                    input::termwiz::convert_key_event(event, modifiers)
                {
                    match event.state {
                        ElementState::Pressed => match key {
                            KeyCode::Char('c') if key_modifiers == KeyModifiers::SUPER => {
                                self.copy()?;
                            }
                            KeyCode::Char('v') if key_modifiers == KeyModifiers::SUPER => {
                                self.paste()?
                            }
                            _ => {
                                self.terminal().lock().key_down(key, key_modifiers)?;
                                self.presenter.enable_autoscroll();
                            }
                        },
                        ElementState::Released => {
                            self.terminal().lock().key_up(key, key_modifiers)?;
                        }
                    }
                }
            }
            ViewEvent::CloseRequested => {
                // Desktop handles close requests
            }
            _ => {}
        }
        Ok(())
    }

    // Architecture: Is the presenter responsible for this?
    fn may_forward_event_to_terminal(
        ev: &Event<ViewEvent>,
        terminal: &mut Terminal,
        geometry: &TerminalViewGeometry,
        map_to_view: impl Fn(Point) -> Option<PixelPoint>,
    ) {
        debug_assert!(terminal.is_mouse_grabbed());

        let Some((kind, button, point)) = convert_mouse_event_from_view(ev) else {
            return;
        };

        // Performance: Shouldn't we check if the pos is on the view before converting the mouse event?
        let Some(point_on_view) = map_to_view(point) else {
            return;
        };

        let cell_pos = geometry.hit_test_cell(point_on_view);

        let stable_top = terminal.screen().visible_row_to_stable_row(0);
        let visible_row = cell_pos.row - stable_top;

        let Some(column): Option<usize> = cell_pos.column.try_into().ok() else {
            return;
        };

        let Some(discrete_point) = point_on_view.round().try_cast() else {
            return;
        };

        let event = MouseEvent {
            kind,
            x: column,
            y: visible_row as _,
            x_pixel_offset: discrete_point.x,
            y_pixel_offset: discrete_point.y,
            button,
            modifiers: convert_modifiers(ev.states().keyboard_modifiers()),
        };

        debug!("Sending mouse event to terminal {event:?}");
        if let Err(e) = terminal.mouse_event(event) {
            warn!("Sending mouse event to terminal failed: {e:?}")
        }
    }

    fn resize(&mut self, new_view_size_px: SizePx) -> Result<()> {
        let suggested_terminal_size_px = self.view_state.geometry.resize(new_view_size_px);
        if self.presenter.resize(suggested_terminal_size_px)? {
            self.pty_pair
                .master
                .resize(self.presenter.geometry().pty_size())?;
        }

        Ok(())
    }

    fn min_pixel_distance_considered_movement(&self, scale_factor: f64) -> f64 {
        const LOGICAL_POINTS_CONSIDERED_MOVEMENT: f64 = 5.0;
        LOGICAL_POINTS_CONSIDERED_MOVEMENT * scale_factor
    }
}

// Clipboard

impl MassiveTerminal {
    fn copy(&mut self) -> Result<()> {
        let text = self.selected_text();
        if !text.is_empty() {
            // Robustness: May not fail if this returns an error.
            self.clipboard.set_text(text)?;
        }
        Ok(())
    }

    fn paste(&mut self) -> Result<()> {
        // Robustness: May not fail if this returns an error?
        let text = self.clipboard.get_text()?;
        if !text.is_empty() {
            self.terminal().lock().send_paste(&text)?;
            self.presenter.enable_autoscroll();
        }
        Ok(())
    }
}

// Selection

impl MassiveTerminal {
    // Copied from wezterm_gui/src/termwindow/selection.rs

    // Architecture: This does not seem to belong here, push this down to the presenter at least.
    // Also it seems to lock the terminal multiple lines, see the current implementation of
    // selected_range().

    /// Returns the selected text
    pub fn selected_text(&self) -> String {
        let mut s = String::new();
        // Feature: Rectangular selection.
        let rectangular = false;
        let Some(sel) = self.presenter.selected_range() else {
            return s;
        };
        let mut last_was_wrapped = false;
        let first_row = sel.stable_rows().start;
        let last_row = sel.stable_rows().end;

        let terminal = self.terminal().lock();

        for line in get_logical_lines(&terminal, sel.stable_rows()) {
            if !s.is_empty() && !last_was_wrapped {
                s.push('\n');
            }
            let last_idx = line.physical_lines.len().saturating_sub(1);
            for (idx, phys) in line.physical_lines.iter().enumerate() {
                let this_row = line.first_row + idx as StableRowIndex;
                if this_row >= first_row && this_row < last_row {
                    let last_phys_idx = phys.len().saturating_sub(1);
                    let cols = sel.cols_for_row(this_row, rectangular);
                    let last_col_idx = cols.end.saturating_sub(1).min(last_phys_idx);
                    let col_span = phys.columns_as_str(cols);
                    // Only trim trailing whitespace if we are the last line
                    // in a wrapped sequence
                    if idx == last_idx {
                        s.push_str(col_span.trim_end());
                    } else {
                        s.push_str(&col_span);
                    }

                    last_was_wrapped = last_col_idx == last_phys_idx
                        && phys
                            .get_cell(last_col_idx)
                            .map(|c| c.attrs().wrapped())
                            .unwrap_or(false);
                }
            }
        }

        s
    }
}

#[derive(Debug)]
struct MassiveTerminalConfiguration {}

impl TerminalConfiguration for MassiveTerminalConfiguration {
    fn color_palette(&self) -> color::ColorPalette {
        // Production: Review.
        color::ColorPalette::default()
    }
}

// Detail: pass terminal as a Weak reference handle, because otherwise we would lock the terminal in
// memory, which locks the writer in memory, which causes the child process (usually a shell) to
// never terminate and therefore read() never to return here.
//
// An alternative is to use the child handle and explicitly invoke kill() on the ChildKiller trait.
async fn dispatch_output_to_terminal(
    mut reader: impl io::Read + Send + 'static,
    terminal: Weak<Mutex<Terminal>>,
    notify: Arc<Notify>,
) -> Result<()> {
    // Using a thread does not make a difference here.
    let join_handle = task::spawn_blocking(move || {
        let mut buf = [0u8; 0x8000];
        loop {
            // Usually there are not more than 1024 bytes returned on macOS.
            match reader.read(&mut buf) {
                Ok(0) => {
                    // Child process ended.
                    return Ok(()); // EOF
                }
                Ok(bytes_read) => {
                    if let Some(terminal) = terminal.upgrade() {
                        terminal.lock().advance_bytes(&buf[0..bytes_read]);
                        notify.notify_one();
                    } else {
                        // Terminal is gone.
                        return Ok(());
                    }
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {
                    // Retry as recommended.
                }
                Err(e) => {
                    return Result::Err(e);
                }
            }
        }
    });

    Ok(join_handle.await??)
}

fn open_file_http_or_mailto_url(uri: &str) -> Result<()> {
    let parsed = Url::parse(uri)?;
    let scheme = parsed.scheme();
    match scheme {
        "https" | "http" | "mailto" | "file" => Ok(opener::open(uri)?),
        _ => bail!("Unsupported URI scheme: `{scheme}` in `{uri}`"),
    }
}

mod config {
    use std::sync::LazyLock;

    use termwiz::hyperlink::{self, Rule};

    pub static DEFAULT_HYPERLINK_RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
        vec![
            // First handle URLs wrapped with punctuation (i.e. brackets)
            // e.g. [http://foo] (http://foo) <http://foo>
            Rule::with_highlight(r"\((\w+://\S+)\)", "$1", 1).unwrap(),
            Rule::with_highlight(r"\[(\w+://\S+)\]", "$1", 1).unwrap(),
            Rule::with_highlight(r"<(\w+://\S+)>", "$1", 1).unwrap(),
            // Then handle URLs not wrapped in brackets that
            // 1) have a balanced ending parenthesis or
            Rule::new(hyperlink::CLOSING_PARENTHESIS_HYPERLINK_PATTERN, "$0").unwrap(),
            // 2) include terminating _, / or - characters, if any
            Rule::new(hyperlink::GENERIC_HYPERLINK_PATTERN, "$0").unwrap(),
            // implicit mailto link
            Rule::new(r"\b\w+@[\w-]+(\.[\w-]+)+\b", "mailto:$0").unwrap(),
        ]
    });
}
