use std::{
    collections::VecDeque,
    ops::Range,
    sync::{Arc, Mutex},
};

use anyhow::{Result, bail};
use cosmic_text::{
    Attrs, AttrsList, BufferLine, CacheKey, Family, FontSystem, LineEnding, Shaping, SubpixelBin,
    Wrap,
};
use termwiz::{
    cellcluster::CellCluster,
    surface::{CursorShape, CursorVisibility},
};
use wezterm_term::{CursorPosition, Intensity, Line, color::ColorPalette};

use massive_geometry::{Identity, Rect, Size};
use massive_scene::{Handle, Location, Matrix, Scene, Visual};
use massive_shapes::{GlyphRun, GlyphRunMetrics, RunGlyph, Shape, StrokeRect, TextWeight};

use crate::TerminalFont;

/// Panel is the representation of the terminal.
///
/// - It always contains a single [`Visual`] for each line. Even if this line is
///   currently not rendered.
/// - The coordinate system starts the left top (centering it may cause half-pixel).
/// - All lines use the same base "location" and translate
#[derive(Debug)]
pub struct Panel {
    font_system: Arc<Mutex<FontSystem>>,
    /// The terminal font.
    font: TerminalFont,
    color_palette: ColorPalette,

    /// The matrix all visuals are transformed with.
    ///
    /// This effectively moves all lines _only_ up.
    scroll_matrix: Handle<Matrix>,
    scroll_location: Handle<Location>,
    /// The number of lines with which _all_ lines are transformed upwards. If the view scrolls up and a new line
    /// comes in on the bottom, this increases.
    scroll_offset: isize,

    /// The visible lines. This contains _only_ the lines currently visible in the terminal.
    ///
    /// No lines for the scrollback. And if we are _inside_ the scrollback buffer, no lines below.
    ///
    /// VecDeque because we want to optimize them for scrolling.
    visible_lines: VecDeque<Handle<Visual>>,
    cursor: Option<Handle<Visual>>,
}

impl Panel {
    /// Create a new panel.
    ///
    /// Scene is needed to pre-create all the rows. This in turn prevents us from caring too much
    /// lazily creating them later, but may put a little more pressure on the renderer to filter out
    /// unused visuals.
    pub fn new(
        font_system: Arc<Mutex<FontSystem>>,
        font: TerminalFont,
        rows: usize,
        location: Handle<Location>,
        scene: &Scene,
    ) -> Self {
        let scroll_matrix = scene.stage(Matrix::identity());

        let scroll_location = scene.stage(Location {
            parent: Some(location),
            matrix: scroll_matrix.clone(),
        });

        let line_visuals = (0..rows)
            .map(|_| {
                scene.stage(Visual {
                    location: scroll_location.clone(),
                    shapes: [].into(),
                })
            })
            .collect();

        Self {
            font_system,
            font,
            color_palette: ColorPalette::default(),
            scroll_offset: 0,
            scroll_matrix,
            scroll_location,
            visible_lines: line_visuals,
            cursor: None,
        }
    }
}

// Cursor

impl Panel {
    pub fn update_cursor(&mut self, scene: &Scene, pos: CursorPosition, focused: bool) {
        match pos.visibility {
            CursorVisibility::Hidden => {
                self.cursor = None;
            }
            CursorVisibility::Visible => {
                let basic_shape = Self::basic_cursor_shape(pos.shape, focused);
                let shape = self.cursor_shape(basic_shape, pos);
                let visual = Visual::new(self.scroll_location.clone(), [shape]);
                self.cursor = Some(scene.stage(visual));
            }
        }
    }

    fn basic_cursor_shape(shape: CursorShape, focused: bool) -> BasicCursorShape {
        if !focused {
            return BasicCursorShape::Rect;
        }
        match shape {
            // Feature: Make default cursor configurable.
            CursorShape::Default => BasicCursorShape::Block,
            CursorShape::BlinkingBlock => BasicCursorShape::Block,
            CursorShape::SteadyBlock => BasicCursorShape::Block,
            CursorShape::BlinkingUnderline => BasicCursorShape::Underline,
            CursorShape::SteadyUnderline => BasicCursorShape::Underline,
            CursorShape::BlinkingBar => BasicCursorShape::Bar,
            CursorShape::SteadyBar => BasicCursorShape::Bar,
        }
    }

    fn cursor_shape(&self, shape: BasicCursorShape, pos: CursorPosition) -> Shape {
        let cursor_color = self.color_palette.cursor_bg;
        let cell_size = self.font.cell_size_px();
        let left = cell_size.0 * pos.x;
        // pos is screen relative, but we do attach the cursor visual to the scroll matrix, so have
        // to add scroll offset here.
        let top = cell_size.1 as f64 * (pos.y as f64 + self.scroll_offset as f64);

        // Feature: The size of the bar / underline should be derived from the font size / underline
        // position / thickness, not from the cell size.
        let stroke_thickness = ((cell_size.0 as f64 / 4.) + 1.).trunc();

        let rect = match shape {
            BasicCursorShape::Rect => {
                return StrokeRect::new(
                    Rect::new((left as _, top), (cell_size.0 as _, cell_size.1 as _)),
                    Size::new(stroke_thickness, stroke_thickness),
                    color::from_srgba(cursor_color),
                )
                .into();
            }
            BasicCursorShape::Block => {
                Rect::new((left as _, top as _), (cell_size.0 as _, cell_size.1 as _))
            }
            BasicCursorShape::Underline => Rect::new(
                (left as _, (top + self.font.ascender_px as f64) as _),
                (cell_size.0 as _, stroke_thickness),
            ),
            BasicCursorShape::Bar => {
                Rect::new((left as _, top as _), (stroke_thickness, cell_size.1 as _))
            }
        };

        massive_shapes::Rect::new(rect, color::from_srgba(cursor_color)).into()
    }
}

enum BasicCursorShape {
    Rect,
    Block,
    Underline,
    Bar,
}

// Lines

impl Panel {
    /// Scroll all lines by delta lines. Positive: moves all lines up, negative moves all lines down.
    /// This makes sure that empty lines are generated.
    pub fn scroll(&mut self, delta: isize) {
        match delta {
            0 => {
                return;
            }
            _ if delta < 0 => {
                todo!("Scrolling down is unsupported")
            }
            _ => self.scroll_up(delta as usize),
        }

        self.scroll_offset += delta;
        let new_y = -self.scroll_offset * self.font.cell_size_px().1 as isize;
        self.scroll_matrix
            .update(Matrix::from_translation((0., new_y as f64, 0.).into()));
    }

    fn scroll_up(&mut self, lines: usize) {
        if lines < self.rows() {
            self.visible_lines.rotate_left(lines);
        }
        let topmost_to_reset = self.rows().saturating_sub(lines);
        self.reset_lines(topmost_to_reset..self.rows());
    }

    fn reset_lines(&mut self, range: Range<usize>) {
        self.visible_lines.range_mut(range).for_each(|l| {
            // Performance: Only change if not already empty?
            l.update_with(|l| l.shapes = [].into());
        });
    }

    pub fn update_lines(
        &mut self,
        // Not used, we don't stage new objects here (yet!).
        _scene: &Scene,
        visual_line_index_top: usize,
        lines: &[&Line],
    ) -> Result<()> {
        let mut font_system = self.font_system.lock().unwrap();

        for (i, line) in lines.iter().enumerate() {
            let line_index = visual_line_index_top + i;
            let top =
                (self.scroll_offset + line_index as isize) * self.font.cell_size_px().1 as isize;
            let shapes = self.line_to_shapes(&mut font_system, top, line)?;
            self.visible_lines[line_index].update_with(|v| {
                v.shapes = shapes.into();
            });
        }

        Ok(())
    }

    fn line_to_shapes(
        &self,
        font_system: &mut FontSystem,
        top: isize,
        line: &Line,
    ) -> Result<Vec<Shape>> {
        // Production: Add bidi support
        let clusters = line.cluster(None);

        let mut shapes: Vec<Shape> = Vec::with_capacity(clusters.len());
        let mut left = 0;

        // Optimization: Combine clusters with compatible attributes. Colors and widths can vary
        // inside a GlyphRun.
        for cluster in clusters {
            let run = cluster_to_run(
                font_system,
                &self.font,
                &self.color_palette,
                (left, top),
                &cluster,
            )?;

            if let Some(run) = run {
                left += run.metrics.width as usize;
                shapes.push(run.into());
            }
        }

        Ok(shapes)
    }

    fn rows(&self) -> usize {
        self.visible_lines.len()
    }
}

fn cluster_to_run(
    font_system: &mut FontSystem,
    font: &TerminalFont,
    color_palette: &ColorPalette,
    (left, top): (usize, isize),
    cluster: &CellCluster,
) -> Result<Option<GlyphRun>> {
    let attributes = &cluster.attrs;

    // Performance: BufferLine makes a copy of the text, is there a better way?
    // Architecture: Should we shape all clusters in one co and prepare Attrs::metadata() accordingly?
    // Architecture: Under the hood, rustybuzz is used for text shaping, use it directly?
    // Performance: This contains internal caches, which might benefit reusing them.
    let mut buffer = BufferLine::new(
        &cluster.text,
        LineEnding::None,
        AttrsList::new(&Attrs::new().family(Family::Name(&font.family_name))),
        Shaping::Advanced,
    );

    let units_per_em_f = font.units_per_em as f32;

    // ADR: We lay out in em units so that positioning information can be processed and compared in
    // discrete units and perhaps even cached better.
    let lines = buffer.layout(font_system, units_per_em_f, None, Wrap::None, None, 0);
    let line = match lines.len() {
        0 => return Ok(None),
        1 => &lines[0],
        lines => {
            bail!("Expected to see only one line layouted: {lines}")
        }
    };

    // Cosmic text provides fractional positions, but we need to align every character directly on a
    // pixel grid, so start with 0 for now.
    //
    // Robustness: scale everything up so that while layout EM positions are used
    // to exactly map them to the pixel grid.

    let mut glyphs = Vec::with_capacity(line.glyphs.len());

    // Robustness: Shouldn't this be always equal the number of line glyphs?
    let mut cell_width = 0;

    for glyph in &line.glyphs {
        // Compute the discrete x offset and pixel position.
        // Robustness: Report unexpected variance here (> 0.001 ?)
        let glyph_index = (glyph.x / font.glyph_advance_em as f32).round() as usize;
        let glyph_index_width = (glyph.w / font.glyph_advance_em as f32).round() as usize;
        let glyph_x = glyph_index * font.glyph_advance_px;

        // Optimization: Compute this only once.
        cell_width = glyph_index + glyph_index_width;

        // Optimization: Don't pass empty glyphs.
        let glyph = RunGlyph {
            pos: (glyph_x as i32, 0),
            // Architecture: Introduce an internal CacheKey that does not use SubpixelBin (we won't
            // support that ever, because the author holds the belief that subpixel rendering is a scam)
            //
            // Architecture: Research if we would actually benefit from subpixel rendering in
            // inside a regular gray scale anti-alising setup.
            key: CacheKey {
                font_id: glyph.font_id,
                glyph_id: glyph.glyph_id,
                font_size_bits: font.size.to_bits(),
                x_bin: SubpixelBin::Zero,
                y_bin: SubpixelBin::Zero,
                flags: glyph.cache_key_flags,
            },
        };
        glyphs.push(glyph);
    }

    // Precision: Clarify what color profile we are actually using and document this in the massive Color.
    let fg_color = color_palette.resolve_fg(attributes.foreground());
    // Feature: Support a base weight.
    let weight = match attributes.intensity() {
        Intensity::Half => TextWeight::LIGHT,
        Intensity::Normal => TextWeight::NORMAL,
        Intensity::Bold => TextWeight::BOLD,
    };

    let run = GlyphRun {
        translation: (left as _, top as _, 0.).into(),
        metrics: GlyphRunMetrics {
            // Precision: compute this once for the font size so that it also matches the pixel cell
            // size.
            max_ascent: font.ascender_px as u32,
            max_descent: font.descender_px as u32,
            width: (cell_width * font.glyph_advance_px) as u32,
        },
        text_color: color::from_srgba(fg_color),
        text_weight: weight,
        glyphs,
    };

    Ok(Some(run))
}

mod color {
    use massive_geometry::Color;
    use termwiz::color::SrgbaTuple;

    pub fn from_srgba(SrgbaTuple(r, g, b, a): SrgbaTuple) -> Color {
        (r, g, b, a).into()
    }
}
