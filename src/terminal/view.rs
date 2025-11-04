use std::{
    collections::VecDeque,
    ops::Range,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Result, bail};
use cosmic_text::{
    Attrs, AttrsList, BufferLine, CacheKey, Family, FontSystem, LineEnding, Shaping, SubpixelBin,
    fontdb,
};
use euclid::Point2D;
use rangeset::RangeSet;
use tuple::Map;

use termwiz::{cellcluster::CellCluster, color::ColorAttribute, surface::CursorShape};
use wezterm_term::{
    CellAttributes, Hyperlink, Intensity, Line, StableRowIndex, Underline, color::ColorPalette,
};

use super::TerminalGeometry;
use crate::{
    TerminalFont,
    range_ops::{RangeOps, WithLength},
    terminal::{
        SelectedRange, ViewGeometry, cursor::CursorMetrics, scroll_locations::ScrollLocations,
    },
    window_geometry::CellRect,
};
use massive_animation::{Animated, Interpolation};
use massive_geometry::{Color, Point, Rect, Size};
use massive_scene::{Handle, Location, Visual};
use massive_shapes::{GlyphRun, GlyphRunMetrics, RunGlyph, Shape, StrokeRect, TextWeight};
use massive_shell::Scene;

const SCROLL_ANIMATION_DURATION: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
pub struct TerminalViewParams {
    pub font_system: Arc<Mutex<FontSystem>>,
    pub font: TerminalFont,
    pub parent_location: Handle<Location>,
}

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
    pub params: TerminalViewParams,
    /// Is this view showing the alt screen?
    ///
    /// This is kind of a meta property just so that presenter knows and does not really belong
    /// here, but makes switching simpler.
    pub alt_screen: bool,
    color_palette: ColorPalette,

    locations: ScrollLocations,

    /// The number of pixels with which _all_ lines are transformed upwards.
    ///
    /// When the view scrolls up and a new line comes in on the bottom, this value increases.
    ///
    /// May be negative while animating.
    scroll_offset_px: Animated<f64>,

    /// The first line's stable index in visible lines.
    first_line_stable_index: StableRowIndex,

    /// The visible lines. This contains _only_ the lines currently visible in the terminal.
    ///
    /// No lines for the scrollback. And if we are _inside_ the scrollback buffer, no lines below.
    ///
    /// VecDeque because we want to optimize them for scrolling.
    lines: VecDeque<LineVisuals>,
    cursor: Option<Handle<Visual>>,
    selection: Option<SelectionVisual>,
}

#[derive(Debug)]
struct LineVisuals {
    /// The visual representing the line (Currently includes background and text).
    visual: Handle<Visual>,

    /// The visual for the overlays, like underlines.
    overlays: Handle<Visual>,

    /// The top offset to add to all shapes.
    ///
    /// Might be negative for lines over the top of the terminal's stable range.
    top_offset: i64,
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
        params: TerminalViewParams,
        alt_screen: bool,
        scene: &Scene,
        scroll_offset: StableRowIndex,
    ) -> Self {
        let line_height = params.font.cell_size_px().1;
        assert!(scroll_offset >= 0);
        let scroll_offset_px = scroll_offset as u64 * line_height as u64;
        let locations = ScrollLocations::new(
            params.parent_location.clone(),
            line_height,
            scroll_offset_px.cast_signed(),
        );

        Self {
            params,
            alt_screen,
            color_palette: ColorPalette::default(),
            locations,
            scroll_offset_px: scene.animated(scroll_offset_px as f64),
            first_line_stable_index: 0,
            lines: VecDeque::new(),
            cursor: None,
            selection: None,
        }
    }
}

// Animation & Geometry

impl TerminalView {
    /// Scroll to the new scroll offset.
    pub fn scroll_to_stable(&mut self, scroll_offset: StableRowIndex) {
        assert!(scroll_offset >= 0);
        let scroll_offset_px = scroll_offset as u64 * self.line_height_px() as u64;
        self.scroll_to_px(scroll_offset_px as f64);
    }

    /// Scroll to the pixel offset.
    ///
    /// Detail: Even though we never render the final output at fractional pixels, the animation's
    /// final value can be fractional, too. For example while a selection scroll is active.
    ///
    /// As soon a resting position is defined, the final value should be set to a integral value.
    pub fn scroll_to_px(&mut self, new_scroll_offset_px: f64) {
        self.scroll_offset_px.animate_to_if_changed(
            new_scroll_offset_px,
            SCROLL_ANIMATION_DURATION,
            Interpolation::CubicOut,
        );
    }

    pub fn final_scroll_offset_px(&self) -> f64 {
        self.scroll_offset_px.final_value()
    }

    fn current_scroll_offset_px_snapped(&self) -> i64 {
        self.scroll_offset_px.value().round() as i64
    }

    /// Returns the current fractional scroll offset in pixel.
    pub fn current_scroll_offset_px(&self) -> f64 {
        self.scroll_offset_px.value()
    }

    fn line_height_px(&self) -> u32 {
        self.font().cell_size_px().1
    }

    fn font(&self) -> &TerminalFont {
        &self.params.font
    }

    /// Finalize the current animations.
    ///
    /// This places all lines at their final positions.
    #[allow(unused)]
    pub fn finalize_animations(&mut self) {
        self.scroll_offset_px.finalize();
        self.apply_animations();
    }

    /// Update currently running animations.
    pub fn apply_animations(&mut self) {
        // Detail: Even if the animated value is not anymore animating, we might not have retrieved and
        // update the latest value yet.

        // Snap to the nearest pixel, otherwise animated frames would not be pixel perfect.
        let scroll_offset_px = self.current_scroll_offset_px_snapped();
        self.locations.set_scroll_offset_px(scroll_offset_px);
    }

    /// Return the current geometry of the view.
    ///
    /// This returns the stable range of all the lines we need to update taking the current
    /// scrolling animation into account.
    ///
    /// This also means that the returned range might be out of range with respect to the actual
    /// terminal lines.
    pub fn geometry(&self, terminal_geometry: &TerminalGeometry) -> ViewGeometry {
        let rows = terminal_geometry.rows();
        assert!(rows >= 1);

        // First pixel visible inside the screen viewed.
        let line_height_px = self.font().cell_size_px().1 as i64;

        let topmost_pixel_line_visible = self.current_scroll_offset_px_snapped();
        let topmost_stable_render_line = topmost_pixel_line_visible.div_euclid(line_height_px);
        let topmost_stable_render_line_ascend =
            topmost_pixel_line_visible.rem_euclid(line_height_px);

        // -1 because we want to hit the line the pixel is on and don't render more than row cells
        // if animations are done.
        let bottom_pixel_line_visible =
            (topmost_pixel_line_visible + line_height_px * rows as i64) - 1;
        let bottom_stable_render_line = bottom_pixel_line_visible.div_euclid(line_height_px);
        assert!(bottom_stable_render_line >= topmost_stable_render_line);

        let stable_range = topmost_stable_render_line as StableRowIndex
            ..(bottom_stable_render_line + 1) as StableRowIndex;

        ViewGeometry {
            terminal: *terminal_geometry,
            stable_range_ascend_px: topmost_stable_render_line_ascend as u32,
            stable_range,
        }
    }
}

// Updating

impl TerminalView {
    /// We use the RAII pattern to mark the end of the update so that we can see which lines we need
    /// to preserve matrices for.
    pub fn begin_update<'a>(
        &'a mut self,
        scene: &'a Scene,
        view_range: Range<StableRowIndex>,
        reverse_video: bool,
    ) -> (ViewUpdate<'a>, RangeSet<StableRowIndex>) {
        let additional_lines_needed = self.update_view_range(scene, view_range);
        (
            ViewUpdate {
                scene,
                view: self,
                reverse_video,
            },
            additional_lines_needed,
        )
    }

    fn end_update(&mut self) {
        // Because the cursor does not leave the visible part (I hope), we ignore that for now
        // because its matrix can be recreated any time.
        let mut visuals_range = self.first_line_stable_index.with_len(self.lines.len());
        if let Some(selection) = &self.selection {
            // Review: Unioning the selection can have a nasty large range extension, which needs
            // many locations active.
            visuals_range = visuals_range.union(selection.row_range.clone());
        }
        self.locations.mark_used(visuals_range);
    }
}

#[derive(Debug)]
pub struct ViewUpdate<'a> {
    pub scene: &'a Scene,
    view: &'a mut TerminalView,
    reverse_video: bool,
}

impl Drop for ViewUpdate<'_> {
    fn drop(&mut self) {
        self.view.end_update();
    }
}

impl ViewUpdate<'_> {
    pub fn lines(
        &mut self,
        first_line_stable_index: StableRowIndex,
        lines: &[Line],
        underlined_hyperlink: Option<&Arc<Hyperlink>>,
    ) -> Result<()> {
        self.view.update_lines(
            first_line_stable_index,
            lines,
            underlined_hyperlink,
            self.reverse_video,
        )
    }

    pub fn cursor(&mut self, metrics: Option<CursorMetrics>) {
        self.view.update_cursor(self.scene, metrics);
    }

    pub fn selection(
        &mut self,
        selection: Option<SelectedRange>,
        terminal_geometry: &TerminalGeometry,
    ) {
        self.view
            .update_selection(self.scene, selection, terminal_geometry);
    }
}

impl TerminalView {
    /// This is the first step before lines can be updated.
    ///
    /// This returns a set of stable index ranges that are _required_ to be updated together with
    /// the changed ones in the `view_range`.
    ///
    /// This view_range is the one returned from `view_range()`.
    ///
    /// Architecture: If nothing had changed, we already know the view range.
    ///
    /// This begins the update cycle.
    fn update_view_range(
        &mut self,
        scene: &Scene,
        view_range: Range<StableRowIndex>,
    ) -> RangeSet<StableRowIndex> {
        let mut required_line_updates = RangeSet::new();

        assert!(view_range.end > view_range.start);
        let current_range = self.first_line_stable_index.with_len(self.lines.len());

        let mut new_visual = |stable_index| {
            let (location, top_offset) = self.locations.acquire_line_location(scene, stable_index);
            // Performance: Don't stage visuals with empty shapes?
            let visual = scene.stage(Visual::new(location.clone(), []));
            let overlays = scene.stage(Visual::new(location, []).with_depth_bias(1));
            LineVisuals {
                visual,
                overlays,
                top_offset,
            }
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

        // The line updates returned shall never exceed the view_range passed in.
        debug_assert_eq!(
            required_line_updates.intersection_with_range(view_range),
            required_line_updates
        );

        required_line_updates
    }
}

impl TerminalView {
    fn update_lines(
        &mut self,
        first_line_stable_index: StableRowIndex,
        lines: &[Line],
        underlined_hyperlink: Option<&Arc<Hyperlink>>,
        reverse_video: bool,
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
            let (shapes, overlay_shapes) = {
                // Lock the font_system for the least amount of time possible. This is shared with
                // the renderer.
                let mut font_system = self.params.font_system.lock().unwrap();
                self.create_line_shapes(
                    &mut font_system,
                    top,
                    line,
                    underlined_hyperlink,
                    reverse_video,
                )?
            };

            let line_visuals = &mut self.lines[line_index];

            line_visuals.visual.update_with(|v| {
                v.shapes = shapes.into();
            });

            line_visuals.overlays.update_with(|v| {
                v.shapes = overlay_shapes.into();
            });
        }

        Ok(())
    }

    fn create_line_shapes(
        &self,
        font_system: &mut FontSystem,
        top: i64,
        line: &Line,
        active_hyperlink: Option<&Arc<Hyperlink>>,
        reverse_video: bool,
    ) -> Result<(Vec<Shape>, Vec<Shape>)> {
        // Production: Add bidi support
        let clusters = line.cluster(None);

        // Performance: Background shapes are not included in the capacity. Use a temporary array here.
        let mut shapes: Vec<Shape> = Vec::with_capacity(clusters.len());
        // Performance: Can we use some capacity here? Use a temporary array here?
        let mut overlay_shapes = Vec::new();
        let mut left = 0;
        let cell_size_px = self.font().cell_size_px().0 as i64;

        // Optimization: Combine clusters with compatible attributes. Colors and widths can vary
        // inside a GlyphRun.
        for cluster in clusters {
            let attributes =
                AttributeResolver::new(&self.color_palette, reverse_video, &cluster.attrs);

            let run =
                Self::cluster_to_run(font_system, self.font(), &attributes, (left, top), &cluster)?;

            let background =
                Self::cluster_background(&cluster, self.font(), &attributes, (left, top));

            let underline_hyperlink =
                active_hyperlink.is_some() && cluster.attrs.hyperlink() == active_hyperlink;

            let overlay = Self::cluster_decorations(
                &cluster,
                self.font(),
                &attributes,
                (left, top),
                underline_hyperlink,
            );

            if let Some(run) = run {
                shapes.push(run.into());
            }

            if let Some(background) = background {
                shapes.push(background)
            }

            if let Some(overlay) = overlay {
                overlay_shapes.push(overlay);
            }

            left += cluster.width as i64 * cell_size_px;
        }

        Ok((shapes, overlay_shapes))
    }

    fn cluster_to_run(
        font_system: &mut FontSystem,
        font: &TerminalFont,
        attributes: &AttributeResolver,
        (left, top): (i64, i64),
        cluster: &CellCluster,
    ) -> Result<Option<GlyphRun>> {
        let text_weight = attributes.text_weight();
        let font_weight = fontdb::Weight(text_weight.0);

        // Performance: BufferLine makes a copy of the text, is there a better way?
        // Architecture: Should we shape all clusters in one go and prepare Attrs::metadata() accordingly?
        // Architecture: Under the hood, HarfRust is used for text shaping, use it directly?
        // Performance: This contains internal caches, which might benefit reusing them.
        let mut buffer = BufferLine::new(
            &cluster.text,
            LineEnding::None,
            AttrsList::new(
                &Attrs::new()
                    .family(Family::Name(&font.family_name))
                    .weight(font_weight),
            ),
            Shaping::Advanced,
        );

        let shaped_glyphs = buffer
            // Simplify: If the ShapeLine cache is always empty, we may be able to use
            // ShapeLine::build directly, or even better cache it directly here? This will then
            // reuse most allocations? ... but we could just re-use BufferLine, or....?
            .shape(font_system, 0)
            .spans
            .iter()
            .flat_map(|span| &span.words)
            .filter(|word| !word.blank)
            .flat_map(|word| &word.glyphs);

        let mut glyphs = Vec::with_capacity(cluster.width);

        for glyph in shaped_glyphs {
            // We place the glyphs based on what the cluster says not what the layout engine
            // provides.
            let cell_index = cluster.byte_to_cell_idx(glyph.start) - cluster.first_cell_idx;
            let glyph_x = cell_index as u32 * font.glyph_advance_px;

            // Optimization: Don't pass empty / blank glyphs.
            let glyph = RunGlyph {
                pos: (glyph_x as i32, 0),
                // Architecture: Introduce an internal CacheKey that does not use SubpixelBin (we won't
                // support that ever, because the author holds the belief that subpixel rendering is a scam)
                //
                // Architecture: Research if we would actually benefit from subpixel rendering in
                // inside a regular gray scale anti-aliasing setup.
                key: CacheKey {
                    font_id: glyph.font_id,
                    glyph_id: glyph.glyph_id,
                    font_size_bits: font.size.to_bits(),
                    x_bin: SubpixelBin::Zero,
                    y_bin: SubpixelBin::Zero,
                    font_weight: glyph.font_weight,
                    flags: glyph.cache_key_flags,
                },
            };
            glyphs.push(glyph);
        }

        let run = GlyphRun {
            translation: (left as _, top as _, 0.).into(),
            metrics: GlyphRunMetrics {
                // Precision: compute this once for the font size so that it also matches the pixel cell
                // size.
                max_ascent: font.ascender_px,
                max_descent: font.descender_px,
                width: (cluster.width as u32 * font.glyph_advance_px),
            },
            text_color: attributes.foreground_color,
            // This looks redundant here.
            text_weight,
            glyphs,
        };

        Ok(Some(run))
    }

    /// Generates the background shape for the cluster.
    fn cluster_background(
        cluster: &CellCluster,
        font: &TerminalFont,
        attributes: &AttributeResolver,
        (left, top): (i64, i64),
    ) -> Option<Shape> {
        let Some(background_color) = attributes.background_color else {
            // Assume that the background is rendered in the default background color.
            return None;
        };

        let size: Size = (
            // Precision: We keep multiplication in the u32 range here. Unlikely it's overflowing.
            (cluster.width as u32 * font.cell_size_px().0) as f64,
            font.cell_size_px().1 as f64,
        )
            .into();

        let lt: Point = (left as f64, top as f64).into();

        Some(massive_shapes::Rect::new(Rect::new(lt, size), background_color).into())
    }

    /// Generates the decoration shape for the cluster.
    ///
    /// This includes underlines, etc.
    fn cluster_decorations(
        cluster: &CellCluster,
        font: &TerminalFont,
        attributes: &AttributeResolver,
        (left, top): (i64, i64),
        underline_hyperlink: bool,
    ) -> Option<Shape> {
        let underline = cluster.attrs.underline();
        // Feature: Don't highlight if the hyperlink is not hovered over.
        let effective_underline = match (underline_hyperlink, underline) {
            (true, Underline::None) => Underline::Single,
            (true, Underline::Single) => Underline::Double,
            (true, _) => Underline::Single,
            (false, u) => u,
        };

        // Feature: Implement overline
        // Feature: Implement strikethrough
        let underline_metrics = match effective_underline {
            Underline::None => None,
            Underline::Single => Some(&font.underline_px),
            Underline::Double => Some(&font.double_underline_px),
            // Feature: Implement the rest of them.
            Underline::Curly => None,
            Underline::Dotted => None,
            Underline::Dashed => None,
        };

        if let Some(underline_metrics) = underline_metrics {
            let lt: Point = (
                left as f64,
                (top + underline_metrics.position as i64) as f64,
            )
                .into();

            let size: Size = (
                // Precision: We keep multiplication in the u32 range here. Unlikely it's overflowing.
                (cluster.width as u32 * font.cell_size_px().0) as f64,
                underline_metrics.thickness as f64,
            )
                .into();

            return Some(
                massive_shapes::Rect::new(Rect::new(lt, size), attributes.underline_color()).into(),
            );
        }

        None
    }
}

// Cursor

#[derive(Debug)]
enum CursorShapeType {
    Rect,
    Block,
    Underline,
    Bar,
}

impl TerminalView {
    fn update_cursor(&mut self, scene: &Scene, cursor_metrics: Option<CursorMetrics>) {
        self.cursor = cursor_metrics.map(|metrics| {
            let shape_type = Self::cursor_shape_type(metrics.pos.shape, metrics.focused);
            // Detail: pos.y is a VisibleRowIndex.
            let (location, top_px) = self
                .locations
                .acquire_line_location(scene, metrics.stable_y);
            let shape = self.cursor_shape(shape_type, metrics.pos.x, metrics.width, top_px);
            scene.stage(Visual::new(location, [shape]))
        })
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

    fn cursor_shape(
        &self,
        ty: CursorShapeType,
        column: usize,
        width: usize,
        y_offset_px: i64,
    ) -> Shape {
        let cursor_color = self.color_palette.cursor_bg;
        let cell_size = self.font().cell_size_px();
        let left = cell_size.0 * column as u32;

        // Feature: The size of the bar / underline should be derived from the font size / underline
        // position / thickness, not from the cell size.
        let stroke_thickness = ((cell_size.0 as f64 / 4.) + 1.).trunc();

        let cell_width = cell_size.0 * width as u32;

        let rect = match ty {
            CursorShapeType::Rect => {
                return StrokeRect::new(
                    Rect::new(
                        (left as _, y_offset_px as _),
                        (cell_width as _, cell_size.1 as _),
                    ),
                    Size::new(stroke_thickness, stroke_thickness),
                    color::from_srgba(cursor_color),
                )
                .into();
            }
            CursorShapeType::Block => Rect::new(
                (left as _, y_offset_px as _),
                (cell_width as _, cell_size.1 as _),
            ),
            CursorShapeType::Underline => Rect::new(
                (
                    left as _,
                    ((y_offset_px + self.font().ascender_px as i64) as f64) as _,
                ),
                (cell_width as _, stroke_thickness),
            ),
            CursorShapeType::Bar => Rect::new(
                (left as _, y_offset_px as _),
                // Ergonomics: Shouldn't we multiply stroke_thickness with width?
                (stroke_thickness, cell_size.1 as _),
            ),
        };

        massive_shapes::Rect::new(rect, color::from_srgba(cursor_color)).into()
    }
}

// Selection

impl TerminalView {
    fn update_selection(
        &mut self,
        scene: &Scene,
        selection: Option<SelectedRange>,
        terminal_geometry: &TerminalGeometry,
    ) {
        match selection {
            Some(selection_range) => {
                // Robustness: A selection can span lines outside of the view range. To keep the
                // numerical stability in the matrix, we should clip the rects to the visible range.
                let rects_stable =
                    Self::selection_rects(&selection_range, terminal_geometry.columns());
                let cell_size = terminal_geometry.cell_size_px.map(f64::from);
                let location_stable_index = selection_range.stable_rows().start;

                let (location, top_px) = self
                    .locations
                    .acquire_line_location(scene, location_stable_index);

                let top_stable_px = location_stable_index as i64 * self.line_height_px() as i64;
                let translation_offset = top_px - top_stable_px;

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
                        selection.row_range = selection_range.stable_rows();
                        selection.visual.update_if_changed(visual);
                    }
                    None => {
                        self.selection = Some(SelectionVisual {
                            row_range: selection_range.stable_rows(),
                            visual: scene.stage(visual),
                        })
                    }
                }
            }
            None => self.selection = None,
        }
    }

    /// A selection can be rendered in one to three rectangles.
    /// Robustness: Pass a clip rect here.
    fn selection_rects(selection: &SelectedRange, terminal_columns: usize) -> Vec<CellRect> {
        assert!(terminal_columns > 0);

        let min = Point2D::new(0, 0);
        // Precision: Also clamp rows here?
        let max = Point2D::new(terminal_columns as isize, isize::MAX);

        // First convert to half-open intervals, then clamp the columns.
        //
        // This may result in an empty column range.
        let start_point = selection
            .start()
            .point()
            .clamp(min, max)
            .map(|c| c as usize);
        let end_point = selection
            .end()
            .point()
            .map(|c| c.saturating_add(1))
            .clamp(min, max)
            .map(|c| c as usize);

        let lines_covering = end_point.y - start_point.y;
        assert!(lines_covering > 0);

        // Performance: Capacity
        let mut vecs = if lines_covering == 1 {
            vec![CellRect::new(
                start_point,
                (end_point.x - start_point.x, 1).into(),
            )]
        } else {
            let top_line = CellRect::new(start_point, (terminal_columns - start_point.x, 1).into());
            let bottom_line = CellRect::new((0, end_point.y - 1).into(), (end_point.x, 1).into());

            if lines_covering == 2 {
                vec![top_line, bottom_line]
            } else {
                vec![
                    top_line,
                    CellRect::new(
                        (0, start_point.y + 1).into(),
                        (terminal_columns, lines_covering - 2).into(),
                    ),
                    bottom_line,
                ]
            }
        };

        // Some of the rects might be empty, because of clamping.
        vecs.retain(|r| !r.is_empty());
        vecs
    }
}

#[derive(Debug)]
struct AttributeResolver<'a> {
    palette: &'a ColorPalette,
    pub attributes: &'a CellAttributes,
    foreground_color: Color,
    // `None` indicates no background rendering.
    background_color: Option<Color>,
}

impl<'a> AttributeResolver<'a> {
    pub fn new(palette: &'a ColorPalette, reverse_video: bool, attrs: &'a CellAttributes) -> Self {
        // Precompute the ones we use multiple times.

        let (foreground, background) = (attrs.foreground(), attrs.background());
        let background_default = background == ColorAttribute::Default;

        let foreground = Self::resolve_fg(foreground, palette, attrs);
        let background = color::from_srgba(palette.resolve_bg(background));

        let (foreground, background, background_default) = if attrs.reverse() != reverse_video {
            (background, foreground, false)
        } else {
            (foreground, background, background_default)
        };

        Self {
            palette,
            attributes: attrs,
            foreground_color: foreground,
            background_color: (!background_default).then_some(background),
        }
    }

    pub fn underline_color(&self) -> Color {
        let color = self.attributes.underline_color();
        if color == ColorAttribute::Default {
            return self.foreground_color;
        }
        // Detail: Resolving fg / bg behaves the same if the color is not the default.
        Self::resolve_fg(color, self.palette, self.attributes)
    }

    /// Resolve a foreground color, including bold brightening.
    fn resolve_fg(color: ColorAttribute, palette: &ColorPalette, attrs: &CellAttributes) -> Color {
        // bold brightening.
        let color = match color {
            ColorAttribute::PaletteIndex(i) if i < 8 && attrs.intensity() == Intensity::Bold => {
                ColorAttribute::PaletteIndex(i + 8)
            }
            color => color,
        };

        color::from_srgba(palette.resolve_fg(color))
    }

    pub fn text_weight(&self) -> TextWeight {
        match self.attributes.intensity() {
            Intensity::Half => TextWeight::LIGHT,
            Intensity::Normal => TextWeight::NORMAL,
            Intensity::Bold => TextWeight::BOLD,
        }
    }
}

mod color {
    use massive_geometry::Color;
    use termwiz::color::SrgbaTuple;

    // Precision: Clarify what color profile we are actually using and document this in the massive Color.
    pub fn from_srgba(SrgbaTuple(r, g, b, a): SrgbaTuple) -> Color {
        (r, g, b, a).into()
    }
}
