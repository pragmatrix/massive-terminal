use std::{
    io::{self, ErrorKind},
    ops::Range,
    sync::{self, Arc},
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow, bail};
use arboard::Clipboard;
use cosmic_text::{FontSystem, fontdb};
use derive_more::Debug;
use log::{info, trace, warn};
use parking_lot::Mutex;
use tokio::{pin, select, sync::Notify, task};
use url::Url;
use winit::{
    dpi::PhysicalSize,
    event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent},
    window::{CursorIcon, WindowId},
};

use portable_pty::{CommandBuilder, PtyPair, native_pty_system};
use wezterm_term::{
    KeyCode, KeyModifiers, Line, StableRowIndex, Terminal, TerminalConfiguration, color,
};

use massive_geometry::{Camera, Color, Identity};
use massive_input::{EventManager, ExternalEvent, MouseGesture, Movement};
use massive_scene::{Handle, Location, Matrix};
use massive_shell::{
    ApplicationContext, AsyncWindowRenderer, Scene, ShellEvent, ShellWindow, shell,
};

mod input;
mod logical_line;
mod range_ops;
mod terminal;
mod window_geometry;
mod window_state;

use crate::{
    logical_line::LogicalLine,
    range_ops::WithLength,
    terminal::*,
    window_geometry::{PixelPoint, WindowGeometry},
    window_state::WindowState,
};

const TERMINAL_NAME: &str = "MassiveTerminal";
/// Production: Extract from the build.
const TERMINAL_VERSION: &str = "1.0";
const DEFAULT_FONT_SIZE: f32 = 13.;
const DEFAULT_TERMINAL_SIZE: (usize, usize) = (80 * 2, 24 * 2);
const APPLICATION_NAME: &str = "Massive Terminal";

const JETBRAINS_MONO: &[u8] =
    include_bytes!("fonts/JetBrainsMono-2.304/fonts/variable/JetBrainsMono[wght].ttf");

#[tokio::main]
async fn main() -> Result<()> {
    shell::run(async |ctx| MassiveTerminal::new(ctx).await?.run().await)
}

#[derive(Debug)]
struct MassiveTerminal {
    context: ApplicationContext,
    window: ShellWindow,
    renderer: AsyncWindowRenderer,

    #[debug(skip)]
    pty_pair: PtyPair,

    scene: Scene,
    view_matrix: Handle<Matrix>,

    event_manager: EventManager,

    window_state: WindowState,
    presenter: TerminalPresenter,
    // Architecture: This may belong into TerminalState or even TerminalView?
    terminal_scroller: TerminalScroller,

    // User state
    selecting: Option<Movement>,

    #[debug(skip)]
    clipboard: Clipboard,
}

impl MassiveTerminal {
    async fn new(context: ApplicationContext) -> Result<Self> {
        let ids;
        let mut font_system = {
            // In wasm the system locale can't be acquired. `sys_locale::get_locale()`
            let locale =
                sys_locale::get_locale().ok_or(anyhow!("Failed to retrieve current locale"))?;

            // Don't load system fonts for now, this way we get the same result on wasm and local runs.
            let mut font_db = fontdb::Database::new();
            let source = fontdb::Source::Binary(Arc::new(JETBRAINS_MONO));
            ids = font_db.load_font_source(source);
            FontSystem::new_with_locale_and_db(locale, font_db)
        };

        let font = font_system.get_font(ids[0]).unwrap();

        let scale_factor = context.primary_monitor_scale_factor().unwrap_or(1.0);
        let font_size = DEFAULT_FONT_SIZE * scale_factor as f32;

        let terminal_font = TerminalFont::from_cosmic_text(font, font_size)?;

        let terminal_size = DEFAULT_TERMINAL_SIZE;

        let padding_px = terminal_font.cell_size_px().0 / 2;

        let terminal_geometry = TerminalGeometry::new(terminal_font.cell_size_px(), terminal_size);

        let window_geometry =
            WindowGeometry::from_terminal_geometry(&terminal_geometry, scale_factor, padding_px);

        let inner_window_size: PhysicalSize<u32> = window_geometry.inner_size_px().into();

        let window = context.new_window(inner_window_size, None).await?;
        window.set_title(APPLICATION_NAME);

        let font_system = Arc::new(sync::Mutex::new(font_system));

        // Ergonomics: Camera::default() should probably create this one.
        let camera = {
            let fovy: f64 = 45.0;
            let camera_distance = 1.0 / (fovy / 2.0).to_radians().tan();
            Camera::new((0.0, 0.0, camera_distance), (0.0, 0.0, 0.0))
        };

        // Ergonomics: Why does the renderer need a camera here this early?
        let renderer = window
            .new_renderer(font_system.clone(), camera, inner_window_size)
            .await?;

        renderer.set_background_color(Some(Color::BLACK))?;

        // Use the native pty implementation for the system
        let pty_system = native_pty_system();

        // Create a new pty
        let pty_pair = pty_system.openpty(terminal_geometry.pty_size())?;

        let cmd = CommandBuilder::new_default_prog();

        let _child = pty_pair.slave.spawn_command(cmd)?;

        // No one knows here what and when anything blocks, so we create two channels for writing and on
        // for reading from the pty.
        // Send data to the pty by writing to the master

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

        let scene = Scene::new();

        let view_matrix = scene.stage(Matrix::identity());
        let view_location = scene.stage(Location {
            parent: None,
            matrix: view_matrix.clone(),
        });

        let view_params = TerminalViewParams {
            font_system: font_system.clone(),
            font: terminal_font.clone(),
            parent_location: view_location.clone(),
        };

        let terminal_scroller =
            TerminalScroller::new(&scene, Duration::from_secs(1), Duration::from_secs(1));

        let presenter = TerminalPresenter::new(
            terminal_geometry,
            terminal,
            view_params,
            last_rendered_seq_no,
            &scene,
        );

        Ok(Self {
            context,
            window,
            renderer,
            pty_pair,
            scene,
            view_matrix,
            event_manager: EventManager::default(),
            window_state: WindowState::new(window_geometry),
            presenter,
            terminal_scroller,
            selecting: None,
            clipboard: Clipboard::new()?,
        })
    }

    fn terminal(&self) -> &Arc<Mutex<Terminal>> {
        &self.presenter.terminal
    }

    async fn run(&mut self) -> Result<()> {
        let notify = Arc::new(Notify::new());
        // Read and parse output from the pty with reader
        let reader = self.pty_pair.master.try_clone_reader()?;

        let output_dispatcher =
            dispatch_output_to_terminal(reader, self.terminal().clone(), notify.clone());

        pin!(output_dispatcher);

        // Architecture: This is wrong. Need some way to query the current mouse pointer (from the
        // `WindowState`). Not only from events coming in.
        let mut mouse_pointer_on_view = None;

        loop {
            let shell_event_opt = select! {
                r = &mut output_dispatcher => {
                    info!("Shell output stopped. Exiting.");
                    return r;
                }
                _ = notify.notified() => {
                    None
                }
                shell_event = self.context.wait_for_shell_event(&mut self.renderer) => {
                    Some(shell_event?)
                }
            };

            // We have to process window events before going into the update cycle for now because
            // of the borrow checker.
            //
            // Detail: Animations starting here _are_ considered, but not updates.
            if let Some(shell_event) = &shell_event_opt
                && let Some(window_event) = shell_event.window_event_for(&self.window)
            {
                self.process_window_event(
                    self.window.id(),
                    window_event,
                    &mut mouse_pointer_on_view,
                )?;
            }

            // Performance: We begin an update cycle whenever the terminal advances, too. This
            // should probably be done asynchronously, deferred, etc. But note that the renderer is
            // also running asynchronously at the end of the update cycle.
            //
            // Architecture: We need to enforce running animations _inside_ the update cycle
            // somehow. Otherwise this can lead to confusing bugs, for example if the following code
            // does run before begin_update_cycle().
            let _cycle = self
                .scene
                .begin_update_cycle(&mut self.renderer, shell_event_opt.as_ref())?;

            // Idea: Make shell_event opaque and allow checking for animations update in UpdateCycle
            // that is returned from begin_update_cycle()?
            if matches!(shell_event_opt, Some(ShellEvent::ApplyAnimations)) {
                trace!("Applying animations");
                self.terminal_scroller.proceed();
            }

            {
                // Update lines, selection, and cursor.
                self.presenter
                    .update(&self.window_state, &self.scene, mouse_pointer_on_view)?;
            }

            // Center

            {
                let inner_size = self.window.inner_size();
                let center_transform = {
                    Matrix::from_translation(
                        (
                            -((inner_size.width / 2) as f64),
                            -((inner_size.height / 2) as f64),
                            0.0,
                        )
                            .into(),
                    )
                };

                self.view_matrix.update_if_changed(center_transform);
            }

            // Update mouse cursor shape.

            {
                let cursor_icon = if self.presenter.is_hyperlink_underlined_under_mouse() {
                    CursorIcon::Pointer
                } else {
                    CursorIcon::Default
                };
                self.window.set_cursor(cursor_icon);
            }
        }

        // Ok(())
    }

    // Robustness: May not end the terminal when this returns an error?
    // Architecture: Think about a general strategy about how to handle recoverable errors.
    fn process_window_event(
        &mut self,
        window_id: WindowId,
        window_event: &WindowEvent,
        mouse_pointer_on_view: &mut Option<PixelPoint>,
    ) -> Result<()> {
        let min_movement_distance = self.min_pixel_distance_considered_movement();

        let now = Instant::now();
        let ev = self.event_manager.event(ExternalEvent::from_window_event(
            window_id,
            window_event.clone(),
            now,
        ));

        let modifiers = ev.states().keyboard_modifiers();

        let window_pos_to_terminal_view = |p| {
            self.renderer
                .geometry()
                .unproject_to_model_z0(p, &self.view_matrix.value())
                .map(|p| (p.x, p.y).into())
        };

        // Process mouse pointer movements

        if let Some(pos) = ev.pos() {
            let mouse_pointer_pos = window_pos_to_terminal_view(pos);
            *mouse_pointer_on_view = mouse_pointer_pos;

            // Refresh implicit hyperlinks.
            //
            // Precision: This is asynchronous. The hit pos may be out of range, or somewhere else.
            // But good enough for now.
            if let Some(current_mouse_pos) = mouse_pointer_pos {
                let cell_pos = self
                    .presenter
                    .view_geometry()
                    .hit_test_cell(current_mouse_pos);
                self.presenter
                    .terminal
                    .lock()
                    .screen_mut()
                    .for_each_logical_line_in_stable_range_mut(
                        cell_pos.row.with_len(1),
                        |_, lines| {
                            Line::apply_hyperlink_rules(&config::DEFAULT_HYPERLINK_RULES, lines);
                            true
                        },
                    );
            }
        }

        // Process selecting user state

        match &mut self.selecting {
            None => match ev.detect_mouse_gesture(MouseButton::Left, min_movement_distance) {
                // WezTerm reacts on Click, macOS term on Clicked.
                Some(MouseGesture::Clicked(point)) => {
                    if let Some(view_px) = window_pos_to_terminal_view(point) {
                        let geometry = self.presenter.view_geometry();
                        let cell_pos = geometry.hit_test_cell(view_px);
                        if let Some(cell) =
                            geometry.get_cell(cell_pos, self.terminal().lock().screen_mut())
                            && let Some(hyperlink) = cell.attrs().hyperlink()
                            && let Err(e) = open_file_http_or_mailto_url(hyperlink.uri())
                        {
                            warn!("{e:?}");
                        }
                    }

                    self.presenter.selection_clear();
                }
                Some(MouseGesture::Movement(movement)) => {
                    if let Some(hit) = window_pos_to_terminal_view(movement.from) {
                        self.presenter.selection_begin(hit);
                        self.selecting = Some(movement);
                    }
                }
                _ => {}
            },
            Some(movement) => {
                if let Some(progress) = movement.track_to(&ev) {
                    let progress = progress.map_or_cancel(window_pos_to_terminal_view);

                    assert!(self.presenter.selection_can_progress());
                    self.presenter.selection_progress(&self.scene, progress);

                    if progress.ends() {
                        self.selecting = None;
                    }
                }
            }
        }

        // Process remaining events

        match window_event {
            WindowEvent::Resized(physical_size) => {
                self.resize((*physical_size).into())?;
            }
            WindowEvent::Focused(focused) => {
                // Architecture: Should we track the focused state of the window in the EventAggregator?
                self.window_state.focused = *focused;
                self.terminal().lock().focus_changed(*focused);
            }
            WindowEvent::MouseWheel {
                device_id: _,
                delta,
                phase: TouchPhase::Moved,
            } => {
                let delta_px = match delta {
                    MouseScrollDelta::LineDelta(_, delta) => {
                        (*delta as f64) * self.presenter.geometry().line_height_px() as f64
                    }
                    MouseScrollDelta::PixelDelta(physical_position) => physical_position.y,
                };

                self.presenter.scroll_delta_px(-delta_px)
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let Some((key, modifiers)) = input::termwiz::convert_key_event(event, modifiers)
                {
                    match event.state {
                        ElementState::Pressed => match key {
                            KeyCode::Char('c') if modifiers == KeyModifiers::SUPER => {
                                self.copy()?;
                            }
                            KeyCode::Char('v') if modifiers == KeyModifiers::SUPER => {
                                self.paste()?
                            }
                            _ => {
                                // Architecture: Should probably move into the presenter which owns terminal now.
                                self.terminal().lock().key_down(key, modifiers)?;
                                self.presenter.enable_autoscroll();
                            }
                        },
                        ElementState::Released => {
                            self.terminal().lock().key_up(key, modifiers)?;
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn resize(&mut self, new_window_size_px: (u32, u32)) -> Result<()> {
        // First the window.
        let suggested_terminal_size_px = self.window_state.geometry.resize(new_window_size_px);
        if self.presenter.resize(suggested_terminal_size_px)? {
            self.pty_pair
                .master
                .resize(self.presenter.geometry().pty_size())?;
        }

        Ok(())
    }

    fn min_pixel_distance_considered_movement(&self) -> f64 {
        const LOGICAL_POINTS_CONSIDERED_MOVEMENT: f64 = 5.0;
        LOGICAL_POINTS_CONSIDERED_MOVEMENT
            * self.context.primary_monitor_scale_factor().unwrap_or(1.0)
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

    /// Returns the selected text
    pub fn selected_text(&self) -> String {
        let mut s = String::new();
        // Feature: Rectangular selection.
        let rectangular = false;
        let Some(sel) = self.presenter.selection_range() else {
            return s;
        };
        let mut last_was_wrapped = false;
        let first_row = sel.stable_rows().start;
        let last_row = sel.stable_rows().end;

        let terminal = self.terminal().lock();

        for line in Self::get_logical_lines(&terminal, sel.stable_rows()) {
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

    fn get_logical_lines(terminal: &Terminal, lines: Range<StableRowIndex>) -> Vec<LogicalLine> {
        let mut logical_lines = Vec::new();

        terminal
            .screen()
            .for_each_logical_line_in_stable_range(lines, |stable_range, lines| {
                logical_lines.push(LogicalLine::from_physical_range(stable_range, lines));
                true
            });

        logical_lines
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

async fn dispatch_output_to_terminal(
    mut reader: impl io::Read + Send + 'static,
    terminal: Arc<Mutex<Terminal>>,
    notify: Arc<Notify>,
) -> Result<()> {
    // Using a thread does not make a difference here.
    let join_handle = task::spawn_blocking(move || {
        let mut buf = [0u8; 0x8000];
        loop {
            // Usually there are not more than 1024 bytes returned on macOS.
            match reader.read(&mut buf) {
                Ok(0) => {
                    return Ok(()); // EOF
                }
                Ok(bytes_read) => {
                    terminal.lock().advance_bytes(&buf[0..bytes_read]);
                    notify.notify_one();
                }
                Err(e) if e.kind() == ErrorKind::Interrupted => {
                    // Retry as recommended.
                }
                Err(e) => return Result::Err(e),
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
