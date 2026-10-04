//! Bigfont binary asset (format v1) — zero-copy parser.
//!
//! Normative spec: docs/bigfont-asset-format.md (written by the generator
//! tools/gen-bigfont). Layout: 24-byte header, `glyph_count` × 8-byte index
//! sorted by char, `glyph_count` × 24-byte packed bitmaps (12 rows × 2 bytes
//! big-endian, bit 15 − c = pixel column c), UTF-8 provenance trailer.
//! Integers are little-endian.
//!
//! Parsing is strict: every structural invariant (magic, version, geometry,
//! exact file size, sorted index, legal widths, zero reserved/flags) is
//! checked once in [`BigFontAsset::parse`], so lookups afterwards are
//! infallible and allocation-free.

use std::fmt;

pub const MAGIC: &[u8; 4] = b"MRF1";
pub const VERSION: u16 = 1;
pub const GLYPH_HEIGHT_PX: u16 = 12;
pub const WIDTH_FULL_PX: u16 = 12;
pub const WIDTH_HALF_PX: u16 = 6;
pub const ROW_BYTES: usize = 2;
pub const HEADER_LEN: usize = 24;
pub const INDEX_ENTRY_LEN: usize = 8;
pub const GLYPH_BYTES: usize = GLYPH_HEIGHT_PX as usize * ROW_BYTES;

#[derive(Debug, PartialEq, Eq)]
pub enum AssetError {
    TooShort,
    BadMagic,
    UnsupportedVersion(u16),
    BadGeometry { field: &'static str, value: u16 },
    SizeMismatch { declared: usize, actual: usize },
    BadProvenanceUtf8,
    IndexUnsorted { at: usize },
    BadWidth { at: usize, width: u16 },
    NonZeroReserved { at: usize, value: u16 },
}

impl fmt::Display for AssetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort => write!(f, "file shorter than the {HEADER_LEN}-byte header"),
            Self::BadMagic => write!(f, "magic is not {MAGIC:?} — not a bigfont asset"),
            Self::UnsupportedVersion(v) => {
                write!(f, "asset version {v}, this parser knows {VERSION}")
            }
            Self::BadGeometry { field, value } => write!(
                f,
                "header {field} is {value}, expected the format-v1 constant"
            ),
            Self::SizeMismatch { declared, actual } => {
                write!(
                    f,
                    "header declares {declared} bytes but the file has {actual}"
                )
            }
            Self::BadProvenanceUtf8 => write!(f, "provenance trailer is not valid UTF-8"),
            Self::IndexUnsorted { at } => {
                write!(f, "index entry {at} breaks the ascending char order")
            }
            Self::BadWidth { at, width } => write!(
                f,
                "index entry {at}: width {width} is neither {WIDTH_HALF_PX} nor {WIDTH_FULL_PX}"
            ),
            Self::NonZeroReserved { at, value } => write!(
                f,
                "index entry {at}: reserved flags are {value}, expected 0"
            ),
        }
    }
}

impl std::error::Error for AssetError {}

/// A parsed asset. Borrows the input bytes; no copy, no allocation per glyph.
#[derive(Debug)]
pub struct BigFontAsset<'a> {
    bytes: &'a [u8],
    glyph_count: usize,
    provenance: &'a str,
}

/// One glyph's bitmap: a borrowed 24-byte slice plus its width class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyph<'a> {
    width_px: u16,
    bitmap: &'a [u8],
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

impl<'a> BigFontAsset<'a> {
    /// Validates the whole structure once; after this, [`Self::lookup`] is
    /// infallible and allocation-free.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, AssetError> {
        if bytes.len() < HEADER_LEN {
            return Err(AssetError::TooShort);
        }
        if &bytes[0..4] != MAGIC {
            return Err(AssetError::BadMagic);
        }
        let version = le16(bytes, 4);
        if version != VERSION {
            return Err(AssetError::UnsupportedVersion(version));
        }
        for (at, field, want) in [
            (6, "glyph_height", GLYPH_HEIGHT_PX),
            (8, "width_full", WIDTH_FULL_PX),
            (10, "width_half", WIDTH_HALF_PX),
            (12, "row_bytes", ROW_BYTES as u16),
            (14, "reserved", 0),
        ] {
            let value = le16(bytes, at);
            if value != want {
                return Err(AssetError::BadGeometry { field, value });
            }
        }
        let glyph_count = le32(bytes, 16) as usize;
        let provenance_len = le32(bytes, 20) as usize;
        let declared = HEADER_LEN + glyph_count * (INDEX_ENTRY_LEN + GLYPH_BYTES) + provenance_len;
        if bytes.len() != declared {
            return Err(AssetError::SizeMismatch {
                declared,
                actual: bytes.len(),
            });
        }
        let provenance = std::str::from_utf8(&bytes[declared - provenance_len..])
            .map_err(|_| AssetError::BadProvenanceUtf8)?;

        let asset = Self {
            bytes,
            glyph_count,
            provenance,
        };
        let mut prev: Option<u32> = None;
        for i in 0..glyph_count {
            let ch = asset.char_at(i);
            if prev.is_some_and(|p| p >= ch) {
                return Err(AssetError::IndexUnsorted { at: i });
            }
            let width = asset.width_at(i);
            if width != WIDTH_HALF_PX && width != WIDTH_FULL_PX {
                return Err(AssetError::BadWidth { at: i, width });
            }
            let flags = le16(bytes, asset.index_at(i) + 6);
            if flags != 0 {
                return Err(AssetError::NonZeroReserved {
                    at: i,
                    value: flags,
                });
            }
            prev = Some(ch);
        }
        Ok(asset)
    }

    pub fn glyph_count(&self) -> usize {
        self.glyph_count
    }

    pub fn provenance(&self) -> &'a str {
        self.provenance
    }

    fn index_at(&self, i: usize) -> usize {
        HEADER_LEN + i * INDEX_ENTRY_LEN
    }

    fn bitmaps_at(&self, i: usize) -> usize {
        HEADER_LEN + self.glyph_count * INDEX_ENTRY_LEN + i * GLYPH_BYTES
    }

    fn char_at(&self, i: usize) -> u32 {
        le32(self.bytes, self.index_at(i))
    }

    fn width_at(&self, i: usize) -> u16 {
        le16(self.bytes, self.index_at(i) + 4)
    }

    /// Binary search for `ch`; the returned glyph borrows the asset bytes.
    pub fn lookup(&self, ch: char) -> Option<Glyph<'a>> {
        let target = ch as u32;
        let mut lo = 0usize;
        let mut hi = self.glyph_count;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match self.char_at(mid).cmp(&target) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => {
                    let base = self.bitmaps_at(mid);
                    return Some(Glyph {
                        width_px: self.width_at(mid),
                        bitmap: &self.bytes[base..base + GLYPH_BYTES],
                    });
                }
            }
        }
        None
    }

    /// Iterates every (char, glyph) pair in index order — for tests and
    /// diagnostics, not the render path.
    pub fn iter(&self) -> impl Iterator<Item = (char, Glyph<'a>)> + '_ {
        (0..self.glyph_count).map(|i| {
            let ch = char::from_u32(self.char_at(i)).expect("asset chars are valid scalars");
            let base = self.bitmaps_at(i);
            (
                ch,
                Glyph {
                    width_px: self.width_at(i),
                    bitmap: &self.bytes[base..base + GLYPH_BYTES],
                },
            )
        })
    }
}

impl Glyph<'_> {
    /// Glyph width in pixels: 6 (halfwidth) or 12 (fullwidth); at scale 1
    /// one pixel is one terminal column.
    pub fn width_px(&self) -> u16 {
        self.width_px
    }

    /// One packed bitmap row (bit 15 − c = pixel column c), `y` = 0 is the
    /// topmost pixel row.
    pub fn row(&self, y: usize) -> u16 {
        debug_assert!(y < GLYPH_HEIGHT_PX as usize);
        u16::from_be_bytes([self.bitmap[y * ROW_BYTES], self.bitmap[y * ROW_BYTES + 1]])
    }

    /// Pixel state at (`x`, `y`); `x` = 0 is the leftmost pixel column.
    pub fn pixel(&self, x: usize, y: usize) -> bool {
        debug_assert!(x < self.width_px as usize && y < GLYPH_HEIGHT_PX as usize);
        self.row(y) & (1 << (15 - x)) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bigfont::EMBEDDED_ZH_HANS;

    /// The committed zh-Hans atlas was written by tools/gen-bigfont and is
    /// the real round-trip partner of this parser (tool writes → parser
    /// reads → goldens below match the generator's own --dump output).
    fn committed() -> BigFontAsset<'static> {
        BigFontAsset::parse(EMBEDDED_ZH_HANS).expect("the committed asset must parse")
    }

    #[test]
    fn parses_committed_asset_header() {
        let asset = committed();
        assert_eq!(
            asset.glyph_count(),
            7188,
            "record in docs/bigfont-asset-format.md"
        );
        let prov = asset.provenance();
        assert!(prov.contains("release=2026.09.25"), "{prov}");
        assert!(
            prov.contains(
                "sha256=d75f5262f108757edb0f47ee8e3d2dfdfbecfd94558faf5ffb0c06dc5866fb3b"
            ),
            "{prov}"
        );
        assert!(prov.contains("variant=zh-hans"), "{prov}");
    }

    #[test]
    fn lookup_fullwidth_goldens() {
        let asset = committed();

        // 一 U+4E00: a single bar on pixel row 6, columns 0..=10.
        let yi = asset.lookup('一').expect("一 is in the charset");
        assert_eq!(yi.width_px(), WIDTH_FULL_PX);
        assert_eq!(yi.row(6), 0xFFE0);
        assert!(yi.row(0) == 0 && yi.row(5) == 0 && yi.row(7) == 0 && yi.row(11) == 0);

        // 口 U+53E3: hollow box, top bar row 2, sides rows 3..=9 at
        // columns 1 and 9, bottom bar row 10.
        let kou = asset.lookup('口').expect("口 is in the charset");
        assert_eq!(kou.width_px(), WIDTH_FULL_PX);
        assert_eq!(kou.row(2), 0x7FC0);
        assert_eq!(kou.row(10), 0x7FC0);
        for y in 3..10 {
            assert_eq!(kou.row(y), 0x4040, "side row {y}");
        }
        assert_eq!(kou.row(0), 0);
        assert_eq!(kou.row(11), 0);

        // 中 U+4E2D must exist (GB2312 level 1).
        assert!(asset.lookup('中').is_some());
    }

    #[test]
    fn lookup_halfwidth_goldens() {
        let asset = committed();

        // A U+0041 rows (from the generator --dump): stem, forks, crossbar,
        // legs — proves 6px glyphs pack into bits 15..=10 only.
        let a = asset.lookup('A').expect("ASCII is in the charset");
        assert_eq!(a.width_px(), WIDTH_HALF_PX);
        assert_eq!(a.row(2), 0x2000);
        assert_eq!(a.row(4), 0x5000);
        assert_eq!(a.row(6), 0x7000);
        assert_eq!(a.row(7), 0x8800);
        assert_eq!(a.row(9), 0x8800);
        assert_eq!(a.row(0), 0);
        assert_eq!(a.row(10), 0);

        // ! U+0021: stem rows 2..=7, gap, dot on the baseline row 9.
        let bang = asset.lookup('!').unwrap();
        assert_eq!(bang.width_px(), WIDTH_HALF_PX);
        for y in 2..8 {
            assert_eq!(bang.row(y), 0x2000, "stem row {y}");
        }
        assert_eq!(bang.row(8), 0);
        assert_eq!(bang.row(9), 0x2000);

        // Space is present and blank (not tofu).
        let space = asset.lookup(' ').unwrap();
        assert_eq!(space.width_px(), WIDTH_HALF_PX);
        assert!((0..12).all(|y| space.row(y) == 0));
    }

    #[test]
    fn lookup_misses_are_none() {
        let asset = committed();
        assert!(
            asset.lookup('🚀').is_none(),
            "emoji are outside the V1 charset"
        );
        assert!(
            asset.lookup('働').is_none(),
            "ja-only hanzi are not in GB2312/zh-Hans"
        );
        assert!(asset.lookup('\u{e100}').is_none(), "no PUA in the asset");
    }

    /// Sweep every glyph of the committed asset: widths are legal, unused
    /// bits are zero, and pixel() agrees with row() bit by bit.
    #[test]
    fn committed_asset_is_self_consistent() {
        let asset = committed();
        let mut half = 0usize;
        let mut full = 0usize;
        for (ch, g) in asset.iter() {
            match g.width_px() {
                WIDTH_HALF_PX => half += 1,
                WIDTH_FULL_PX => full += 1,
                w => panic!("glyph {ch:?}: illegal width {w}"),
            }
            let unused_mask = (1u16 << (16 - g.width_px())) - 1;
            for y in 0..GLYPH_HEIGHT_PX as usize {
                let row = g.row(y);
                assert_eq!(
                    row & unused_mask,
                    0,
                    "glyph {ch:?} row {y}: bits outside the canvas"
                );
                for x in 0..g.width_px() as usize {
                    assert_eq!(
                        g.pixel(x, y),
                        row & (1 << (15 - x)) != 0,
                        "glyph {ch:?} pixel ({x},{y})"
                    );
                }
            }
        }
        assert_eq!(half, 104, "record in docs/bigfont-asset-format.md");
        assert_eq!(full, 7084, "record in docs/bigfont-asset-format.md");
    }

    /// A minimal valid asset built byte by byte from the format doc, used to
    /// exercise the error paths (the committed asset only parses).
    fn synthetic(entries: &[(u32, u16)], provenance: &str) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&GLYPH_HEIGHT_PX.to_le_bytes());
        out.extend_from_slice(&WIDTH_FULL_PX.to_le_bytes());
        out.extend_from_slice(&WIDTH_HALF_PX.to_le_bytes());
        out.extend_from_slice(&(ROW_BYTES as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        out.extend_from_slice(&(provenance.len() as u32).to_le_bytes());
        for (ch, width) in entries {
            out.extend_from_slice(&ch.to_le_bytes());
            out.extend_from_slice(&width.to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
        }
        for _ in entries {
            out.extend_from_slice(&[0u8; GLYPH_BYTES]);
        }
        out.extend_from_slice(provenance.as_bytes());
        out
    }

    #[test]
    fn parses_minimal_synthetic_asset() {
        let bytes = synthetic(&[(65, WIDTH_HALF_PX), (0x4E00, WIDTH_FULL_PX)], "prov");
        let asset = BigFontAsset::parse(&bytes).unwrap();
        assert_eq!(asset.glyph_count(), 2);
        assert_eq!(asset.provenance(), "prov");
        assert!(asset.lookup('A').is_some());
        assert!(asset.lookup('一').is_some());
        assert!(asset.lookup('B').is_none());
    }

    #[test]
    fn rejects_corrupt_headers() {
        let base = synthetic(&[(65, WIDTH_HALF_PX)], "prov");

        let mut b = base.clone();
        b[0] = b'X';
        assert_eq!(BigFontAsset::parse(&b).unwrap_err(), AssetError::BadMagic);

        let mut b = base.clone();
        b[4] = 2;
        assert_eq!(
            BigFontAsset::parse(&b).unwrap_err(),
            AssetError::UnsupportedVersion(2)
        );

        assert_eq!(
            BigFontAsset::parse(&base[..10]).unwrap_err(),
            AssetError::TooShort
        );

        let mut b = base.clone();
        b[8] = 11; // width_full 12 -> 11
        assert!(matches!(
            BigFontAsset::parse(&b),
            Err(AssetError::BadGeometry {
                field: "width_full",
                value: 11
            })
        ));

        let mut b = base.clone();
        b.push(0); // trailing byte breaks the exact-size rule
        assert!(matches!(
            BigFontAsset::parse(&b),
            Err(AssetError::SizeMismatch { .. })
        ));

        let mut b = base.clone();
        let prov_at = b.len() - 4;
        b[prov_at] = 0xFF; // invalid UTF-8 start byte
        assert_eq!(
            BigFontAsset::parse(&b).unwrap_err(),
            AssetError::BadProvenanceUtf8
        );
    }

    #[test]
    fn rejects_bad_index_entries() {
        // Descending order.
        let bytes = synthetic(&[(0x4E00, WIDTH_FULL_PX), (65, WIDTH_HALF_PX)], "prov");
        assert_eq!(
            BigFontAsset::parse(&bytes).unwrap_err(),
            AssetError::IndexUnsorted { at: 1 }
        );

        // Duplicate char also breaks strict ascending order.
        let bytes = synthetic(&[(65, WIDTH_HALF_PX), (65, WIDTH_HALF_PX)], "prov");
        assert_eq!(
            BigFontAsset::parse(&bytes).unwrap_err(),
            AssetError::IndexUnsorted { at: 1 }
        );

        // Illegal width.
        let bytes = synthetic(&[(65, 9)], "prov");
        assert_eq!(
            BigFontAsset::parse(&bytes).unwrap_err(),
            AssetError::BadWidth { at: 0, width: 9 }
        );

        // Non-zero reserved flags.
        let mut bytes = synthetic(&[(65, WIDTH_HALF_PX)], "prov");
        bytes[HEADER_LEN + 6] = 1;
        assert_eq!(
            BigFontAsset::parse(&bytes).unwrap_err(),
            AssetError::NonZeroReserved { at: 0, value: 1 }
        );
    }
}
