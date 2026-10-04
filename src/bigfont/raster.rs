//! Half-block rasterization (docs/big-font-mode.md §4): one terminal cell
//! carries two vertical pixels via `█ ▀ ▄` and space. A glyph of `w`×12
//! pixels at scale `s` occupies `w*s` cell columns and `6*s` cell rows;
//! cell row `R` samples pixel row `2R/s` (top half) and `(2R+1)/s` (bottom
//! half), cell column `C` samples pixel column `C/s`.

use super::ResolvedGlyph;
use super::asset::GLYPH_HEIGHT_PX;

/// One terminal cell of a big-font strip: a vertical pair of pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cell {
    Blank,
    Top,
    Bottom,
    Full,
}

impl Cell {
    pub fn from_pixels(top: bool, bottom: bool) -> Self {
        match (top, bottom) {
            (true, true) => Self::Full,
            (true, false) => Self::Top,
            (false, true) => Self::Bottom,
            (false, false) => Self::Blank,
        }
    }

    /// The half-block character for this cell (§4 table).
    pub fn to_char(self) -> char {
        match self {
            Self::Blank => ' ',
            Self::Top => '▀',
            Self::Bottom => '▄',
            Self::Full => '█',
        }
    }
}

/// Cell columns one glyph occupies at `scale`.
pub fn cell_cols(width_px: u16, scale: usize) -> usize {
    width_px as usize * scale
}

/// Cell rows any glyph occupies at `scale`: 12 pixel rows, 2 per cell row.
pub fn cell_rows(scale: usize) -> usize {
    (GLYPH_HEIGHT_PX as usize / 2) * scale
}

/// Row-buffer rasterizer: renders one cell row at a time into a reused
/// internal buffer, so the render path allocates nothing per glyph once the
/// buffer is warm (the T-12 renderer copies rows into its frame).
#[derive(Debug, Default)]
pub struct Rasterizer {
    row: Vec<Cell>,
}

impl Rasterizer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Renders cell row `row` of `glyph` at `scale` into the reused buffer
    /// and returns it. The slice is `cell_cols(width_px, scale)` long and
    /// stays valid until the next `render_row` call.
    ///
    /// Panics in debug builds if `scale == 0` or `row >= cell_rows(scale)`.
    pub fn render_row<'r>(
        &'r mut self,
        glyph: &ResolvedGlyph<'_>,
        scale: usize,
        row: usize,
    ) -> &'r [Cell] {
        debug_assert!(scale >= 1, "scale must be >= 1");
        debug_assert!(
            row < cell_rows(scale),
            "cell row {row} out of range at scale {scale}"
        );
        let cols = cell_cols(glyph.width_px(), scale);
        let top_px = (2 * row) / scale;
        let bottom_px = (2 * row + 1) / scale;
        self.row.resize(cols, Cell::Blank);
        for (c, cell) in self.row.iter_mut().enumerate() {
            let x = c / scale;
            *cell = Cell::from_pixels(glyph.pixel(x, top_px), glyph.pixel(x, bottom_px));
        }
        &self.row
    }

    /// The whole glyph as `█▀▄ `/newline text — tests and diagnostics only
    /// (allocates; the render path uses [`Self::render_row`]).
    pub fn render_text(glyph: &ResolvedGlyph<'_>, scale: usize) -> String {
        let mut raster = Self::new();
        let mut out = String::new();
        for row in 0..cell_rows(scale) {
            for cell in raster.render_row(glyph, scale, row) {
                out.push(cell.to_char());
            }
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bigfont::BigFontFace;

    fn face() -> BigFontFace<'static> {
        BigFontFace::embedded_zh_hans().unwrap()
    }

    fn rendered(g: &ResolvedGlyph<'_>, scale: usize) -> Vec<String> {
        Rasterizer::render_text(g, scale)
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn cell_char_table() {
        assert_eq!(Cell::from_pixels(false, false).to_char(), ' ');
        assert_eq!(Cell::from_pixels(true, false).to_char(), '▀');
        assert_eq!(Cell::from_pixels(false, true).to_char(), '▄');
        assert_eq!(Cell::from_pixels(true, true).to_char(), '█');
    }

    #[test]
    fn geometry_math() {
        assert_eq!(cell_cols(12, 1), 12);
        assert_eq!(cell_cols(6, 1), 6);
        assert_eq!(cell_cols(12, 2), 24);
        assert_eq!(cell_cols(6, 3), 18);
        assert_eq!(cell_rows(1), 6);
        assert_eq!(cell_rows(2), 12);
        assert_eq!(cell_rows(3), 18);
    }

    #[test]
    fn golden_yi_scale_1() {
        // 一: one bar on pixel row 6 → cell row 3 shows it as ▀ (row 7 off).
        let g = face().resolve_char('一');
        assert_eq!(
            rendered(&g, 1),
            [
                "            ",
                "            ",
                "            ",
                "▀▀▀▀▀▀▀▀▀▀▀ ",
                "            ",
                "            ",
            ]
        );
    }

    #[test]
    fn golden_kou_scale_1() {
        // 口: hollow box — top bar pixel row 2, sides rows 3..=9, bottom
        // row 10; every half-block state appears.
        let g = face().resolve_char('口');
        assert_eq!(
            rendered(&g, 1),
            [
                "            ",
                " █▀▀▀▀▀▀▀█  ",
                " █       █  ",
                " █       █  ",
                " █       █  ",
                " ▀▀▀▀▀▀▀▀▀  ",
            ]
        );
    }

    #[test]
    fn golden_halfwidth_a_scale_1() {
        // A: halfwidth (6 columns), legs on the baseline row 9.
        let g = face().resolve_char('A');
        assert_eq!(
            rendered(&g, 1),
            ["      ", "  █   ", " █ █  ", "▄▀▀▀▄ ", "█   █ ", "      ",]
        );
    }

    #[test]
    fn golden_yi_scale_2() {
        // At scale 2 each pixel row is one full-block cell row (12 rows).
        let g = face().resolve_char('一');
        let rows = rendered(&g, 2);
        assert_eq!(rows.len(), 12);
        for (r, row) in rows.iter().enumerate() {
            assert_eq!(row.chars().count(), 24, "row {r}");
            if r == 6 {
                assert_eq!(row, "██████████████████████  ");
            } else {
                assert_eq!(row, "                        ", "row {r} should be blank");
            }
        }
    }

    #[test]
    fn golden_kou_scale_2() {
        let g = face().resolve_char('口');
        let rows = rendered(&g, 2);
        assert_eq!(rows.len(), 12);
        let bar = "  ██████████████████    ";
        let side = "  ██              ██    ";
        for (r, row) in rows.iter().enumerate() {
            assert_eq!(row.chars().count(), 24, "row {r}");
            let expected = match r {
                2 | 10 => bar,
                3..=9 => side,
                _ => "                        ",
            };
            assert_eq!(row, expected, "row {r}");
        }
    }

    #[test]
    fn golden_tofu_scale_1() {
        // Missing glyph → 12×12 hollow box, fullwidth, layout unchanged.
        let g = face().resolve_char('🚀');
        assert!(g.is_tofu());
        assert_eq!(
            rendered(&g, 1),
            [
                "█▀▀▀▀▀▀▀▀▀▀█",
                "█          █",
                "█          █",
                "█          █",
                "█          █",
                "█▄▄▄▄▄▄▄▄▄▄█",
            ]
        );
    }

    #[test]
    fn odd_scale_uses_smooth_half_rows() {
        // At scale 3 (18 cell rows) pixel row 6 spans cell rows 9 (full,
        // top=bottom=6) and 10 (▀, top=6 bottom=7) — no jagged doubling.
        let g = face().resolve_char('一');
        let rows = rendered(&g, 3);
        assert_eq!(rows.len(), 18);
        for (r, row) in rows.iter().enumerate() {
            assert_eq!(row.chars().count(), 36, "row {r}");
            let expected = match r {
                9 => "█████████████████████████████████   ",
                10 => "▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀   ",
                _ => "                                    ",
            };
            assert_eq!(row, expected, "row {r}");
        }
    }

    #[test]
    fn strip_layout_mixes_widths_without_gaps() {
        // Σ cell_cols over a mixed run — tofu counts as a fullwidth cell
        // block, so the strip never collapses or drifts (T-11 consumes
        // exactly this sum).
        let f = face();
        let run = ['中', 'A', '🚀', ' ', '口'];
        let total: usize = run
            .iter()
            .map(|c| cell_cols(f.resolve_char(*c).width_px(), 2))
            .sum();
        assert_eq!(total, (12 + 6 + 12 + 6 + 12) * 2);
    }

    #[test]
    fn row_buffer_is_reused_across_widths() {
        let f = face();
        let mut raster = Rasterizer::new();
        let full = f.resolve_char('口');
        let half = f.resolve_char('!');
        assert_eq!(raster.render_row(&full, 1, 1).len(), 12);
        assert_eq!(raster.render_row(&half, 1, 4).len(), 6);
        assert_eq!(raster.render_row(&full, 2, 3).len(), 24);
        // Content correctness after shrinking and growing:
        let row = raster.render_row(&half, 1, 4).to_vec();
        assert_eq!(
            row,
            vec![
                Cell::Blank,
                Cell::Blank,
                Cell::Bottom,
                Cell::Blank,
                Cell::Blank,
                Cell::Blank
            ]
        );
    }
}
