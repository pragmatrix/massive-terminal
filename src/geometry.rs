use portable_pty::PtySize;

// euclid definitions

pub struct CellUnit;

pub type CellRect = euclid::Rect<usize, CellUnit>;
pub type CellPoint = euclid::Point2D<usize, CellUnit>;

pub struct PixelUnit;

/// A point on a pixel coordinate system expressed in floats.
pub type PixelPoint = euclid::Point2D<f64, PixelUnit>;

#[derive(Debug)]
pub struct WindowGeometry {
    _scale_factor: f64,
    inner_size_px: (u32, u32),

    /// Padding around the terminal in physical pixels.
    padding_px: u32,

    // Architecture: Even thogh the WindowGeometry is built from the terminal's geometry, this does not belong here.
    pub terminal: TerminalGeometry,
}

impl WindowGeometry {
    pub fn new(scale_factor: f64, padding_px: u32, terminal: TerminalGeometry) -> Self {
        let (width, height) = terminal.size_px();
        let padding_2 = padding_px * 2;
        let inner_size_px = (width + padding_2, height + padding_2);

        Self {
            _scale_factor: scale_factor,
            inner_size_px,
            padding_px,
            terminal,
        }
    }

    pub fn inner_size_px(&self) -> (u32, u32) {
        self.inner_size_px
    }

    pub fn resize(&mut self, new_inner_size_px: (u32, u32)) {
        let padding_2 = self.padding_px * 2;
        let terminal_inner_size = (
            new_inner_size_px.0.saturating_sub(padding_2),
            new_inner_size_px.1.saturating_sub(padding_2),
        );
        self.terminal.resize(terminal_inner_size);
        self.inner_size_px = new_inner_size_px;
    }
}

#[derive(Debug)]
pub struct TerminalGeometry {
    /// Cell size in physical pixels.
    pub cell_size_px: (u32, u32),

    /// Terminal size in cells.
    pub terminal_cell_size: (usize, usize),
}

impl TerminalGeometry {
    pub fn new(cell_size: (u32, u32), terminal_cells: (usize, usize)) -> Self {
        Self {
            cell_size_px: cell_size,
            terminal_cell_size: terminal_cells,
        }
    }

    pub fn resize(&mut self, terminal_inner_size: (u32, u32)) {
        let terminal_cells = (
            (terminal_inner_size.0 / self.cell_size_px.0) as usize,
            (terminal_inner_size.1 / self.cell_size_px.1) as usize,
        );
        let terminal_cells = (terminal_cells.0.max(1), terminal_cells.1.max(1));

        self.terminal_cell_size = terminal_cells;
    }

    pub fn columns(&self) -> usize {
        self.terminal_cell_size.0
    }

    pub fn rows(&self) -> usize {
        self.terminal_cell_size.1
    }

    pub fn size_px(&self) -> (u32, u32) {
        (
            self.cell_size_px.0 * self.terminal_cell_size.0 as u32,
            self.cell_size_px.1 * self.terminal_cell_size.1 as u32,
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
            rows: self.rows(),
            cols: self.columns(),
            pixel_width: self.cell_size_px.0 as usize,
            pixel_height: self.cell_size_px.1 as usize,
            // Production: Set dpi
            ..wezterm_term::TerminalSize::default()
        }
    }
}
