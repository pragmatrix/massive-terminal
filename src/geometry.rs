use portable_pty::PtySize;

#[derive(Debug)]
pub struct WindowGeometry {
    pub scale_factor: f64,
    /// Padding around the panel in physcial pixels.
    pub padding_px: u32,

    pub terminal: TerminalGeometry,
}

impl WindowGeometry {
    pub fn new(scale_factor: f64, padding_px: u32, terminal: TerminalGeometry) -> Self {
        Self {
            scale_factor,
            padding_px,
            terminal,
        }
    }

    pub fn inner_size_px(&self) -> (u32, u32) {
        let (width, height) = self.terminal.size_px();
        let padding_2 = self.padding_px * 2;
        (width + padding_2, height + padding_2)
    }
}

#[derive(Debug)]
pub struct TerminalGeometry {
    /// Cell size in physcial pixels.
    pub cell_size_px: (u32, u32),

    /// Terminal size in cells.
    pub terminal_cell_size: (u32, u32),
}

impl TerminalGeometry {
    pub fn new(cell_size: (u32, u32), terminal_cells: (u32, u32)) -> Self {
        Self {
            cell_size_px: cell_size,
            terminal_cell_size: terminal_cells,
        }
    }

    pub fn columns(&self) -> u32 {
        self.terminal_cell_size.0
    }

    pub fn rows(&self) -> u32 {
        self.terminal_cell_size.1
    }

    pub fn size_px(&self) -> (u32, u32) {
        (
            self.cell_size_px.0 * self.terminal_cell_size.0,
            self.cell_size_px.1 * self.terminal_cell_size.1,
        )
    }

    pub fn pty_size(&self) -> PtySize {
        PtySize {
            rows: self.rows() as _,
            cols: self.columns() as _,
            // Robustness: is this physical or logical size, and what does a terminal actually do with it?
            pixel_width: self.cell_size_px.0 as _,
            pixel_height: self.cell_size_px.1 as _,
        }
    }

    pub fn wezterm_terminal_size(&self) -> wezterm_term::TerminalSize {
        wezterm_term::TerminalSize {
            rows: self.rows() as usize,
            cols: self.columns() as usize,
            pixel_width: self.cell_size_px.0 as usize,
            pixel_height: self.cell_size_px.1 as usize,
            // Production: Set dpi
            ..wezterm_term::TerminalSize::default()
        }
    }
}
