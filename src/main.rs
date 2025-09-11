use std::{
    io::{self, ErrorKind},
    ops::Range,
    sync::{Arc, Mutex},
    time::Instant,
};

use anyhow::{Result, anyhow};
use arboard::Clipboard;
use cosmic_text::{FontSystem, fontdb};
use derive_more::Debug;
use tokio::{pin, select, sync::Notify, task};
use tracing::info;
use winit::{
    dpi::PhysicalSize,
    event::{ElementState, MouseButton, WindowEvent},
    window::WindowId,
};

use portable_pty::{CommandBuilder, PtyPair, native_pty_system};
use terminal_state::TerminalState;
use wezterm_term::{KeyCode, KeyModifiers, StableRowIndex, Terminal, TerminalConfiguration, color};

use massive_geometry::{Camera, Color, Identity};
use massive_input::{EventManager, ExternalEvent, Movement};
use massive_scene::{Handle, Location, Matrix, Scene};
use massive_shell::{ApplicationContext, AsyncWindowRenderer, ShellWindow, shell};

mod geometry;
mod input;
mod logical_line;
mod panel;
mod selection;
mod terminal_font;
mod terminal_state;
mod window_state;

pub use panel::*;
pub use terminal_font::*;

use crate::{
    geometry::{PixelPoint, TerminalGeometry, WindowGeometry},
    logical_line::LogicalLine,
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

    #[debug(skip)]
    terminal: Arc<Mutex<Terminal>>,

    scene: Scene,
    panel: Panel,
    panel_matrix: Handle<Matrix>,

    event_manager: EventManager,

    window_state: WindowState,
    terminal_state: TerminalState,

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

        let window_geometry = WindowGeometry::new(
            scale_factor,
            padding_px,
            TerminalGeometry::new(terminal_font.cell_size_px(), terminal_size),
        );

        let inner_window_size: PhysicalSize<u32> = window_geometry.inner_size_px().into();

        let window = context.new_window(inner_window_size, None).await?;
        window.set_title(APPLICATION_NAME);

        let font_system = Arc::new(Mutex::new(font_system));

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
        let pty_pair = pty_system.openpty(window_geometry.terminal.pty_size())?;

        let cmd = CommandBuilder::new_default_prog();

        let _child = pty_pair.slave.spawn_command(cmd)?;

        // Noone knows here what and when anything blocks, so we create two channels for writing and on
        // for reading from the pty.
        // Send data to the pty by writing to the master

        let writer = pty_pair.master.take_writer()?;

        let configuration = MassiveTerminalConfiguration {};

        let terminal = Terminal::new(
            window_geometry.terminal.wezterm_terminal_size(),
            Arc::new(configuration),
            TERMINAL_NAME,
            TERMINAL_VERSION,
            writer,
        );
        let last_rendered_seq_no = terminal.current_seqno();
        let terminal = Arc::new(Mutex::new(terminal));

        let scene = Scene::new();

        let panel_matrix = scene.stage(Matrix::identity());
        let panel_location = scene.stage(Location {
            parent: None,
            matrix: panel_matrix.clone(),
        });

        let panel = Panel::new(
            font_system,
            terminal_font,
            window_geometry.terminal.rows(),
            panel_location,
            &scene,
        );

        Ok(Self {
            context,
            window,
            renderer,
            pty_pair,
            terminal,
            scene,
            panel,
            panel_matrix,
            event_manager: EventManager::default(),
            window_state: WindowState::new(window_geometry),
            terminal_state: TerminalState::new(last_rendered_seq_no),
            selecting: None,
            clipboard: Clipboard::new()?,
        })
    }

    async fn run(&mut self) -> Result<()> {
        let notify = Arc::new(Notify::new());
        // Read and parse output from the pty with reader
        let reader = self.pty_pair.master.try_clone_reader()?;

        let output_dispatcher =
            dispatch_output_to_terminal(reader, self.terminal.clone(), notify.clone());

        pin!(output_dispatcher);

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

            if let Some(event) = &shell_event_opt
                && let Some(window_event) = event.window_event_for(&self.window)
            {
                self.process_window_event(self.window.id(), window_event)?;
            }

            // Performance: We begin an update cycle whenever the terminal advances. This should
            // probably be done asynchronously, deferred, etc. But note that the renderer is also
            // running asynchronously at the end of the update cycle.
            let _cycle = self.context.begin_update_cycle(
                &self.scene,
                &mut self.renderer,
                shell_event_opt.as_ref(),
            )?;

            {
                // Update lines & cursor

                self.terminal_state.update(
                    &self.terminal,
                    &self.window_state,
                    &mut self.panel,
                    &self.scene,
                )?;
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

                self.panel_matrix.update_if_changed(center_transform);
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
    ) -> Result<()> {
        let min_movement_distance = self.min_pixel_distance_considered_movement();

        let now = Instant::now();
        let ev = self.event_manager.event(ExternalEvent::from_window_event(
            window_id,
            window_event.clone(),
            now,
        ));

        let modifiers = ev.states().keyboard_modifiers();

        // Process selecting user state

        if let Some(selecting) = &mut self.selecting {
            if let Some(progress) = selecting.track_to(&ev)
                && let Some(progress) = progress.try_map(|to| {
                    let panel_hit = self.hit_test_panel((to.x, to.y).into())?;
                    self.panel_to_cell(panel_hit)
                })
            {
                assert!(self.terminal_state.selection_can_progress());
                self.terminal_state.selection_progress(progress);
                if progress.ends() {
                    self.selecting = None;
                }
            }
        } else if let Some(movement) = ev.detect_movement(MouseButton::Left, min_movement_distance)
        {
            let from = movement.from;
            if let Some(panel_hit) = self.hit_test_panel((from.x, from.y).into())
                && let Some(cell_hit) = self.panel_to_cell(panel_hit)
            {
                self.terminal_state.selection_begin(cell_hit);
                self.selecting = Some(movement);
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
                self.terminal.lock().unwrap().focus_changed(*focused);
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
                                self.terminal.lock().unwrap().key_down(key, modifiers)?;
                            }
                        },
                        ElementState::Released => {
                            self.terminal.lock().unwrap().key_up(key, modifiers)?;
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn resize(&mut self, new_window_size_px: (u32, u32)) -> Result<()> {
        let current_size = self.window_state.geometry.terminal.terminal_cell_size;

        // First the geometry.
        self.window_state.geometry.resize(new_window_size_px);

        if self.window_state.geometry.terminal.terminal_cell_size == current_size {
            return Ok(());
        }

        // Then we go bottom up.
        let terminal = &self.window_state.terminal;

        self.pty_pair.master.resize(terminal.pty_size())?;

        self.terminal
            .lock()
            .unwrap()
            .resize(terminal.wezterm_terminal_size());

        self.panel.resize(terminal.rows(), &self.scene);

        Ok(())
    }

    fn hit_test_panel(&mut self, pos_px: PixelPoint) -> Option<PixelPoint> {
        let hit = self
            .renderer
            .geometry()
            .unproject_to_model_z0(pos_px.into(), &self.panel_matrix.value())?;

        Some((hit.x, hit.y).into())
    }

    /// The cell for a particular pixel coordinate in the pixel space of the panel.
    fn panel_to_cell(&self, panel_px: PixelPoint) -> Option<(usize, usize)> {
        let (x, y) = panel_px.into();

        let geometry = &self.window_state.geometry;
        let (panel_px_w, panel_px_h) = geometry.inner_size_px();
        let (panel_px_w, panel_px_h) = (panel_px_w as f64, panel_px_h as f64);

        if x < 0.0 || y < 0.0 || x >= panel_px_w || y >= panel_px_h {
            return None;
        }

        let (cell_w, cell_h) = {
            let (cw, ch) = geometry.terminal.cell_size_px; // field access
            (cw as f64, ch as f64)
        };

        if cell_w <= 0.0 || cell_h <= 0.0 {
            return None;
        }

        let col = (x / cell_w).floor() as usize;
        let row = (y / cell_h).floor() as usize;
        Some((col, row))
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
            self.terminal.lock().unwrap().send_paste(&text)?;
        }
        Ok(())
    }
}

// Selection

impl MassiveTerminal {
    // Copied from wezterm_gui/src/terminwindow/selection.rs

    /// Returns the selected text
    pub fn selected_text(&self) -> String {
        let mut s = String::new();
        // Feature: Rectangular selection.
        let rectangular = false;
        let Some(sel) = self.terminal_state.selection().range() else {
            return s;
        };
        let mut last_was_wrapped = false;
        let first_row = sel.rows().start;
        let last_row = sel.rows().end;

        let terminal = self.terminal.lock().unwrap();

        for line in Self::get_logical_lines(&terminal, sel.rows()) {
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
                    terminal.lock().unwrap().advance_bytes(&buf[0..bytes_read]);
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
