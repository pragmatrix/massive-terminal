use portable_pty::PtySize;

use crate::window_geometry::PixelPoint;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct TerminalGeometry {
    /// Cell size in physical pixels.
    pub cell_size_px: (u32, u32),

    /// Terminal size in cells (columns, rows).
    pub terminal_size: (usize, usize),
}

impl TerminalGeometry {
    pub fn new(cell_size: (u32, u32), terminal_cells: (usize, usize)) -> Self {
        Self {
            cell_size_px: cell_size,
            terminal_size: terminal_cells,
        }
    }

    pub fn resize_px(&mut self, terminal_inner_size: (u32, u32)) {
        let terminal_cells = (
            (terminal_inner_size.0 / self.cell_size_px.0) as usize,
            (terminal_inner_size.1 / self.cell_size_px.1) as usize,
        );
        let terminal_cells = (terminal_cells.0.max(1), terminal_cells.1.max(1));

        self.terminal_size = terminal_cells;
    }

    pub fn columns(&self) -> usize {
        self.terminal_size.0
    }

    pub fn rows(&self) -> usize {
        self.terminal_size.1
    }

    pub fn line_height_px(&self) -> u32 {
        self.cell_size_px.1
    }

    pub fn size_px(&self) -> (u32, u32) {
        (
            self.cell_size_px.0 * self.terminal_size.0 as u32,
            self.cell_size_px.1 * self.terminal_size.1 as u32,
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

    /// Decide if scrolling is needed and how many pixels the hit position lies away from.
    ///
    /// Negative values scroll up, positives, scroll down.
    pub fn scroll_distance(&self, view_hit: PixelPoint) -> Option<f64> {
        let hit_y = view_hit.y;

        if hit_y < 0.0 {
            return Some(view_hit.y);
        }

        let height = self.size_px().1 as f64;
        if hit_y > height {
            return Some(hit_y - height);
        }

        None
    }
}
