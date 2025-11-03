use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use cosmic_text::Font;
use swash::StringId;

/// A monospaced, terminal font of a certain size.
///
/// Ergonomics: Separate metrics from font.
#[allow(unused)]
#[derive(Debug, Clone)]
pub struct TerminalFont {
    pub font: Arc<Font>,
    pub family_name: String,
    pub size: f32,

    pub units_per_em: u32,
    pub ascender_em: u32,
    pub descender_em: u32,
    pub glyph_advance_em: u32,

    /// Normalized glyph size.
    ///
    /// Width is equal to the character's `M` width. Height is equal to font height.
    pub glyph_size: (f32, f32),

    /// Ascender in pixel, never larger than cell pixel height.
    pub ascender_px: u32,
    /// Descender in pixel.
    pub descender_px: u32,

    pub glyph_advance_px: u32,

    /// Converted to px. If not provided, a line at ascender_px.
    pub underline_px: LineMetrics,
    pub double_underline_px: LineMetrics,
}

#[derive(Debug, Clone)]
pub struct LineMetrics {
    pub position: u32,
    pub thickness: u32,
}

const MONOSPACE_WIDTH_CHARACTER: char = '0';

impl TerminalFont {
    pub fn from_cosmic_text(font: Arc<Font>, size: f32) -> Result<Self> {
        let family_name = font
            .as_swash()
            .localized_strings()
            .find_by_id(StringId::Family, None)
            .ok_or(anyhow!("Failed to get family name from font (name id 1)"))?
            .to_string();
        let swash = font.as_swash();

        // Feature: May be use swash metrics directly?
        // let swash_metrics = swash.metrics(&[]);

        let metrics = font.metrics();

        if !metrics.is_monospace {
            bail!("Terminal fonts must be monospaced");
        }

        // Robustness: Reimplement this
        // if hb_font.line_gap() != 0 {
        //     bail!("Monospace fonts with a line gap aren't supported (yet)")
        // }

        // We make ascender and descender larger (ceil), so that the font always fits in.
        let ascender_em = to_em_unsigned(metrics.ascent, "ascender")?;

        // By convention, descender is negative, but we treat it as positive.
        let descender_em = to_em_unsigned(-metrics.descent, "descender")?;

        // Line gaps / leading may be negative. For now, we want to know about this.
        let line_gap = to_em_unsigned(metrics.leading, "line gap / leading")?;

        if line_gap != 0 {
            bail!("Monospace fonts with a line gap aren't supported (yet)")
        }

        // monospace_em_width() may be an alternative, but it wasn't available for JetBrains Mono.

        let m_id = swash.charmap().map(MONOSPACE_WIDTH_CHARACTER);
        let gm = swash.glyph_metrics(&[]);
        let glyph_width_em = to_em_unsigned(
            gm.advance_width(m_id),
            "advance with of monospace width defining character",
        )?;
        // Detail: Keep line gap here even though it's currently always 0.
        let glyph_height_em = ascender_em + descender_em + line_gap;

        let units_per_em = metrics.units_per_em;
        let units_per_em_f = units_per_em as f32;
        let font_size_f = size / units_per_em_f;

        let glyph_size = {
            (
                glyph_width_em as f32 * font_size_f,
                glyph_height_em as f32 * font_size_f,
            )
        };

        // Research: Why trunc() and not round()?
        let cell_pixel_size = (glyph_size.0.trunc() as u32, glyph_size.1.trunc() as u32);

        let ascender_px =
            ((ascender_em as f32 * font_size_f).trunc() as u32).min(cell_pixel_size.1);

        let descender_px = cell_pixel_size.1 - ascender_px;

        let underline_px = if let Some(underline_metrics) = metrics.underline {
            // Precision: Make sure that the underline fits in the cell.
            LineMetrics {
                position: ((glyph_height_em.cast_signed() + underline_metrics.offset as i32) as f32
                    * font_size_f)
                    .trunc() as u32,
                thickness: ((underline_metrics.thickness * font_size_f) as u32).max(1),
            }
        } else {
            LineMetrics {
                position: ascender_px,
                // Precision: This should be relative to the font size.
                thickness: 1,
            }
        };

        let double_underline_px = LineMetrics {
            position: underline_px.position,
            // Precision: Make sure this fits in a cell / does not exceed descender.
            thickness: underline_px.thickness * 2,
        };

        Ok(Self {
            font,
            family_name,
            size,
            units_per_em: units_per_em as u32,
            ascender_em,
            descender_em,
            glyph_advance_em: glyph_width_em,
            glyph_size,
            ascender_px,
            descender_px,
            glyph_advance_px: cell_pixel_size.0,
            underline_px,
            double_underline_px,
        })
    }

    pub fn cell_size_px(&self) -> (u32, u32) {
        (self.glyph_advance_px, self.font_height_px())
    }

    pub fn font_height_px(&self) -> u32 {
        self.ascender_px + self.descender_px
    }
}

fn to_em_unsigned(value: f32, value_type: &str) -> Result<u32> {
    // Detail: Use round(), this is to compensate for internal inaccuracies. Internally fonts store
    // design units as integers.
    (value.round() as i32).try_into().with_context(|| {
        format!("Failed to convert em font value `{value_type}` from f32 to a positive integer")
    })
}
