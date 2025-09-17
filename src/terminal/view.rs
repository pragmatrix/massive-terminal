use std::{
    collections::VecDeque,
    ops::Range,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Result, bail};
use cosmic_text::{
    Attrs, AttrsList, BufferLine, CacheKey, Family, FontSystem, LineEnding, Shaping, SubpixelBin,
    Wrap,
};
use log::trace;
use rangeset::RangeSet;
use tuple::Map;

use termwiz::{
    cellcluster::CellCluster,
    color::ColorAttribute,
    surface::{CursorShape, CursorVisibility},
};
use wezterm_term::{CursorPosition, Intensity, Line, StableRowIndex, color::ColorPalette};

use super::TerminalGeometry;
use crate::{
    TerminalFont,
    range_ops::{RangeOps, WithLength},
    selection::{NormalizedSelectionRange, SelectionRange},
    terminal::scroll_locations::ScrollLocations,
    window_geometry::CellRect,
};
use massive_animation::{Interpolation, Timeline};
use massive_geometry::{Point, Rect, Size};
use massive_scene::{Handle, Location, Visual};
use massive_shapes::{GlyphRun, GlyphRunMetrics, RunGlyph, Shape, StrokeRect, TextWeight};
use massive_shell::Scene;

const SCROLL_DURATION: Duration = Duration::from_millis(100);

/// TerminalView is the into a terminal's screen lines.
///
/// - It always contains a single [`Visual`] for each line. Even if this line is currently not
///   rendered.
/// - The coordinate system starts the left top (centering it may cause half-pixel).
/// - All lines use the same base "location" and translate
///
/// Naming: ScreenRenderer? ScreenVisuals, TerminalScreen, because now this corresponds to a
/// terminal screen.
#[derive(Debug)]
pub struct TerminalView {
    font_system: Arc<Mutex<FontSystem>>,
    /// The terminal font.
    font: TerminalFont,
    color_palette: ColorPalette,

    locations: ScrollLocations,

    /// Positive scroll offset of the screen.
    scroll_offset: StableRowIndex,

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
    lines: VecDeque<LineVisual>,
    cursor: Option<Handle<Visual>>,
    selection: Option<SelectionVisual>,
}

#[derive(Debug)]
struct LineVisual {
    /// The visual representing the line.
    visual: Handle<Visual>,
    /// The top offset to add to all shapes.
    top_offset: u64,
}

#[derive(Debug)]
struct SelectionVisual {
    row_range: Range<StableRowIndex>,
    visual: Handle<Visual>,
}

impl TerminalView {
    /// Create a new view.
    ///
    /// Scene is needed to pre-create all the rows. This in turn prevents us from caring too much
    /// lazily creating them later, but may put a little more pressure on the renderer to filter out
    /// unused visuals.
    pub fn new(
        font_system: Arc<Mutex<FontSystem>>,
        font: TerminalFont,
        scroll_offset: isize,
        parent_location: Handle<Location>,
        scene: &Scene,
    ) -> Self {
        let line_height = font.cell_size_px().1;
        assert!(scroll_offset >= 0);
        let scroll_offset_px = scroll_offset as u64 * line_height as u64;
        let locations = ScrollLocations::new(parent_location, line_height, scroll_offset_px);

        Self {
            font_system,
            font,
            color_palette: ColorPalette::default(),
            locations,
            scroll_offset,
            scroll_offset_px: scene.timeline(scroll_offset_px as f64),
            first_line_stable_index: 0,
            lines: VecDeque::new(),
            cursor: None,
            selection: None,
        }
    }
}

// Lines

impl TerminalView {
    /// Scroll to the new scroll offset.
    pub fn scroll_to(&mut self, new_scroll_offset: StableRowIndex) {
        let delta_lines = new_scroll_offset - self.scroll_offset;
        if delta_lines == 0 {
            return;
        }

        self.scroll_offset += delta_lines;
        assert!(self.scroll_offset >= 0);

        self.scroll_offset_px.animate_to(
            self.scroll_offset_in_px() as f64,
            SCROLL_DURATION,
            Interpolation::CubicOut,
        );
    }

    pub fn scroll_offset(&self) -> StableRowIndex {
        self.scroll_offset
    }

    fn scroll_offset_in_px(&self) -> i64 {
        self.scroll_offset as i64 * self.line_height_px()
    }

    fn line_height_px(&self) -> i64 {
        self.font.cell_size_px().1 as i64
    }

    /// Reset the current soft scrolling.
    ///
    /// This places all lines at their final positions.
    pub fn reset_animations(&mut self) {
        self.scroll_offset_px.commit_animation();
        self.apply_animations();
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
        self.locations.set_scroll_offset_px(scroll_offset_px as u64);
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
        // Update the range of matrices we need upfront.
        // Missing: This is incorrect, needs to include selection and cursor.
        // self.locations.mark_used(view_range.clone());

        let mut required_line_updates = RangeSet::new();

        assert!(view_range.end > view_range.start);
        let current_range = self.first_line_stable_index.with_len(self.lines.len());

        let mut new_visual = |stable_index| {
            let (location, top_offset) = self.locations.acquire_line_location(scene, stable_index);
            let visual = scene.stage(Visual::new(location, []));
            LineVisual { visual, top_offset }
        };

        if !view_range.intersects(&current_range) {
            // Non-overlapping: reset completely.
            self.first_line_stable_index = view_range.start;
            self.lines = view_range.clone().map(new_visual).collect();
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
                let range = view_range.start..current_range.start;
                range.rev().for_each(|stable_index| {
                    self.lines.push_front(new_visual(stable_index));
                });
                self.first_line_stable_index = view_range.start;
                required_line_updates.add_range(view_range.start..current_range.start);
            }
            _ => {}
        }

        let bottom_delta = view_range.end - current_range.end;
        match bottom_delta {
            _ if bottom_delta > 0 => {
                let new_lines = current_range.end..view_range.end;
                self.lines.extend(new_lines.clone().map(new_visual));
                required_line_updates.add_range(new_lines);
            }
            _ if bottom_delta < 0 => {
                let to_remove = (-bottom_delta) as usize;
                self.lines.truncate(self.lines.len() - to_remove);
            }
            _ => {}
        }

        debug_assert_eq!(
            view_range,
            self.first_line_stable_index.with_len(self.lines.len())
        );

        required_line_updates
    }

    pub fn update_lines(
        &mut self,
        first_line_stable_index: StableRowIndex,
        lines: &[Line],
    ) -> Result<()> {
        let update_range = first_line_stable_index.with_len(lines.len());
        let lines_range = self.first_line_stable_index.with_len(self.lines.len());
        if !update_range.is_inside(&lines_range) {
            bail!("Internal error: Updated lines {update_range:?} is not inside {lines_range:?}");
        }

        for (i, line) in lines.iter().enumerate() {
            let line_index =
                (update_range.start - self.first_line_stable_index + i as isize) as usize;

            let top = self.lines[line_index].top_offset;
            let shapes = {
                // Lock the font_system for the least amount of time possible. This is shared with
                // the renderer.
                let mut font_system = self.font_system.lock().unwrap();
                self.line_to_shapes(&mut font_system, top as i64, line)?
            };

            self.lines[line_index].visual.update_with(|v| {
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

enum CursorShapeType {
    Rect,
    Block,
    Underline,
    Bar,
}

// Cursor

impl TerminalView {
    pub fn update_cursor(&mut self, scene: &Scene, pos: CursorPosition, window_focused: bool) {
        match pos.visibility {
            CursorVisibility::Hidden => {
                self.cursor = None;
            }
            CursorVisibility::Visible => {
                let shape_type = Self::cursor_shape_type(pos.shape, window_focused);
                // Detail: pos.y is a VisibleRowIndex.
                let cursor_stable_line = pos.y as isize + self.scroll_offset;
                let (location, top_px) = self
                    .locations
                    .acquire_line_location(scene, cursor_stable_line);
                let shape = self.cursor_shape(shape_type, pos.x, top_px);
                self.cursor = Some(scene.stage(Visual::new(location, [shape])));
            }
        }
    }

    fn cursor_shape_type(shape: CursorShape, focused: bool) -> CursorShapeType {
        if !focused {
            return CursorShapeType::Rect;
        }
        match shape {
            // Feature: Make default cursor configurable.
            CursorShape::Default => CursorShapeType::Block,
            CursorShape::BlinkingBlock => CursorShapeType::Block,
            CursorShape::SteadyBlock => CursorShapeType::Block,
            CursorShape::BlinkingUnderline => CursorShapeType::Underline,
            CursorShape::SteadyUnderline => CursorShapeType::Underline,
            CursorShape::BlinkingBar => CursorShapeType::Bar,
            CursorShape::SteadyBar => CursorShapeType::Bar,
        }
    }

    fn cursor_shape(&self, ty: CursorShapeType, column: usize, top_px: u64) -> Shape {
        let cursor_color = self.color_palette.cursor_bg;
        let cell_size = self.font.cell_size_px();
        let left = cell_size.0 * column as u32;

        // Feature: The size of the bar / underline should be derived from the font size / underline
        // position / thickness, not from the cell size.
        let stroke_thickness = ((cell_size.0 as f64 / 4.) + 1.).trunc();

        let rect = match ty {
            CursorShapeType::Rect => {
                return StrokeRect::new(
                    Rect::new(
                        (left as _, top_px as _),
                        (cell_size.0 as _, cell_size.1 as _),
                    ),
                    Size::new(stroke_thickness, stroke_thickness),
                    color::from_srgba(cursor_color),
                )
                .into();
            }
            CursorShapeType::Block => Rect::new(
                (left as _, top_px as _),
                (cell_size.0 as _, cell_size.1 as _),
            ),
            CursorShapeType::Underline => Rect::new(
                (
                    left as _,
                    ((top_px + self.font.ascender_px as u64) as f64) as _,
                ),
                (cell_size.0 as _, stroke_thickness),
            ),
            CursorShapeType::Bar => Rect::new(
                (left as _, top_px as _),
                (stroke_thickness, cell_size.1 as _),
            ),
        };

        massive_shapes::Rect::new(rect, color::from_srgba(cursor_color)).into()
    }
}

// Selection

impl TerminalView {
    pub fn update_selection(
        &mut self,
        scene: &Scene,
        selection: Option<NormalizedSelectionRange>,
        terminal_geometry: &TerminalGeometry,
    ) {
        match selection {
            Some(selection) => {
                // Robustness: A selection can span a lot of lines here, even the ones outside. To
                // keep the numerical stability in the matrix, we should clip the rects to the
                // visible range.
                let rects_stable = Self::selection_rects(&selection, terminal_geometry.columns());
                let cell_size = terminal_geometry.cell_size_px.map(f64::from);
                let location_stable_index = selection.row_range().start;

                let (location, top_px) = self
                    .locations
                    .acquire_line_location(scene, location_stable_index);

                let top_stable_px = location_stable_index as i64 * self.line_height_px();
                let translation_offset = top_px as i64 - top_stable_px;

                let rects_final = rects_stable.iter().map(|r| {
                    r.to_f64()
                        .scale(cell_size.0, cell_size.1)
                        .translate((0., translation_offset as f64).into())
                });

                let selection_color = color::from_srgba(self.color_palette.selection_bg);

                let shapes: Vec<_> = rects_final
                    .map(|r| massive_shapes::Rect::new(r, selection_color).into())
                    .collect();

                let visual = Visual::new(location, shapes);

                //
                match &mut self.selection {
                    Some(selection) => {
                        selection.visual.update_if_changed(visual);
                    }
                    None => {
                        self.selection = Some(SelectionVisual {
                            row_range: selection.row_range(),
                            visual: scene.stage(visual),
                        })
                    }
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

// Udating cycle. This was introduced to clean up the locations.

impl TerminalView {
    pub fn updates_done(&mut self) {
        // Because the cursor does not leave the visible part (I hope), we ignore it for now (its
        // matrix can be recreated any time).
        let mut visuals_range = self.first_line_stable_index.with_len(self.lines.len());
        if let Some(selection) = &self.selection {
            visuals_range = visuals_range.union(selection.row_range.clone());
        }
        self.locations.mark_used(visuals_range);
    }
}

mod color {
    use massive_geometry::Color;
    use termwiz::color::SrgbaTuple;

    pub fn from_srgba(SrgbaTuple(r, g, b, a): SrgbaTuple) -> Color {
        (r, g, b, a).into()
    }
}
