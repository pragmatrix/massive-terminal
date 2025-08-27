use std::{
    io::{self, ErrorKind},
    sync::{Arc, Mutex},
};

use anyhow::anyhow;
use anyhow::{Result, bail};
use cosmic_text::{FontSystem, fontdb};
use derive_more::Debug;
use massive_geometry::{Camera, Color, Identity};
use massive_scene::{Handle, Location, Matrix, Scene};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use rangeset::RangeSet;
use termwiz::surface::SequenceNo;
use tokio::{
    select,
    sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    task,
};
use tracing::error;

use massive_shell::{ApplicationContext, AsyncWindowRenderer, RendererMessage, ShellWindow, shell};
use wezterm_term::{Terminal, TerminalConfiguration, TerminalSize, color};
use winit::{
    dpi::PhysicalSize,
    event::{ElementState, Modifiers, WindowEvent},
};

mod input;
mod panel;
mod terminal_font;

pub use panel::*;
pub use terminal_font::*;

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
    shell_output: UnboundedReceiver<Result<Vec<u8>>>,
    #[debug(skip)]
    terminal: Terminal,
    last_rendered_seq_no: SequenceNo,

    scene: Scene,
    panel: Panel,
    panel_matrix: Handle<Matrix>,
    window_state: WindowState,
}

#[derive(Default, Debug)]
struct WindowState {
    focused: bool,
    keyboard_modifiers: Modifiers,
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

        let font_size =
            DEFAULT_FONT_SIZE * context.primary_monitor_scale_factor().unwrap_or_default() as f32;

        let terminal_font = TerminalFont::from_cosmic_text(font, font_size)?;

        let cell_pixel_size = terminal_font.cell_size_px();

        // Research: Why trunc() and not ceil() or round()?
        let terminal_size = DEFAULT_TERMINAL_SIZE;

        let inner_window_size = (
            cell_pixel_size.0 * terminal_size.0,
            cell_pixel_size.1 * terminal_size.1,
        );

        let window = context
            .new_window(
                PhysicalSize::new(inner_window_size.0 as u32, inner_window_size.1 as _),
                None,
            )
            .await?;
        window.set_title(APPLICATION_NAME);

        let font_system = Arc::new(Mutex::new(font_system));

        // Ergonomics: Camera::default() should create this one.
        let camera = {
            let fovy: f64 = 45.0;
            let camera_distance = 1.0 / (fovy / 2.0).to_radians().tan();
            Camera::new((0.0, 0.0, camera_distance), (0.0, 0.0, 0.0))
        };

        // Ergonomics: Why does the renderer need a camera here this early?
        let renderer = window
            .new_renderer(
                font_system.clone(),
                camera,
                (inner_window_size.0 as u32, inner_window_size.1 as _),
            )
            .await?;

        // Ergonomics: Feels weird sending a message to set the background color.
        renderer.post_msg(RendererMessage::SetBackgroundColor(Some(Color::BLACK)))?;

        // Use the native pty implementation for the system
        let pty_system = native_pty_system();

        let (columns, rows) = terminal_size;

        // Create a new pty
        let pair = pty_system.openpty(PtySize {
            rows: rows as _,
            cols: columns as _,
            // Robustness: is this physical or logical size, and what does a terminal actually do with it?
            pixel_width: cell_pixel_size.0 as _,
            pixel_height: cell_pixel_size.1 as _,
        })?;

        let cmd = CommandBuilder::new_default_prog();

        let _child = pair.slave.spawn_command(cmd)?;

        // Read and parse output from the pty with reader
        let reader = pair.master.try_clone_reader()?;
        let shell_output = read_to_receiver(reader);

        // Noone knows here what and when anything blocks, so we create two channels for writing and on
        // for reading from the pty.
        // Send data to the pty by writing to the master

        let writer = pair.master.take_writer()?;
        // thread::spawn(move || {
        //     writeln!(writer, "ls -l").unwrap();
        //     // writer.flush().unwrap();
        //     // drop(writer);
        //     #[allow(clippy::empty_loop)]
        //     loop {}
        // });

        let configuration = MassiveTerminalConfiguration {};

        let terminal = Terminal::new(
            // Production: Set dpi
            TerminalSize {
                rows,
                cols: columns,
                pixel_width: cell_pixel_size.0,
                pixel_height: cell_pixel_size.1,
                ..TerminalSize::default()
            },
            Arc::new(configuration),
            TERMINAL_NAME,
            TERMINAL_VERSION,
            writer,
        );
        let last_rendered_seq_no = terminal.current_seqno();

        let scene = Scene::new();

        let panel_matrix = scene.stage(Matrix::identity());
        let panel_location = scene.stage(Location {
            parent: None,
            matrix: panel_matrix.clone(),
        });

        let panel = Panel::new(
            font_system,
            terminal_font,
            terminal_size.1,
            panel_location,
            &scene,
        );

        Ok(Self {
            context,
            window,
            renderer,
            shell_output,
            terminal,
            last_rendered_seq_no,
            scene,
            panel,
            panel_matrix,
            window_state: WindowState::default(),
        })
    }

    async fn run(&mut self) -> Result<()> {
        loop {
            let shell_event_opt = select! {
                output = self.shell_output.recv() => {
                    let Some(Ok(output)) = output else {
                        bail!("Shell output stopped");
                    };

                    self.terminal.advance_bytes(output);
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

            // Performance: No need to begin an update cycle if there are no visible changes
            let update_lines = {
                let current_seq_no = self.terminal.current_seqno();
                assert!(current_seq_no >= self.last_rendered_seq_no);
                current_seq_no > self.last_rendered_seq_no
            };

            if update_lines {
                let screen = self.terminal.screen();
                let stable_top = screen.visible_row_to_stable_row(0);
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

                    let mut r = Ok(());

                    screen.with_phys_lines(phys_range, |lines| {
                        r = self.panel.update_lines(
                            &self.scene,
                            visible_range_start as usize,
                            lines,
                        );
                    });

                    r?;
                }

                // Commit

                self.last_rendered_seq_no = self.terminal.current_seqno()
            }

            // Cursor

            {
                let pos = self.terminal.cursor_pos();
                self.panel
                    .update_cursor(&self.scene, pos, self.window_state.focused);
            }

            // Center

            {
                let page_size = self.window.inner_size();
                let center_transform = {
                    Matrix::from_translation(
                        (
                            -((page_size.width / 2) as f64),
                            -((page_size.height / 2) as f64),
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
            WindowEvent::Resized { .. } => {}
            WindowEvent::Moved { .. } => {}
            WindowEvent::CloseRequested => {}
            WindowEvent::Destroyed => {}
            WindowEvent::DroppedFile(_) => {}
            WindowEvent::HoveredFile(_) => {}
            WindowEvent::HoveredFileCancelled => {}
            WindowEvent::Focused(focused) => {
                self.window_state.focused = *focused;
                self.terminal.focus_changed(*focused);
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let Some((key, modifiers)) =
                    input::convert_key_event(event, self.window_state.keyboard_modifiers.state())
                {
                    match event.state {
                        ElementState::Pressed => {
                            self.terminal.key_down(key, modifiers)?;
                        }
                        ElementState::Released => {
                            self.terminal.key_up(key, modifiers)?;
                        }
                    }
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.window_state.keyboard_modifiers = *modifiers;
            }
            WindowEvent::Ime(_) => {}
            WindowEvent::CursorMoved { .. } => {}
            WindowEvent::CursorEntered { .. } => {}
            WindowEvent::CursorLeft { .. } => {}
            WindowEvent::MouseWheel { .. } => {}
            WindowEvent::MouseInput { .. } => {}
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
}

#[derive(Debug)]
struct MassiveTerminalConfiguration {}

impl TerminalConfiguration for MassiveTerminalConfiguration {
    fn color_palette(&self) -> color::ColorPalette {
        // Production: Review.
        color::ColorPalette::default()
    }
}

fn read_to_receiver(reader: impl io::Read + Send + 'static) -> UnboundedReceiver<Result<Vec<u8>>> {
    let (tx, rx) = unbounded_channel();

    task::spawn_blocking(move || {
        if let Err(e) = lp(reader, tx) {
            error!("Reader ended unexpectedly: {e:?}")
        }

        fn lp(
            mut reader: impl io::Read + Send + 'static,
            tx: UnboundedSender<Result<Vec<u8>>>,
        ) -> Result<()> {
            let mut buf = [0u8; 0x8000];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        return Ok(()); // EOF
                    }
                    Ok(bytes_read) => {
                        tx.send(Ok(buf[0..bytes_read].to_vec()))?;
                    }
                    Err(e) if e.kind() == ErrorKind::Interrupted => {
                        // as suggested, retry.
                    }
                    Err(e) => {
                        tx.send(Err(e.into()))?;
                        bail!("Reader ended because of an error the receiver must handle.")
                    }
                }
            }
        }
    });

    rx
}
