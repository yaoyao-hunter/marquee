//! The bigfont binary asset format (version 1) — writer, self-check reader
//! and glyph canvas composition. The normative description lives in
//! docs/bigfont-asset-format.md; keep the two in sync.
//!
//! Layout (integers little-endian, bitmap row words big-endian):
//!
//! ```text
//! header   24 B   magic b"MRF1", version, glyph_height, width_full,
//!                  width_half, row_bytes, reserved, glyph_count,
//!                  provenance_len
//! index    glyph_count × 8 B   char u32, width_px u16 (6|12), flags u16 = 0
//!                              — sorted strictly ascending by char
//! bitmaps  glyph_count × glyph_height × row_bytes (= 24 B)
//!                              — row words: bit (15 - c) = pixel column c,
//!                              unused bits (halfwidth rows, padding) are 0
//! provenance  provenance_len B of UTF-8 (font release tag + sha256 + variant)
//! ```

use crate::bdf::BdfGlyph;

pub const MAGIC: &[u8; 4] = b"MRF1";
pub const VERSION: u16 = 1;
pub const GLYPH_HEIGHT: u16 = 12;
pub const WIDTH_FULL: u16 = 12;
pub const WIDTH_HALF: u16 = 6;
pub const ROW_BYTES: u16 = 2;
pub const HEADER_LEN: usize = 24;
pub const INDEX_ENTRY_LEN: usize = 8;
pub const GLYPH_BYTES: usize = GLYPH_HEIGHT as usize * ROW_BYTES as usize;

/// One glyph ready for the index + bitmap sections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlyphOut {
    pub ch: char,
    pub width_px: u16,
    /// Canvas rows top to bottom; bit (15 - c) is pixel column c.
    pub rows: [u16; GLYPH_HEIGHT as usize],
}

/// Place a BDF glyph onto its `width_px` × 12 canvas. BDF row `i` (top
/// first) covers y = `yoff + h - 1 - i`; canvas row = `ascent - 1 - y`.
/// Returns the canvas and the number of set pixels that fell outside the
/// canvas (clipped).
pub fn compose(g: &BdfGlyph, ascent: i32, width_px: u16) -> ([u16; GLYPH_HEIGHT as usize], u32) {
    let mut rows = [0u16; GLYPH_HEIGHT as usize];
    let mut clipped = 0u32;
    let h = g.bbx.h as i32;
    for (i, row_bytes) in g.rows.iter().enumerate() {
        let y = g.bbx.yoff + h - 1 - i as i32;
        let r = ascent - 1 - y;
        if !(0..i32::from(GLYPH_HEIGHT)).contains(&r) {
            clipped += row_bytes.iter().map(|b| b.count_ones()).sum::<u32>();
            continue;
        }
        let mut word = 0u16;
        for (j, byte) in row_bytes.iter().enumerate() {
            for bit in 0..8u32 {
                if byte & (0x80 >> bit) == 0 {
                    continue;
                }
                let x = g.bbx.xoff + j as i32 * 8 + bit as i32;
                if x < 0 || x >= i32::from(width_px) {
                    clipped += 1;
                    continue;
                }
                word |= 1u16 << (15 - x);
            }
        }
        rows[r as usize] = word;
    }
    (rows, clipped)
}

/// Serialize sorted glyphs into the asset byte string.
pub fn write_asset(glyphs: &[GlyphOut], provenance: &str) -> Result<Vec<u8>, String> {
    if glyphs.windows(2).any(|w| w[0].ch >= w[1].ch) {
        return Err("glyphs must be sorted by char, strictly ascending".to_string());
    }
    for g in glyphs {
        if g.width_px != WIDTH_HALF && g.width_px != WIDTH_FULL {
            return Err(format!(
                "glyph {:?}: width_px must be {WIDTH_HALF} or {WIDTH_FULL}",
                g.ch
            ));
        }
        // Set pixels must stay inside the glyph's columns: a row word uses
        // bit (15 - c) for column c, so every set bit must sit at
        // position >= 16 - width_px.
        let min_bit = 16 - u32::from(g.width_px);
        for (r, row) in g.rows.iter().enumerate() {
            if row.trailing_zeros() < min_bit {
                return Err(format!(
                    "glyph {:?} row {r}: pixels outside the {}-column canvas",
                    g.ch, g.width_px
                ));
            }
        }
    }

    let mut out = Vec::with_capacity(
        HEADER_LEN + glyphs.len() * (INDEX_ENTRY_LEN + GLYPH_BYTES) + provenance.len(),
    );
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&GLYPH_HEIGHT.to_le_bytes());
    out.extend_from_slice(&WIDTH_FULL.to_le_bytes());
    out.extend_from_slice(&WIDTH_HALF.to_le_bytes());
    out.extend_from_slice(&ROW_BYTES.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(glyphs.len() as u32).to_le_bytes());
    out.extend_from_slice(&(provenance.len() as u32).to_le_bytes());

    for g in glyphs {
        out.extend_from_slice(&(g.ch as u32).to_le_bytes());
        out.extend_from_slice(&g.width_px.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
    }
    for g in glyphs {
        for row in &g.rows {
            out.extend_from_slice(&row.to_be_bytes());
        }
    }
    out.extend_from_slice(provenance.as_bytes());
    Ok(out)
}

/// Read-back view used for the post-generation self-check and `--dump`.
pub struct AssetView<'a> {
    bytes: &'a [u8],
    glyph_count: usize,
    provenance: &'a str,
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

impl<'a> AssetView<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, String> {
        if bytes.len() < HEADER_LEN || &bytes[0..4] != MAGIC {
            return Err("not a bigfont asset (bad magic or too short)".to_string());
        }
        if le16(bytes, 4) != VERSION {
            return Err(format!("unsupported asset version {}", le16(bytes, 4)));
        }
        if le16(bytes, 6) != GLYPH_HEIGHT
            || le16(bytes, 8) != WIDTH_FULL
            || le16(bytes, 10) != WIDTH_HALF
            || le16(bytes, 12) != ROW_BYTES
            || le16(bytes, 14) != 0
        {
            return Err("header geometry does not match format v1 constants".to_string());
        }
        let glyph_count = le32(bytes, 16) as usize;
        let provenance_len = le32(bytes, 20) as usize;
        let expected = HEADER_LEN + glyph_count * (INDEX_ENTRY_LEN + GLYPH_BYTES) + provenance_len;
        if bytes.len() != expected {
            return Err(format!(
                "file size {} does not match declared layout {expected}",
                bytes.len()
            ));
        }
        let provenance = std::str::from_utf8(&bytes[expected - provenance_len..])
            .map_err(|_| "provenance is not valid UTF-8")?;
        let view = Self {
            bytes,
            glyph_count,
            provenance,
        };
        let mut prev: Option<u32> = None;
        for i in 0..glyph_count {
            let c = view.char_at(i);
            if prev.is_some_and(|p| p >= c) {
                return Err(format!("index not sorted at entry {i}"));
            }
            prev = Some(c);
        }
        Ok(view)
    }

    fn index_at(&self, i: usize) -> usize {
        HEADER_LEN + i * INDEX_ENTRY_LEN
    }

    pub fn glyph_count(&self) -> usize {
        self.glyph_count
    }

    pub fn provenance(&self) -> &str {
        self.provenance
    }

    pub fn char_at(&self, i: usize) -> u32 {
        le32(self.bytes, self.index_at(i))
    }

    pub fn width_at(&self, i: usize) -> u16 {
        le16(self.bytes, self.index_at(i) + 4)
    }

    /// Bitmap rows of glyph `i`, top row first, bit (15 - c) = column c.
    pub fn rows_at(&self, i: usize) -> impl Iterator<Item = u16> + '_ {
        let base = HEADER_LEN + self.glyph_count * INDEX_ENTRY_LEN + i * GLYPH_BYTES;
        (0..GLYPH_HEIGHT as usize).map(move |r| {
            u16::from_be_bytes([
                self.bytes[base + r * ROW_BYTES as usize],
                self.bytes[base + r * ROW_BYTES as usize + 1],
            ])
        })
    }

    pub fn find(&self, ch: char) -> Option<usize> {
        let target = ch as u32;
        let mut lo = 0usize;
        let mut hi = self.glyph_count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match self.char_at(mid).cmp(&target) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(mid),
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bdf::{Bbx, BdfGlyph};

    fn glyph(encoding: u32, dwidth_x: u32, bbx: Bbx, rows: Vec<&str>) -> BdfGlyph {
        BdfGlyph {
            name: format!("u{encoding:04X}"),
            encoding,
            dwidth_x,
            bbx,
            rows: rows
                .iter()
                .map(|r| {
                    (0..r.len())
                        .step_by(2)
                        .map(|i| u8::from_str_radix(&r[i..i + 2], 16).unwrap())
                        .collect()
                })
                .collect(),
        }
    }

    #[test]
    fn compose_places_rows_by_ascent_and_yoff() {
        // 一-like: a single full row of 11 bits at yoff 3 → canvas row 6.
        let g = glyph(
            0x4E00,
            12,
            Bbx {
                w: 11,
                h: 1,
                xoff: 0,
                yoff: 3,
            },
            vec!["FFE0"],
        );
        let (rows, clipped) = compose(&g, 10, WIDTH_FULL);
        assert_eq!(clipped, 0);
        assert_eq!(rows.iter().filter(|&&r| r != 0).count(), 1);
        // columns 0..11 set: bits 15..5 (11 of the 12 columns)
        assert_eq!(rows[6], 0xFFE0);
    }

    #[test]
    fn compose_maps_halfwidth_bits_to_high_columns() {
        // !-like: 1 pixel wide at xoff 2, rows y = 0..7 → canvas rows 2..9.
        let g = glyph(
            33,
            6,
            Bbx {
                w: 1,
                h: 8,
                xoff: 2,
                yoff: 0,
            },
            vec!["80", "80", "80", "80", "80", "80", "00", "80"],
        );
        let (rows, clipped) = compose(&g, 10, WIDTH_HALF);
        assert_eq!(clipped, 0);
        // column 2 → bit 13
        assert_eq!(rows[2], 0b0010_0000_0000_0000);
        assert_eq!(rows[8], 0);
        assert_eq!(rows[9], 0b0010_0000_0000_0000);
        assert_eq!(rows.iter().filter(|&&r| r != 0).count(), 7);
    }

    #[test]
    fn compose_counts_clipped_pixels() {
        // A row above the canvas (yoff too high) and a column beyond a
        // halfwidth canvas are both dropped but counted.
        let g = glyph(
            65,
            6,
            Bbx {
                w: 2,
                h: 1,
                xoff: 5,
                yoff: 9,
            },
            vec!["C0"],
        );
        let (rows, clipped) = compose(&g, 10, WIDTH_HALF);
        // y = 9 → canvas row 0 ✓; x = 5 ✓, x = 6 ✗ (halfwidth canvas)
        assert_eq!(rows[0], 1 << (15 - 5));
        assert_eq!(clipped, 1);

        let g2 = glyph(
            66,
            12,
            Bbx {
                w: 12,
                h: 1,
                xoff: 0,
                yoff: 10,
            },
            vec!["FFF0"],
        );
        let (rows2, clipped2) = compose(&g2, 10, WIDTH_FULL);
        // y = 10 → canvas row -1 → whole row clipped
        assert!(rows2.iter().all(|&r| r == 0));
        assert_eq!(clipped2, 12);
    }

    #[test]
    fn write_then_parse_round_trips() {
        let g1 = glyph(
            0x4E00,
            12,
            Bbx {
                w: 11,
                h: 1,
                xoff: 0,
                yoff: 3,
            },
            vec!["FFE0"],
        );
        let (rows1, _) = compose(&g1, 10, WIDTH_FULL);
        let g2 = glyph(
            33,
            6,
            Bbx {
                w: 1,
                h: 8,
                xoff: 2,
                yoff: 0,
            },
            vec!["80", "80", "80", "80", "80", "80", "00", "80"],
        );
        let (rows2, _) = compose(&g2, 10, WIDTH_HALF);
        let glyphs = vec![
            GlyphOut {
                ch: '!',
                width_px: WIDTH_HALF,
                rows: rows2,
            },
            GlyphOut {
                ch: '一',
                width_px: WIDTH_FULL,
                rows: rows1,
            },
        ];
        let prov = "source=test release=0 sha256=abc variant=zh-hans generator=test 0";
        let bytes = write_asset(&glyphs, prov).unwrap();
        assert_eq!(
            bytes.len(),
            HEADER_LEN + 2 * (INDEX_ENTRY_LEN + GLYPH_BYTES) + prov.len()
        );

        let view = AssetView::parse(&bytes).unwrap();
        assert_eq!(view.glyph_count(), 2);
        assert_eq!(view.provenance(), prov);
        assert_eq!(view.find('!'), Some(0));
        assert_eq!(view.find('一'), Some(1));
        assert_eq!(view.find('口'), None);
        assert_eq!(view.width_at(0), WIDTH_HALF);
        assert_eq!(view.width_at(1), WIDTH_FULL);
        let got1: Vec<u16> = view.rows_at(0).collect();
        assert_eq!(got1, rows2.to_vec());
        let got2: Vec<u16> = view.rows_at(1).collect();
        assert_eq!(got2, rows1.to_vec());
    }

    #[test]
    fn write_rejects_unsorted_and_bad_width() {
        let empty = [0u16; GLYPH_HEIGHT as usize];
        let unsorted = vec![
            GlyphOut {
                ch: '一',
                width_px: WIDTH_FULL,
                rows: empty,
            },
            GlyphOut {
                ch: '!',
                width_px: WIDTH_HALF,
                rows: empty,
            },
        ];
        assert!(write_asset(&unsorted, "p").is_err());

        let bad_width = vec![GlyphOut {
            ch: '!',
            width_px: 9,
            rows: empty,
        }];
        assert!(write_asset(&bad_width, "p").is_err());
    }

    #[test]
    fn write_rejects_pixels_outside_canvas() {
        let mut rows = [0u16; GLYPH_HEIGHT as usize];
        rows[0] = 1 << (15 - 7); // column 7 — outside a 6-column canvas
        let glyphs = vec![GlyphOut {
            ch: '!',
            width_px: WIDTH_HALF,
            rows,
        }];
        assert!(write_asset(&glyphs, "p").is_err());
    }

    #[test]
    fn parse_rejects_corrupt_files() {
        let glyphs = vec![GlyphOut {
            ch: 'a',
            width_px: WIDTH_HALF,
            rows: [0; GLYPH_HEIGHT as usize],
        }];
        let mut bytes = write_asset(&glyphs, "prov").unwrap();
        assert!(AssetView::parse(&bytes).is_ok());
        bytes[0] = b'X';
        assert!(AssetView::parse(&bytes).is_err()); // magic
        let mut bytes = write_asset(&glyphs, "prov").unwrap();
        bytes.truncate(bytes.len() - 1);
        assert!(AssetView::parse(&bytes).is_err()); // size mismatch
        let mut bytes = write_asset(&glyphs, "prov").unwrap();
        bytes[4] = 99;
        assert!(AssetView::parse(&bytes).is_err()); // version
    }
}
