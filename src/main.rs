use std::{
    io::{self, ErrorKind},
    sync::{Arc, Mutex},
};

use anyhow::{Result, anyhow};
use cosmic_text::{FontSystem, fontdb};
use derive_more::Debug;
use tokio::{pin, select, sync::Notify, task};
use tracing::info;
use winit::{
    dpi::PhysicalSize,
    event::{ElementState, MouseButton, WindowEvent},
};

use massive_geometry::{Camera, Color, Identity};
use massive_scene::{Handle, Location, Matrix, Scene};
use massive_shell::{ApplicationContext, AsyncWindowRenderer, ShellWindow, shell};
use portable_pty::{CommandBuilder, PtyPair, native_pty_system};
use terminal_state::TerminalState;
use wezterm_term::{Terminal, TerminalConfiguration, color};

mod geometry;
mod input;
mod panel;
mod selection;
mod terminal_font;
mod terminal_state;
mod window_state;

pub use panel::*;
pub use terminal_font::*;

use crate::{
    geometry::{TerminalGeometry, WindowGeometry},
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

    window_state: WindowState,
    terminal_state: TerminalState,
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
            println!("camera dist: {camera_distance}");
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
            window_state: WindowState::new(window_geometry),
            terminal_state: TerminalState::new(last_rendered_seq_no),
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
                self.process_window_event(window_event)?;
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

    fn process_window_event(&mut self, event: &WindowEvent) -> Result<()> {
        match event {
            WindowEvent::ActivationTokenDone { .. } => {}
            WindowEvent::Resized(physical_size) => {
                self.resize((*physical_size).into())?;
            }
            WindowEvent::Moved { .. } => {}
            WindowEvent::CloseRequested => {}
            WindowEvent::Destroyed => {}
            WindowEvent::DroppedFile(_) => {}
            WindowEvent::HoveredFile(_) => {}
            WindowEvent::HoveredFileCancelled => {}
            WindowEvent::Focused(focused) => {
                self.window_state.focused = *focused;
                self.terminal.lock().unwrap().focus_changed(*focused);
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let Some((key, modifiers)) =
                    input::convert_key_event(event, self.window_state.keyboard_modifiers.state())
                {
                    match event.state {
                        ElementState::Pressed => {
                            self.terminal.lock().unwrap().key_down(key, modifiers)?;
                        }
                        ElementState::Released => {
                            self.terminal.lock().unwrap().key_up(key, modifiers)?;
                        }
                    }
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.window_state.keyboard_modifiers = *modifiers;
            }
            WindowEvent::Ime(_) => {}
            WindowEvent::CursorMoved {
                device_id,
                position,
            } => {
                self.window_state.cursor_moved(*device_id, *position);
                self.hit_test((position.x, position.y));
            }
            WindowEvent::CursorEntered { device_id } => {
                self.window_state.cursor_entered(*device_id);
            }
            WindowEvent::CursorLeft { device_id } => {
                self.window_state.cursor_left(*device_id);
            }
            WindowEvent::MouseWheel { .. } => {}
            WindowEvent::MouseInput { button, state, .. } => {
                if *button == MouseButton::Left && *state == ElementState::Pressed {}
            }
            WindowEvent::PinchGesture { .. } => {}
            WindowEvent::PanGesture { .. } => {}
            WindowEvent::DoubleTapGesture { .. } => {}
            WindowEvent::RotationGesture { .. } => {}
            WindowEvent::TouchpadPressure { .. } => {}
            WindowEvent::AxisMotion { .. } => {}
            WindowEvent::Touch(_) => {}
            WindowEvent::ScaleFactorChanged { .. } => {}
            WindowEvent::ThemeChanged(_) => {}
            WindowEvent::Occluded(_) => {}
            WindowEvent::RedrawRequested => {}
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

    fn hit_test(&mut self, pos_px: (f64, f64)) -> Option<(usize, usize)> {
        // Prepare combined matrix once.
        let hit = self
            .renderer
            .geometry()
            .unproject_to_model(pos_px, &self.panel_matrix.value())?;
        println!("local hit: {hit:?}");

        // Map to cell coordinates
        let geometry = &self.window_state.geometry;
        let (cell_w, cell_h) = {
            let (cw, ch) = geometry.terminal.cell_size_px; // field access
            (cw as f64, ch as f64)
        };
        let (panel_px_w, panel_px_h) = geometry.inner_size_px();
        let (panel_px_w, panel_px_h) = (panel_px_w as f64, panel_px_h as f64);

        let (x, y) = (hit.x, hit.y);
        if x < 0.0 || y < 0.0 || x >= panel_px_w || y >= panel_px_h {
            return None;
        }
        if cell_w <= 0.0 || cell_h <= 0.0 {
            return None;
        }

        let col = (x / cell_w).floor() as usize;
        let row = (y / cell_h).floor() as usize;
        Some((col, row))
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
