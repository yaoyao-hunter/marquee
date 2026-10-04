# Bigfont asset format (v1)

`assets/bigfont-12px-<variant>.bin` is the compact glyph atlas that big-font
mode embeds with `include_bytes!` (`src/bigfont/asset.rs`).
It is generated at development time from the official
[Fusion Pixel Font](https://github.com/TakWolf/fusion-pixel-font) BDF release
by [`tools/gen-bigfont`](../tools/gen-bigfont) and committed to the repo, so
builds stay offline and reproducible (design doc: `big-font-mode.md` §3).

The format is designed for zero-copy parsing: fixed-size records, no
compression, no stored offsets, one sorted index for binary search.

All integers are **little-endian**. Bitmap row words are stored
**big-endian** (bit order below).

## Layout

| Section    | Size                        | Contents                                    |
|------------|-----------------------------|---------------------------------------------|
| Header     | 24 B                        | magic, version, geometry, counts            |
| Index      | `glyph_count` × 8 B         | one record per glyph, sorted by char        |
| Bitmaps    | `glyph_count` × 24 B        | `glyph_height` (12) rows × `row_bytes` (2)  |
| Provenance | `provenance_len` B          | UTF-8: source release tag + sha256 + variant + generator |

### Header (24 bytes)

| Bytes   | Field           | Value / meaning                              |
|---------|-----------------|----------------------------------------------|
| `[0..4)`   | magic        | `MRF1`                                       |
| `[4..6)`   | version      | u16 = 1                                      |
| `[6..8)`   | glyph_height | u16 = 12 pixel rows per glyph                |
| `[8..10)`  | width_full   | u16 = 12 pixel columns of a fullwidth glyph  |
| `[10..12)` | width_half   | u16 = 6 pixel columns of a halfwidth glyph   |
| `[12..14)` | row_bytes    | u16 = 2 packed bytes per bitmap row          |
| `[14..16)` | reserved     | u16 = 0                                      |
| `[16..20)` | glyph_count  | u32                                          |
| `[20..24)` | provenance_len | u32, byte length of the trailing UTF-8 string |

### Index record (8 bytes, `i`-th glyph)

| Bytes   | Field     | Meaning                                             |
|---------|-----------|-----------------------------------------------------|
| `[0..4)` | char     | u32 Unicode scalar value; records strictly ascending → binary search |
| `[4..6)` | width_px | u16: 6 (halfwidth) or 12 (fullwidth)                |
| `[6..8)` | flags    | u16 = 0, reserved                                   |

### Bitmaps

Glyph `i` occupies 24 bytes at `24 + glyph_count*8 + i*24`; offsets are
derived from the record position, never stored. Each of the 12 rows is one
2-byte big-endian word: **bit (15 − c) is pixel column c**, column 0 =
leftmost, row 0 = topmost. Halfwidth glyphs use columns 0..=5 (bits 15..=10);
all unused bits are 0.

Canvas placement: a BDF pixel at glyph coordinates `(x, y)` (y up from the
baseline) lands on canvas row `FONT_ASCENT − 1 − y`, column `x`. Pixels
outside the 12-row canvas or the glyph's width are clipped (the generator
counts and reports them).

### Deviation note (design doc §3.1)

The design sketch says "halfwidth 12B/字" while also fixing rows at "2 字节打包
（大端）… 按行 2 字节对齐存储，简单优先". The two readings disagree; this format
follows the 2-byte-row rule for **both** width classes — a uniform 24 B
stride keeps offsets derivable and the parser branch-free. Cost of the
redundancy: ASCII glyphs carry 12 unused bytes each (~1.2 KB total).

## Character set (V1, zh-Hans)

Per design doc §3.2: printable ASCII (U+0020–007E), GB2312 zones 1–5
(general punctuation, numbers, fullwidth ASCII, hiragana, katakana) and
zones 16–87 (level 1+2 hanzi, exactly 6763), CJK symbols and punctuation
(U+3000–303F), hiragana (U+3041–309F), katakana (U+30A1–30FF) and fullwidth
forms (U+FF01–FF60). GB2312 cells are enumerated by decoding every two-byte
cell of the included zones via encoding_rs's GBK; GBK best-fit mappings into
the private use area (undefined zones 10–15) are rejected and never enter the
asset. Emoji, Greek, Cyrillic and box drawing are outside V1; characters
absent from the BDF fall back to tofu at runtime (design doc §3.3).

## Provenance and reproducibility

The provenance string records everything needed to rebuild the asset:

```
source=fusion-pixel-font release=<tag> file=<zip-name> sha256=<zip-sha256> variant=<variant> generator=gen-bigfont <tool-version>
```

The tool refuses any zip whose sha256 differs from the pinned constant in
`tools/gen-bigfont/src/main.rs`, so a green run always means "generated from
exactly the recorded font release". Generation is deterministic (no
timestamps): two runs over the same zip produce byte-identical assets —
verified for the record below. After writing, the tool re-parses the file and
compares every index entry and bitmap against what it meant to write
(`self-check`).

## Regenerating

1. Download the pinned release zip:
   `https://github.com/TakWolf/fusion-pixel-font/releases/download/2026.09.25/fusion-pixel-font-12px-monospaced-bdf-v2026.09.25.zip`
   (any other zip is rejected by the sha256 gate).
2. From the repo root:
   ```sh
   cargo run -p gen-bigfont -- /path/to/fusion-pixel-font-12px-monospaced-bdf-v2026.09.25.zip
   # writes assets/bigfont-12px-zh-hans.bin; also: --variant ja|zh-hant, -o <path>
   ```
3. Update the record below if anything changed and commit the new asset.
4. Eyeball-check glyphs:
   ```sh
   cargo run -p gen-bigfont -- --dump assets/bigfont-12px-zh-hans.bin "一口A!"
   ```

To move to a **new font release**: update `RELEASE`/`ZIP_NAME`/`ZIP_SHA256`
in `tools/gen-bigfont/src/main.rs`, regenerate, refresh this file, and
re-check the OFL notices under `assets/fusion-pixel/`.

## Current record (zh-Hans, gen-bigfont 0.1.0)

- Asset: `assets/bigfont-12px-zh-hans.bin`
- Asset sha256: `ee6e7bc6faeb67f5336852c650f45d5caf87c106553e9440a9a07edb0f64e910`
- Source zip sha256 (pinned): `d75f5262f108757edb0f47ee8e3d2dfdfbecfd94558faf5ffb0c06dc5866fb3b`
- Size: 230,258 bytes (budget 250,000)
- Glyphs: 7,188 — 104 halfwidth (6 px), 7,084 fullwidth (12 px)
- Charset asked: 7,365; missing from the zh-Hans BDF: 177 (≈143 rare GB2312
  hanzi such as 劐唣嗍, ≈29 zone-1/2 math and symbol chars such as ∈ ∏ ∑ √ ∞ €
  plus tone marks ˇ ˉ, kana ゗ ゘) → tofu at runtime
- Pixels clipped to the canvas: 21 (the vertical kana repeat marks 〱 〲
  U+3031/U+3032 are taller than 12 px)
- Halfwidth beyond ASCII: ¤ § ¨ ° ± × ÷ ′ ″ (GB2312 zone-1 symbols the font
  draws at 6 px)
