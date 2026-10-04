//! Minimal BDF 2.1 reader: extracts exactly the fields the bigfont asset
//! pipeline needs (FONT_ASCENT/FONT_DESCENT/PIXEL_SIZE, and per glyph
//! ENCODING, DWIDTH, BBX and the packed BITMAP rows).

/// Glyph bounding box: `w`×`h` pixels with its lower-left corner at
/// (`xoff`, `yoff`) relative to the pen origin on the baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bbx {
    pub w: u32,
    pub h: u32,
    pub xoff: i32,
    pub yoff: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BdfGlyph {
    pub name: String,
    /// Unicode scalar value from `ENCODING` (ISO10646-1 fonts).
    pub encoding: u32,
    /// Device width in pixels: 6 (halfwidth) or 12 (fullwidth) in the
    /// fusion-pixel monospaced faces.
    pub dwidth_x: u32,
    pub bbx: Bbx,
    /// One entry per bitmap row, top row first; each row is
    /// `ceil(bbx.w / 8)` bytes, most significant bit of byte 0 = leftmost
    /// pixel of the row.
    pub rows: Vec<Vec<u8>>,
}

#[derive(Debug)]
pub struct BdfFont {
    pub ascent: i32,
    pub descent: i32,
    pub pixel_size: u32,
    pub glyphs: Vec<BdfGlyph>,
}

fn parse_i64(line: &str, field: &str, lineno: usize) -> Result<i64, String> {
    line.split_whitespace()
        .nth(1)
        .ok_or_else(|| format!("line {lineno}: {field} without a value"))?
        .parse::<i64>()
        .map_err(|_| format!("line {lineno}: {field} value is not an integer"))
}

fn hex_bytes(s: &str, lineno: usize) -> Result<Vec<u8>, String> {
    let s = s.trim();
    if !s.len().is_multiple_of(2) {
        return Err(format!(
            "line {lineno}: bitmap row has an odd number of hex digits"
        ));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|_| format!("line {lineno}: bitmap row is not hexadecimal"))
        })
        .collect()
}

pub fn parse(input: &str) -> Result<BdfFont, String> {
    let mut ascent: Option<i64> = None;
    let mut descent: Option<i64> = None;
    let mut pixel_size: Option<i64> = None;
    let mut glyphs: Vec<BdfGlyph> = Vec::new();

    let mut name: Option<String> = None;
    let mut encoding: Option<i64> = None;
    let mut dwidth_x: Option<i64> = None;
    let mut bbx: Option<Bbx> = None;
    let mut rows: Vec<Vec<u8>> = Vec::new();
    let mut rows_left: u32 = 0;
    let mut in_bitmap = false;
    let mut skip_glyph = false;

    for (idx, raw) in input.lines().enumerate() {
        let lineno = idx + 1;
        let line = raw.trim_end();

        if in_bitmap {
            if rows_left > 0 {
                if !skip_glyph {
                    rows.push(hex_bytes(line, lineno)?);
                }
                rows_left -= 1;
                continue;
            }
            if !line.starts_with("ENDCHAR") {
                return Err(format!("line {lineno}: expected ENDCHAR, found {line:?}"));
            }
            in_bitmap = false;
            if skip_glyph {
                skip_glyph = false;
                name = None;
                encoding = None;
                dwidth_x = None;
                bbx = None;
                rows.clear();
                continue;
            }
            let Some(bbox) = bbx else {
                return Err(format!("line {lineno}: ENDCHAR after BITMAP without BBX"));
            };
            let expected = bbox.w.div_ceil(8) as usize;
            if rows.iter().any(|r| r.len() != expected) {
                return Err(format!(
                    "line {lineno}: glyph {}: bitmap rows must be {expected} bytes (BBX width {})",
                    name.as_deref().unwrap_or("?"),
                    bbox.w
                ));
            }
            glyphs.push(BdfGlyph {
                name: name.take().unwrap_or_default(),
                encoding: encoding
                    .ok_or_else(|| format!("line {lineno}: glyph without ENCODING"))?
                    as u32,
                dwidth_x: dwidth_x.ok_or_else(|| format!("line {lineno}: glyph without DWIDTH"))?
                    as u32,
                bbx: bbox,
                rows: std::mem::take(&mut rows),
            });
            encoding = None;
            dwidth_x = None;
            bbx = None;
            continue;
        }

        if let Some(rest) = line.strip_prefix("STARTCHAR ") {
            name = Some(rest.trim().to_string());
        } else if line.starts_with("ENCODING ") {
            encoding = Some(parse_i64(line, "ENCODING", lineno)?);
        } else if line.starts_with("DWIDTH ") {
            dwidth_x = Some(parse_i64(line, "DWIDTH", lineno)?);
        } else if line.starts_with("BBX ") {
            let f: Vec<&str> = line.split_whitespace().collect();
            if f.len() != 5 {
                return Err(format!("line {lineno}: BBX needs 4 integers"));
            }
            bbx = Some(Bbx {
                w: f[1]
                    .parse()
                    .map_err(|_| format!("line {lineno}: BBX w is not an integer"))?,
                h: f[2]
                    .parse()
                    .map_err(|_| format!("line {lineno}: BBX h is not an integer"))?,
                xoff: f[3]
                    .parse()
                    .map_err(|_| format!("line {lineno}: BBX xoff is not an integer"))?,
                yoff: f[4]
                    .parse()
                    .map_err(|_| format!("line {lineno}: BBX yoff is not an integer"))?,
            });
        } else if line == "BITMAP" {
            let bbx = bbx.ok_or_else(|| format!("line {lineno}: BITMAP without BBX"))?;
            // Glyphs without a usable ENCODING/DWIDTH (e.g. ENCODING -1) are
            // consumed and dropped.
            skip_glyph = encoding.is_none_or(|e| e < 0) || dwidth_x.is_none();
            rows_left = bbx.h;
            rows.clear();
            in_bitmap = true;
        } else if line.starts_with("FONT_ASCENT ") {
            ascent = Some(parse_i64(line, "FONT_ASCENT", lineno)?);
        } else if line.starts_with("FONT_DESCENT ") {
            descent = Some(parse_i64(line, "FONT_DESCENT", lineno)?);
        } else if line.starts_with("PIXEL_SIZE ") {
            pixel_size = Some(parse_i64(line, "PIXEL_SIZE", lineno)?);
        }
    }

    if in_bitmap {
        return Err("file ends inside a glyph bitmap".to_string());
    }

    Ok(BdfFont {
        ascent: ascent.ok_or("missing FONT_ASCENT")? as i32,
        descent: descent.ok_or("missing FONT_DESCENT")? as i32,
        pixel_size: pixel_size.ok_or("missing PIXEL_SIZE")? as u32,
        glyphs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = "\
STARTFONT 2.1
FONT -Test-Mono-Regular-R-Normal--12-120-75-75-M-118-ISO10646-1
SIZE 12 75 75
FONTBOUNDINGBOX 12 12 0 -2
STARTPROPERTIES 3
PIXEL_SIZE 12
FONT_ASCENT 10
FONT_DESCENT 2
ENDPROPERTIES
CHARS 3
STARTCHAR space
ENCODING 32
SWIDTH 500 0
DWIDTH 6 0
BBX 0 0 0 0
BITMAP
ENDCHAR
STARTCHAR u4E00
ENCODING 19968
SWIDTH 1000 0
DWIDTH 12 0
BBX 11 1 0 3
BITMAP
FFE0
ENDCHAR
STARTCHAR exclam
ENCODING 33
SWIDTH 500 0
DWIDTH 6 0
BBX 1 8 2 0
BITMAP
80
80
80
80
80
80
00
80
ENDCHAR
ENDFONT
";

    #[test]
    fn parses_header_properties() {
        let font = parse(FIXTURE).unwrap();
        assert_eq!(font.ascent, 10);
        assert_eq!(font.descent, 2);
        assert_eq!(font.pixel_size, 12);
    }

    #[test]
    fn parses_glyphs_in_order() {
        let font = parse(FIXTURE).unwrap();
        assert_eq!(font.glyphs.len(), 3);

        let space = &font.glyphs[0];
        assert_eq!(space.encoding, 32);
        assert_eq!(space.dwidth_x, 6);
        assert_eq!(
            space.bbx,
            Bbx {
                w: 0,
                h: 0,
                xoff: 0,
                yoff: 0
            }
        );
        assert!(space.rows.is_empty());

        let yi = &font.glyphs[1];
        assert_eq!(yi.encoding, 0x4E00);
        assert_eq!(yi.dwidth_x, 12);
        assert_eq!(
            yi.bbx,
            Bbx {
                w: 11,
                h: 1,
                xoff: 0,
                yoff: 3
            }
        );
        assert_eq!(yi.rows, vec![vec![0xFF, 0xE0]]);

        let exclam = &font.glyphs[2];
        assert_eq!(exclam.encoding, 33);
        assert_eq!(exclam.dwidth_x, 6);
        assert_eq!(exclam.rows.len(), 8);
        assert_eq!(exclam.rows[6], vec![0x00]);
    }

    #[test]
    fn skips_glyphs_with_negative_encoding() {
        let input = "\
STARTFONT 2.1
PIXEL_SIZE 12
FONT_ASCENT 10
FONT_DESCENT 2
STARTCHAR nonchar
ENCODING -1
DWIDTH 12 0
BBX 12 12 0 -2
BITMAP
0000
0000
0000
0000
0000
0000
0000
0000
0000
0000
0000
0000
ENDCHAR
ENDFONT
";
        let font = parse(input).unwrap();
        assert!(font.glyphs.is_empty());
    }

    #[test]
    fn rejects_bitmap_row_with_wrong_byte_count() {
        let input = "\
STARTFONT 2.1
PIXEL_SIZE 12
FONT_ASCENT 10
FONT_DESCENT 2
STARTCHAR bad
ENCODING 65
DWIDTH 6 0
BBX 4 2 0 0
BITMAP
F0F0
F0
ENDCHAR
ENDFONT
";
        assert!(parse(input).unwrap_err().contains("must be 1 bytes"));
    }

    #[test]
    fn rejects_missing_properties() {
        let input = "STARTFONT 2.1\nFONT_ASCENT 10\nFONT_DESCENT 2\n";
        assert!(parse(input).unwrap_err().contains("PIXEL_SIZE"));
    }
}
