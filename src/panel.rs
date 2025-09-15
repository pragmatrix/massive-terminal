use std::{
    collections::VecDeque,
    iter,
    ops::Range,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Result, bail};
use cosmic_text::{
    Attrs, AttrsList, BufferLine, CacheKey, Family, FontSystem, LineEnding, Shaping, SubpixelBin,
    Wrap,
};
use log::{info, trace};
use massive_animation::{Interpolation, Timeline};
use rangeset::RangeSet;
use tuple::Map;

use termwiz::{
    cellcluster::CellCluster,
    color::ColorAttribute,
    surface::{CursorShape, CursorVisibility},
};
use wezterm_term::{CursorPosition, Intensity, Line, StableRowIndex, color::ColorPalette};

use massive_geometry::{Identity, Point, Rect, Size};
use massive_scene::{Handle, Location, Matrix, Scene, Visual};
use massive_shapes::{GlyphRun, GlyphRunMetrics, RunGlyph, Shape, StrokeRect, TextWeight};

use crate::{
    TerminalFont,
    selection::{NormalizedSelectionRange, SelectionRange},
    terminal_geometry::TerminalGeometry,
    window_geometry::CellRect,
};

const SCROLL_DURATION: Duration = Duration::from_millis(100);

/// Panel is the (massive) representation of the terminal.
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

    /// Positive scroll offset of the screen.
    screen_scroll_offset: StableRowIndex,

    /// The number of pixels with which _all_ lines are transformed upwards.
    ///
    /// When the view scrolls up and a new line comes in on the bottom, this value increases.
    ///
    /// Never negative
    scroll_offset_px: Timeline<f64>,

    /// The first line's stable index in lines.
    first_line_stable_index: StableRowIndex,

    /// The visible lines. This contains _only_ the lines currently visible in the terminal.
    ///
    /// No lines for the scrollback. And if we are _inside_ the scrollback buffer, no lines below.
    ///
    /// VecDeque because we want to optimize them for scrolling.
    lines: VecDeque<Handle<Visual>>,
    cursor: Option<Handle<Visual>>,
    selection: Option<Handle<Visual>>,
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
        scroll_offset_px: Timeline<f64>,
        location: Handle<Location>,
        scene: &Scene,
    ) -> Self {
        let scroll_matrix = scene.stage(Matrix::identity());

        let scroll_location = scene.stage(Location {
            parent: Some(location),
            matrix: scroll_matrix.clone(),
        });

        Self {
            font_system,
            font,
            color_palette: ColorPalette::default(),
            screen_scroll_offset: 0,
            scroll_offset_px,
            scroll_matrix,
            scroll_location,
            first_line_stable_index: 0,
            lines: VecDeque::new(),
            cursor: None,
            selection: None,
        }
    }
}

// Lines

impl Panel {
    /// Scroll all lines by delta lines.
    ///
    /// Positive: moves all lines up, negative moves all lines down. This makes sure that empty
    /// lines are generated.
    pub fn scroll(&mut self, delta_lines: isize) {
        if delta_lines == 0 {
            return;
        }

        self.screen_scroll_offset += delta_lines;
        assert!(self.screen_scroll_offset >= 0);

        self.scroll_offset_px.animate_to(
            self.scroll_offset_px() as f64,
            SCROLL_DURATION,
            Interpolation::CubicOut,
        );
    }

    fn scroll_offset_px(&self) -> i64 {
        self.screen_scroll_offset as i64 * self.line_height_px()
    }

    fn line_height_px(&self) -> i64 {
        self.font.cell_size_px().1 as i64
    }

    /// Update currently running animations.
    pub fn apply_animations(&mut self) {
        // Detail: Even if the timeline is not anymore animating, we might not have retrieved and
        // update the latest value yet.

        // Round to the nearest pixel, otherwise animated frames would not be pixel perfect.
        let scroll_offset_px = self.scroll_offset_px.value().round();
        trace!(
            "Updating scroll offset: {scroll_offset_px} (apx line: {})",
            scroll_offset_px / self.line_height_px() as f64
        );
        self.scroll_matrix
            .update(Matrix::from_translation((0., -scroll_offset_px, 0.).into()));
    }

    /// Return the stable range of all the lines we need to update for the stable_range given in the
    /// screen taking the current animation into account.
    pub fn view_range(&self, rows: usize) -> Range<StableRowIndex> {
        assert!(rows >= 1);

        // First pixel visible inside the screen viewed.
        let line_height_px = self.font.cell_size_px().1 as i64;
        let animated_scroll_offset_px = self.scroll_offset_px.value();

        let topmost_pixel_line_visible = animated_scroll_offset_px.trunc() as i64;
        // -1 because we want to hit the line the pixel is on and don't render more than row cells
        // if animations are done.
        let bottom_pixel_line_visible =
            (topmost_pixel_line_visible + line_height_px * rows as i64) - 1;

        let topmost_stable_render_line = topmost_pixel_line_visible / line_height_px;
        let bottom_stable_render_line = bottom_pixel_line_visible / line_height_px;
        assert!(bottom_stable_render_line >= topmost_stable_render_line);

        topmost_stable_render_line as isize..(bottom_stable_render_line + 1) as isize
    }

    /// This is the first step before lines can be updated. Reset the view range.
    ///
    /// This returns a set of stable index ranges that are _requied_ to be updated together with the changed ones in the view_range.
    ///
    /// This view_range is the one returned from `view_range()`.
    ///
    /// Architecture: If nothing had changed, we already know the view range.
    pub fn update_view_range(
        &mut self,
        scene: &Scene,
        view_range: Range<StableRowIndex>,
    ) -> RangeSet<StableRowIndex> {
        let mut required_line_updates = RangeSet::new();

        assert!(view_range.end > view_range.start);
        let current_range =
            self.first_line_stable_index..self.first_line_stable_index + self.lines.len() as isize;

        let new_visual = || scene.stage(Visual::new(self.scroll_location.clone(), []));

        if view_range.end <= current_range.start || view_range.start >= current_range.end {
            // Non-overlapping: reset completely.
            self.first_line_stable_index = view_range.start;
            self.lines = iter::repeat_with(new_visual)
                .take(view_range.len())
                .collect();
            required_line_updates.add_range(view_range);
            return required_line_updates;
        }

        // Overlapping.

        // Positive movement: down, negative: up
        let top_delta = view_range.start - current_range.start;
        match top_delta {
            _ if top_delta > 0 => {
                let to_remove = top_delta;
                self.lines.drain(0..(to_remove as usize));
                self.first_line_stable_index += to_remove;
            }
            _ if top_delta < 0 => {
                let to_add = -top_delta;
                for _ in 0..to_add {
                    self.lines.push_front(new_visual());
                }
                self.first_line_stable_index -= to_add;
                required_line_updates.add_range(view_range.start..current_range.start);
            }
            _ => {}
        }

        let bottom_delta = view_range.end - current_range.end;
        match bottom_delta {
            _ if bottom_delta > 0 => {
                let to_add = bottom_delta as usize;
                self.lines
                    .extend(iter::repeat_with(new_visual).take(to_add));
                required_line_updates.add_range(current_range.end..view_range.end);
            }
            _ if bottom_delta < 0 => {
                let to_remove = (-bottom_delta) as usize;
                self.lines.truncate(self.lines.len() - to_remove);
            }
            _ => {}
        }

        assert_eq!(
            view_range,
            self.first_line_stable_index..self.first_line_stable_index + self.lines.len() as isize
        );

        required_line_updates
    }

    pub fn update_lines(
        &mut self,
        first_line_stable_index: StableRowIndex,
        lines: &[Line],
    ) -> Result<()> {
        let update_range = first_line_stable_index..first_line_stable_index + lines.len() as isize;
        let lines_range =
            self.first_line_stable_index..self.first_line_stable_index + self.lines.len() as isize;
        if update_range.start < lines_range.start || update_range.end > lines_range.end {
            bail!("Internal error: Updated lines {update_range:?} is not inside {lines_range:?}");
        }

        let line_height_px = self.line_height_px();

        for (i, line) in lines.iter().enumerate() {
            // Place shapes at their stable (non-animated) vertical position.
            let top = (first_line_stable_index + i as isize) as i64 * line_height_px;
            let shapes = {
                // Lock the font_system for the least amount of time possible. This is shared with
                // the renderer.
                let mut font_system = self.font_system.lock().unwrap();
                self.line_to_shapes(&mut font_system, top, line)?
            };

            let line_index =
                (update_range.start - self.first_line_stable_index + i as isize) as usize;

            self.lines[line_index].update_with(|v| {
                v.shapes = shapes.into();
            });
        }

        Ok(())
    }

    fn line_to_shapes(
        &self,
        font_system: &mut FontSystem,
        top: i64,
        line: &Line,
    ) -> Result<Vec<Shape>> {
        // Production: Add bidi support
        let clusters = line.cluster(None);

        // Performance: Background shapes are not included in the capacity.
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

            let background =
                cluster_background(&cluster, &self.font, &self.color_palette, (left, top));

            if let Some(run) = run {
                left += cluster.width as i64 * self.font.cell_size_px().0 as i64;
                shapes.push(run.into());
            }

            if let Some(background) = background {
                shapes.push(background)
            }
        }

        Ok(shapes)
    }
}

fn cluster_to_run(
    font_system: &mut FontSystem,
    font: &TerminalFont,
    color_palette: &ColorPalette,
    (left, top): (i64, i64),
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
        let glyph_index = (glyph.x / font.glyph_advance_em as f32).round() as u32;
        let glyph_index_width = (glyph.w / font.glyph_advance_em as f32).round() as u32;
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
            max_ascent: font.ascender_px,
            max_descent: font.descender_px,
            width: (cell_width * font.glyph_advance_px),
        },
        text_color: color::from_srgba(fg_color),
        text_weight: weight,
        glyphs,
    };

    Ok(Some(run))
}

/// Retrieves the background shape from the cluster.
fn cluster_background(
    cluster: &CellCluster,
    font: &TerminalFont,
    color_palette: &ColorPalette,
    (left, top): (i64, i64),
) -> Option<Shape> {
    let background = cluster.attrs.background();
    // We assume the background is rendered in the default background color.
    if background == ColorAttribute::Default {
        return None;
    }
    let background = color::from_srgba(color_palette.resolve_bg(background));

    let size: Size = (
        // Precision: We keep multiplication in the u32 range here. Unlikely it's breaking out.
        (cluster.width as u32 * font.cell_size_px().0) as f64,
        font.cell_size_px().1 as f64,
    )
        .into();

    let lt: Point = (left as f64, top as f64).into();

    Some(massive_shapes::Rect::new(Rect::new(lt, size), background).into())
}

// Cursor

enum BasicCursorShape {
    Rect,
    Block,
    Underline,
    Bar,
}

// Cursor

impl Panel {
    pub fn update_cursor(&mut self, scene: &Scene, pos: CursorPosition, window_focused: bool) {
        match pos.visibility {
            CursorVisibility::Hidden => {
                self.cursor = None;
            }
            CursorVisibility::Visible => {
                let basic_shape = Self::basic_cursor_shape(pos.shape, window_focused);
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
        let left = cell_size.0 * pos.x as u32;
        // pos is screen relative, but we do attach the cursor visual to the scroll matrix, so have
        // to add scroll offset here.
        // Precision: This may get very large, so u64.
        let top = self.scroll_offset_px.final_value().round() as u64
            + (cell_size.1 as u64 * pos.y as u64);

        // Feature: The size of the bar / underline should be derived from the font size / underline
        // position / thickness, not from the cell size.
        let stroke_thickness = ((cell_size.0 as f64 / 4.) + 1.).trunc();

        let rect = match shape {
            BasicCursorShape::Rect => {
                return StrokeRect::new(
                    Rect::new((left as _, top as _), (cell_size.0 as _, cell_size.1 as _)),
                    Size::new(stroke_thickness, stroke_thickness),
                    color::from_srgba(cursor_color),
                )
                .into();
            }
            BasicCursorShape::Block => {
                Rect::new((left as _, top as _), (cell_size.0 as _, cell_size.1 as _))
            }
            BasicCursorShape::Underline => Rect::new(
                (
                    left as _,
                    ((top + self.font.ascender_px as u64) as f64) as _,
                ),
                (cell_size.0 as _, stroke_thickness),
            ),
            BasicCursorShape::Bar => {
                Rect::new((left as _, top as _), (stroke_thickness, cell_size.1 as _))
            }
        };

        massive_shapes::Rect::new(rect, color::from_srgba(cursor_color)).into()
    }
}

// Selection

impl Panel {
    pub fn update_selection(
        &mut self,
        scene: &Scene,
        selection: Option<NormalizedSelectionRange>,
        terminal_geometry: &TerminalGeometry,
    ) {
        match selection {
            Some(selection) => {
                let rects_stable = Self::selection_rects(&selection, terminal_geometry.columns());
                let cell_size = terminal_geometry.cell_size_px.map(f64::from);

                let rects_final = rects_stable.iter().map(|r| {
                    r.to_f64().scale(cell_size.0, cell_size.1).translate(
                        (
                            0.,
                            self.scroll_offset_px.final_value().round() * cell_size.1,
                        )
                            .into(),
                    )
                });

                let selection_color = color::from_srgba(self.color_palette.selection_bg);

                let shapes: Vec<_> = rects_final
                    .map(|r| massive_shapes::Rect::new(r, selection_color).into())
                    .collect();

                let visual = Visual::new(self.scroll_location.clone(), shapes);

                //
                match &mut self.selection {
                    Some(selection) => {
                        selection.update_if_changed(visual);
                    }
                    None => self.selection = Some(scene.stage(visual)),
                }
            }
            None => self.selection = None,
        }
    }

    /// A selection can be rendered in one to three rectangles.
    fn selection_rects(selection: &SelectionRange, terminal_columns: usize) -> Vec<CellRect> {
        debug_assert!(selection.end >= selection.start);
        let start_point = selection.start.point();
        let end_point = selection.end.point();

        let lines_covering = end_point.y + 1 - start_point.y;
        debug_assert!(lines_covering > 0);

        if lines_covering == 1 {
            return vec![CellRect::new(
                start_point,
                (end_point.x - start_point.x, 1).into(),
            )];
        }

        let top_line = CellRect::new(start_point, (terminal_columns - start_point.x, 1).into());

        let bottom_line = CellRect::new((0, end_point.y).into(), (end_point.x + 1, 1).into());

        if lines_covering == 2 {
            return vec![top_line, bottom_line];
        }

        vec![
            top_line,
            CellRect::new(
                (0, start_point.y + 1).into(),
                (terminal_columns, lines_covering - 2).into(),
            ),
            bottom_line,
        ]
    }
}

mod color {
    use massive_geometry::Color;
    use termwiz::color::SrgbaTuple;

    pub fn from_srgba(SrgbaTuple(r, g, b, a): SrgbaTuple) -> Color {
        (r, g, b, a).into()
    }
}
