use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use cosmic_text::Font;

/// A monospaced, terminal font of a certain size.
#[derive(Debug, Clone)]
pub struct TerminalFont {
    pub font: Arc<Font>,
    pub family_name: String,
    pub size: f32,

    pub units_per_em: usize,
    pub ascender_em: usize,
    pub descender_em: usize,
    pub glyph_advance_em: usize,

    /// Normalized glyph size.
    ///
    /// Width is equal to the character's `M` width. Height is equal to font height.
    pub glyph_size: (f32, f32),

    /// Ascender in pixel, never larger than cell pixel height.
    pub ascender_px: usize,
    /// Descender in pixel.
    pub descender_px: usize,

    pub glyph_advance_px: usize,
}

impl TerminalFont {
    pub fn from_cosmic_text(font: Arc<Font>, size: f32) -> Result<Self> {
        let hb_font = font.rustybuzz();

        let family_name = hb_font
            .names()
            .into_iter()
            .find(|n| n.name_id == 1)
            .ok_or(anyhow!("Failed to get family name from font (name id 1)"))?
            .to_string()
            .ok_or(anyhow!("Only unicode family names are supported (yet)"))?;

        if !hb_font.is_monospaced() {
            bail!("Terminal fonts must be monospaced");
        }

        if hb_font.line_gap() != 0 {
            bail!("Monospace fonts with a line gap aren't supported (yet)")
        }

        let ascender_em: usize = hb_font
            .ascender()
            .try_into()
            .context("Unexpected font ascender")?;
        let descender_em: usize = (-hb_font.descender())
            .try_into()
            .context("Unexpected font descender")?;

        let glyph_size_em = {
            let glyph_width = {
                let glyph_index = hb_font
                    .glyph_index('M')
                    .ok_or(anyhow!("Retrieving glyph `M` failed"))?;
                let advance: u16 = hb_font
                    .glyph_hor_advance(glyph_index)
                    .ok_or(anyhow!("Getting the advance of the letter `M` failed"))?;
                advance as usize
            };
            // Naming: This is font_height, not glyph height.
            let glyph_height: usize = hb_font.height().try_into().context("Font height")?;
            (glyph_width, glyph_height)
        };

        let units_per_em = hb_font.units_per_em();
        let units_per_em_f = units_per_em as f32;
        let font_size_f = size / units_per_em_f;

        let glyph_size = {
            (
                glyph_size_em.0 as f32 * font_size_f,
                glyph_size_em.1 as f32 * font_size_f,
            )
        };

        // Research: Why trunc() and not round()?
        let cell_pixel_size = (glyph_size.0.trunc() as usize, glyph_size.1.trunc() as usize);

        let ascender_px =
            ((ascender_em as f32 * font_size_f).trunc() as usize).min(cell_pixel_size.1);

        let descender_px = cell_pixel_size.1 - ascender_px;

        Ok(Self {
            font,
            family_name,
            size,
            units_per_em: units_per_em.try_into().context("units per em")?,
            ascender_em,
            descender_em,
            glyph_advance_em: glyph_size_em.0,
            glyph_size,
            ascender_px,
            descender_px,
            glyph_advance_px: cell_pixel_size.0,
        })
    }

    pub fn cell_size_px(&self) -> (usize, usize) {
        (self.glyph_advance_px, self.font_height_px())
    }

    pub fn font_height_px(&self) -> usize {
        self.ascender_px + self.descender_px
    }
}
