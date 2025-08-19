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
    pub glyph_size_em: (usize, usize),
    pub glyph_size: (f32, f32),
    pub cell_pixel_size: (usize, usize),
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
            let glyph_height = hb_font.height().try_into().context("Font height")?;
            (glyph_width, glyph_height)
        };

        let units_per_em = hb_font.units_per_em();
        let glyph_size = {
            let f = size / units_per_em as f32;
            (glyph_size_em.0 as f32 * f, glyph_size_em.1 as f32 * f)
        };

        // Research: Why trunc() and not round()?
        let cell_pixel_size = (glyph_size.0.trunc() as usize, glyph_size.1.trunc() as usize);

        Ok(Self {
            font,
            family_name,
            size,
            units_per_em: units_per_em.try_into().context("units per em")?,
            glyph_size_em,
            glyph_size,
            cell_pixel_size,
        })
    }
}
