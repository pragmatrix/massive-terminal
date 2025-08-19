use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use anyhow::{Result, bail};
use cosmic_text::{
    Attrs, AttrsList, BufferLine, CacheKey, Family, FontSystem, LineEnding, Shaping, SubpixelBin,
    Wrap,
};
use termwiz::cellcluster::CellCluster;
use wezterm_term::{Intensity, Line, color::ColorPalette};

use massive_geometry::Identity;
use massive_scene::{Handle, Location, Matrix, Scene, Shape, Visual};
use massive_shapes::{GlyphRun, GlyphRunMetrics, RunGlyph, TextWeight};

use crate::TerminalFont;

/// Panel is the representation of the terminal screen.
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
    _scroll_location: Handle<Location>,
    // VecDeque becase we want to optimize them for scrolling.
    line_visuals: VecDeque<Handle<Visual>>,
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
            _scroll_location: scroll_location,
            line_visuals,
        }
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
            let top = line_index * self.font.cell_pixel_size.1;
            let shapes = self.line_to_shapes(&mut font_system, top, line)?;
            self.line_visuals[line_index].update_with(|v| {
                // Appreciate: This converts a Vec<Shape> directly into a Arc<[Shape]>.
                v.shapes = shapes.into();
            });
        }

        Ok(())
    }

    fn line_to_shapes(
        &self,
        font_system: &mut FontSystem,
        top: usize,
        line: &Line,
    ) -> Result<Vec<Shape>> {
        // Production: Add bidi support
        let clusters = line.cluster(None);

        let mut runs = Vec::with_capacity(clusters.len());

        for cluster in clusters {
            runs.push(cluster_to_run(
                font_system,
                &self.font.family_name,
                &self.color_palette,
                self.font.size,
                top,
                &cluster,
            )?)
        }

        Ok(runs.into_iter().flatten().map(|r| r.into()).collect())
    }
}

fn cluster_to_run(
    font_system: &mut FontSystem,
    family_name: &str,
    color_palette: &ColorPalette,
    font_size: f32,
    top: usize,
    cluster: &CellCluster,
) -> Result<Option<GlyphRun>> {
    let attributes = &cluster.attrs;

    // Performance: BufferLine makes a copy of the text, is there another way?

    // Architecture: Should we shape all clusters in one co and prepare Attrs::metadata() accordingly?
    // Architecture: Under the hood, rustybuzz is used for text shaping, use it directly?
    // Robustness: Directly select a font here, either by name or even better by id.
    let mut buffer = BufferLine::new(
        &cluster.text,
        LineEnding::None,
        AttrsList::new(&Attrs::new().family(Family::Name(family_name))),
        Shaping::Advanced,
    );

    let lines = buffer.layout(font_system, font_size, None, Wrap::None, None, 0);
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

    for glyph in &line.glyphs {
        let glyph = RunGlyph {
            pos: (glyph.x.floor() as _, top as _),
            // Architecture: Interoduce an internal CacheKey that does not use SubpixelBin (we won't
            // support that ever, because the author holds the belief that subpixel rendering is a scam)
            //
            // Architecture: Research if we would actually benefit from subpixel rendering in
            // inside a regular gray scale anti-alising setup.
            key: CacheKey {
                font_id: glyph.font_id,
                glyph_id: glyph.glyph_id,
                font_size_bits: glyph.font_size.to_bits(),
                x_bin: SubpixelBin::Zero,
                y_bin: SubpixelBin::Zero,
                flags: glyph.cache_key_flags,
            },
        };
        glyphs.push(glyph);
    }

    // Precision: Clarify what color profile we are actually using and document this in the massive Color.
    let (r, g, b, a) = color_palette.resolve_fg(attributes.foreground()).into();
    // Feature: Support a base wheigt.
    let weight = match attributes.intensity() {
        Intensity::Half => TextWeight::LIGHT,
        Intensity::Normal => TextWeight::NORMAL,
        Intensity::Bold => TextWeight::BOLD,
    };

    let run = GlyphRun {
        translation: (0., 0., 0.).into(),
        metrics: GlyphRunMetrics {
            // Precision: compute this once for the font size so that it also matches the pixel cell
            // size.
            max_ascent: line.max_ascent.ceil() as u32,
            max_descent: line.max_descent.ceil() as u32,
            width: line.w.ceil() as u32,
        },
        text_color: (r, g, b, a).into(),
        text_weight: weight,
        glyphs,
    };

    Ok(Some(run))
}
