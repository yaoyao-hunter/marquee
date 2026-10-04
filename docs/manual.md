# marquee big-font mode — manual

Chinese version: [manual.zh-CN.md](manual.zh-CN.md)

`--big` scrolls the text as large pixel glyphs spanning several terminal rows
instead of one line of terminal characters: every grapheme cluster maps to a
glyph of the embedded [Fusion Pixel Font](https://github.com/TakWolf/fusion-pixel-font)
and is drawn with half-block characters, so a CJK character becomes a block of
cells twelve columns wide and six rows tall at the default magnification.

```sh
marquee "你好世界" --big
marquee "你好世界" --big --scale 2
echo "under construction" | marquee --big --direction right
```

This manual covers big-font usage completely: the flags, the geometry, the
scrolling behaviour, the size limits, input, redirected output and exit
behaviour. The single-line mode and the general rules (input precedence, exit
codes, environment, themes) are documented in [docs/marquee.1](marquee.1) and
the [README](../README.md); nothing there changes except what is stated below.

## Turning it on

| Flag | Value | Default | Rules |
|------|-------|---------|-------|
| `--big` | — | off | Enables big-font mode. |
| `--scale N` | 1–32 | 1 | Pixel magnification. Requires `--big`. The terminal must fit it (see below). |
| `--font name` | `zh-hans` | `zh-hans` | Which packaged pixel font to render with. Requires `--big`. |

- `--scale` and `--font` without `--big` are usage errors (exit 2): they only
  make sense in big mode.
- A `--scale` outside 1–32, or a `--font` other than `zh-hans`, is a usage
  error (exit 2). Other font variants (`ja`, `zh-hant`) are planned but not
  packaged yet; the atlas generator already supports building them.
- `--font zh-hans` never needs to be spelled out — it is the default.

## Geometry and rendering

- Each font pixel becomes an `N×N` block of terminal cells at
  `--scale N` (`N = 1` by default), drawn with the half-block characters
  `█` `▀` `▄` and spaces, in the theme's foreground colour (the terminal's
  default when the theme is `default`). A theme's background colours the
  blank cells; a plain theme emits no colour escape sequences at all, so
  `--no-color` simply forces it. A palette theme (a list of colours) is
  banded down the screen columns, `band` cells per colour, so one glyph
  wider than a band shows several colours at once.
- A full-width glyph (CJK, full-width punctuation) is 12×12 font pixels, so
  `12·N` columns × `6·N` rows of cells. A half-width glyph (ASCII letters,
  digits, punctuation) is 6×12 pixels, so `6·N` columns × `6·N` rows.
- The glyphs occupy `6·N` rows: the line the cursor is on plus the rows above
  it. Nothing below the cursor line is touched.
- Coverage of the packaged `zh-hans` atlas: printable ASCII, the GB2312
  level 1/2 set (~6763 characters), kana, and full-width punctuation.
  Anything else — including emoji outside the atlas and rare scripts —
  renders as a hollow box (tofu) of the same size, and the scroll never
  breaks.
- Grapheme clusters are mapped as a whole, the way the single-line mode counts
  them: a family joined by zero-width joiners is one unit, rendered from the
  first codepoint of the cluster that has a glyph, or tofu if none does.

## Scrolling behaviour

The motion is the single-line motion with a different painter; the engine is
shared, so every main-mode option works in big mode and means the same thing,
with columns now counted in terminal cells:

- One column step per frame, paced by `--speed MS` (1–10000, default 50) or
  `--fps FPS` (1–1000); the two are mutually exclusive.
- `--direction left` (default) enters at the right edge and exits left;
  `--direction right` mirrors it.
- `--bounce` reverses at both edges instead of wrapping around; a bounce
  cycle is the trip to one edge, the `--gap`, the trip back, and the `--gap`
  again.
- `--continuous` tiles the glyphs into a stream: the next copy of the strip
  follows the previous one `--gap` cell columns behind it, so the screen
  never empties once the first copy has entered (while the gap is narrower
  than the viewport). A cycle is then one period of the stream — the strip
  plus its gap — and `--once`/`--repeat` count periods. It conflicts with
  `--bounce`.
- `--gap COLUMNS` (default 8) counts blank cell columns between cycles, or
  between consecutive copies in continuous mode.
- `--once` and `--repeat N` bound the run; the same exit status 0 applies.
- `--align` has no effect in big mode (it is accepted but not implemented
  anywhere yet).
- `--theme` and `--theme-file` paint the half-blocks as in the single-line
  mode: the theme's foreground colours the blocks, its background the blank
  cells. A solid theme carries its colour sequences once, around the whole
  `6·N`-row frame; a palette colours the rows band by band.

A frame is exactly the terminal's width in columns, always. When the viewport
edge cuts through a glyph, the glyph is clipped at that edge — a half `你`
enters and leaves the screen smoothly — so the motion never jumps a column
and nothing wraps to the next line. In continuous mode the same clipping
applies to every copy of the stream, and the copies tile the viewport
without gaps or overlaps.

## Size limits and resize

Big mode names the smallest terminal it will run on, per scale: the viewport
must show one whole full-width glyph, and fit the glyph rows with one line of
margin:

```
columns ≥ 12·N        rows ≥ 6·N + 1
```

A terminal smaller than that is refused before anything is drawn — exit 2,
the message on **stderr** (stdout stays clean for piping):

```
marquee: the terminal is too small for --big --scale 2: needs at least 24 columns × 13 rows, this terminal is 20×12
```

A window that shrinks below the limits mid-run does not stop the scroll: the
scale is re-fitted to the height — down when the window shrinks, back toward
the request when it grows again — the glyphs are clipped to the narrower
viewport, and one notice goes to **stderr** naming the size that restores the
run:

```
marquee: the terminal became too small for --big --scale 2: needs at least 24 columns × 13 rows, scrolling at --scale 1 until it fits again
```

A resize redraws the very next frame at the new width with the text kept at
the same point of its trip.

## Input

Identical to the single-line mode: a positional `TEXT` wins and stdin is
never read; with no argument, piped stdin is read to EOF with each line
trimmed, blank lines dropped and the rest joined with single spaces; with no
argument and a terminal on stdin, marquee exits 2 rather than block. Blank
input is refused (exit 2).

## Redirected output

When stdout is not a terminal, each frame is written as `6·N` plain lines —
one per cell row, no cursor positioning, no escape bytes — paced as usual.
`COLUMNS` and `LINES` supply the assumed size in that situation (defaults 80
and 24), and the size limits are checked against them: a pipe too narrow or
too short for `--scale N` is refused the same way a small window is. If the
reader of the pipe goes away (`marquee --big … | head -3`), marquee exits 0
in silence instead of reporting a broken pipe.

## Terminal and exit behaviour

- No alternate screen is entered; the scrollback is left intact.
- The cursor is hidden; a frame is one buffered write plus flush, so there is
  no flicker between cell rows.
- On every way out — the cycles ran out, Ctrl+C (read in raw mode, exit 0),
  an I/O error (exit 1), even a panic — the `6·N` rows the marquee occupied
  are erased, the cursor is shown again and raw mode is left behind, so the
  shell prompt lands on a clean line.
- Exit codes are the manual page's: 0 done / Ctrl+C / reader gone, 1 real
  I/O error, 2 usage.

## Examples

Default magnification, one glyph = 12 columns × 6 rows:

```sh
marquee "你好世界" --big
```

Double size — each glyph 24 columns × 12 rows; the terminal must be at
least 24 columns by 13 rows:

```sh
marquee "你好世界" --big --scale 2
```

Fast, from a pipe, mirrored direction, following itself:

```sh
echo "正在部署..." | marquee --big --fps 60 --direction right --continuous
```

Themed — the half-blocks in bold bright green, blanks untouched:

```sh
marquee --big --scale 2 "ON AIR" --theme matrix
```

Bouncing billboard with a tight gap, exactly three cycles:

```sh
marquee --big --bounce --gap 2 --repeat 3 " ON AIR "
```

Asking for more scale than the terminal fits (80×24 terminal, scale 9 needs
108×55):

```sh
$ marquee --big --scale 9 "hello"
marquee: the terminal is too small for --big --scale 9: needs at least 108 columns × 55 rows, this terminal is 80×24
```

Frames as plain text, one file per run of `6·N` lines per frame:

```sh
marquee --big --once "done" > frames.txt
```

## Known gaps

- `--align` is accepted and validated but has no effect (reserved for a
  static placement mode).
- Only the `zh-hans` atlas is packaged; `ja` and `zh-hant` need their
  generated assets committed first.
- No per-mode themes — one theme paints the single line and big mode the
  same way, and palette bands are screen-fixed: colour that travels with
  the text (each cluster keeping its own colour) is not in the theme
  grammar yet.
- No proportional variants, no colour beyond the theme grammar, no vertical
  scrolling — the V1 scope in `docs/big-font-mode.md`.
