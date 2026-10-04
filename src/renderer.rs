//! Single-line frame drawing and pacing, and its big-font multi-row twin.
//!
//! The renderer owns everything that reaches the screen: it takes the
//! engine's frame, wraps it in the two sequences that put it on the terminal's
//! current line — move to column 0, clear the line — and writes that as one
//! buffer with one flush. No newlines are emitted per frame, the cursor never
//! moves to another line, and no alternate screen is involved, so the marquee
//! occupies exactly the line the shell left it on.
//!
//! [`BigRenderer`] is the same idea for [`crate::marquee::StripKind::Big`]
//! engines: the frame is `6·scale` rows of half-block cells drawn from the
//! top of the marquee's region downwards, one write and one flush per frame
//! and still no newline, so the terminal can never scroll mid-marquee.
//!
//! Both buffers (the frame, and the bytes it is wrapped into) are reused across
//! frames, so a steady-state frame performs no allocation: [`Engine::draw`]
//! clears and refills the frame buffer, and the escape sequences are queued
//! into a byte buffer that already has the capacity.
//!
//! Redirected output is the exception: with no terminal there is no cursor to
//! reposition, and escape bytes in a file are just noise, so a frame goes out
//! as one plain line — `6·scale` of them for big mode — and
//! [`Renderer::finish`] has nothing to erase.
//!
//! Pacing lives here too: [`Renderer::delay_until_next_frame`] is how long the
//! run loop should wait before the next frame is due — which is also the right
//! timeout for [`crate::terminal::Terminal::poll`], so a resize or Ctrl+C is
//! handled during the wait instead of after it.
//!
//! Colour comes from the theme layer ([`crate::theme`]): a theme is pure
//! data, and this module is the only place that turns it into bytes. A
//! solid theme wraps the frame in one SGR sequence before the frame's
//! clear (so a background colour paints the erased line too, on terminals
//! with BCE) and one reset after it, so the cleanup and the shell prompt
//! afterwards are never left coloured. A palette — two or more colours —
//! is banded: the entries cycle across the screen's display columns, one
//! run per band, and the scrolling text flows through the colour bands; a
//! wide cluster always takes the colour of the band it starts in, so no
//! character is ever split across two colours. A plain theme emits
//! nothing, byte-for-byte the output of the monochrome marquee, and
//! redirected output never sees a theme's bytes at all.

use std::io;
use std::time::{Duration, Instant};

use crossterm::cursor::{MoveDown, MoveToColumn, MoveUp};
use crossterm::queue;
use crossterm::style::{
    Attribute as CtAttribute, Color as CtColor, Print, ResetColor, SetAttribute,
    SetBackgroundColor, SetForegroundColor,
};
use crossterm::terminal::{Clear, ClearType};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::bigfont::raster::{Cell, Rasterizer, cell_rows};
use crate::marquee::{BigGlyphSlice, Engine, StripKind};
use crate::terminal::{self, Terminal};
use crate::theme::{Color, Theme};

/// One frame per `interval`, the first due at once, and a late draw restarts
/// the interval instead of making the next frames race to catch up.
#[derive(Debug)]
struct Pacing {
    /// How long one frame lasts, from `--speed`/`--fps`.
    interval: Duration,
    /// When the next frame is due.
    next_due: Instant,
}

impl Pacing {
    /// Pacing for a run of one frame per `interval`.
    fn new(interval: Duration) -> Self {
        Self {
            interval,
            next_due: Instant::now(),
        }
    }

    /// How long to wait before the next frame is due; zero means draw now.
    ///
    /// Doubles as the poll timeout for the run loop, so input is handled while
    /// the frame interval runs down.
    fn delay_until_next_frame(&self) -> Duration {
        self.next_due.saturating_duration_since(Instant::now())
    }

    /// Pushes the deadline one interval forward. A frame that is drawn late
    /// (a slow resize, a stalled write) restarts the interval instead of
    /// making the next frames race to catch up.
    fn schedule_next_frame(&mut self) {
        let now = Instant::now();
        let base = if self.next_due < now {
            now
        } else {
            self.next_due
        };
        self.next_due = base + self.interval;
    }
}

/// Draws engine frames onto the terminal's current line, one write and one
/// flush per frame, at the pace the command line asked for.
#[derive(Debug)]
pub struct Renderer {
    /// The engine's frame, reused across frames.
    frame: String,
    /// The frame wrapped in cursor and clear sequences: the single byte
    /// buffer written per frame.
    line: Vec<u8>,
    /// The theme as opening/closing SGR and (for a palette) the banding.
    paint: FramePaint,
    /// When frames are due, from `--speed`/`--fps`.
    pacing: Pacing,
}

impl Renderer {
    /// A renderer pacing one frame per `interval`, the first one due at once,
    /// painting the text with `theme`.
    pub fn new(interval: Duration, theme: Theme) -> Self {
        Self {
            frame: String::new(),
            line: Vec::new(),
            paint: FramePaint::from_theme(&theme),
            pacing: Pacing::new(interval),
        }
    }

    /// Draws the engine's current frame.
    ///
    /// The line is cleared first, so nothing of the previous frame survives —
    /// including its tail when the terminal has just become narrower than the
    /// engine knows (the run loop resizes the engine when the terminal reports
    /// it, and until then the frame is clamped to the width it is drawn into).
    pub fn draw(&mut self, out: &mut dyn Terminal, engine: &Engine) -> io::Result<()> {
        let columns = out.width();
        engine.draw(&mut self.frame);
        if engine.width() > columns {
            self.clamp(columns);
        }

        self.line.clear();
        if out.supports_escape() {
            queue!(self.line, MoveToColumn(0))?;
            // The theme bytes precede the clear so a background colour
            // paints the erased line too (terminals with BCE).
            self.line.extend_from_slice(&self.paint.opening);
            queue!(self.line, Clear(ClearType::CurrentLine))?;
            if let Paint::Banded { fg, bg, band } = &self.paint.paint {
                append_banded(&mut self.line, &self.frame, fg, bg, *band);
            } else {
                queue!(self.line, Print(&self.frame))?;
            }
            self.line.extend_from_slice(&self.paint.closing);
        } else {
            self.line.extend_from_slice(self.frame.as_bytes());
            self.line.push(b'\n');
        }

        out.write_all(&self.line)?;
        out.flush()?;
        self.pacing.schedule_next_frame();
        Ok(())
    }

    /// Erases the marquee's line and leaves the cursor at column 0, so the
    /// shell prompt lands on a clean line. Called on every way out of the run
    /// loop, including Ctrl+C.
    ///
    /// Redirected output needs no cleanup: every frame it got already ended
    /// with a newline.
    pub fn finish(&mut self, out: &mut dyn Terminal) -> io::Result<()> {
        if !out.supports_escape() {
            return Ok(());
        }
        self.line.clear();
        queue!(self.line, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
        out.write_all(&self.line)?;
        out.flush()
    }

    /// How long to wait before the next frame is due; zero means draw now.
    ///
    /// Doubles as the poll timeout for the run loop, so input is handled while
    /// the frame interval runs down.
    pub fn delay_until_next_frame(&self) -> Duration {
        self.pacing.delay_until_next_frame()
    }

    /// Cuts the frame down to `columns` display columns: a cluster that
    /// straddles the cut is dropped, and the result is padded back to exactly
    /// `columns`. Only reached when the terminal shrank before the engine did.
    fn clamp(&mut self, columns: usize) {
        let mut kept = 0;
        let cut = self
            .frame
            .grapheme_indices(true)
            .find_map(|(index, grapheme)| {
                let width = grapheme.width();
                if kept + width > columns {
                    Some(index)
                } else {
                    kept += width;
                    None
                }
            })
            .unwrap_or(self.frame.len());
        self.frame.truncate(cut);
        for _ in kept..columns {
            self.frame.push(' ');
        }
    }

    /// The reserved capacity of the two reused buffers.
    #[cfg(test)]
    fn buffer_capacity(&self) -> (usize, usize) {
        (self.frame.capacity(), self.line.capacity())
    }
}

/// Draws big-font engine frames as `6·scale` rows of half-block cells, one
/// write and one flush per frame.
///
/// The rows occupy the line the shell left the cursor on, plus the rows above
/// it: the frame starts by moving to the top of that region, then works
/// downwards — each row cleared then printed, cursor moves in between — so no
/// newline is ever emitted and the terminal can never scroll mid-marquee. The
/// frame parks the cursor at column 0 of the bottom (original) row, which is
/// where [`BigRenderer::finish`] starts erasing from. Like the single line, no
/// alternate screen is entered: on exit the rows the marquee occupied are
/// erased and the shell's line comes back blank.
///
/// The cell frame is the engine's [`Engine::visible_glyphs`] slices rasterized
/// by [`Rasterizer::render_row`] into a reused `rows × width` buffer: only the
/// glyphs intersecting the viewport are rasterized, and a slice's clipped
/// columns are simply not copied. Steady state allocates nothing.
///
/// Redirected output gets the same rows as plain text, one frame per
/// `6·scale` lines, with no escape bytes.
#[derive(Debug)]
pub struct BigRenderer {
    /// The engine's visible slices, reused across frames.
    slices: Vec<BigGlyphSlice>,
    /// The frame as half-block cells, `rows × width`, reused across frames.
    cells: Vec<Cell>,
    /// The row currently being turned into text, reused across frames.
    text: String,
    /// The frame wrapped in cursor and clear sequences: the single byte
    /// buffer written per frame.
    line: Vec<u8>,
    /// The theme as opening/closing SGR and (for a palette) the banding.
    paint: FramePaint,
    /// The glyph row rasterizer, with its reused row buffer.
    raster: Rasterizer,
    /// When frames are due, from `--speed`/`--fps`.
    pacing: Pacing,
    /// Rows the last drawn frame took; `0` until then, so [`Self::finish`]
    /// knows whether there is anything to erase.
    rows: usize,
}

impl BigRenderer {
    /// A big renderer pacing one frame per `interval`, the first one due at
    /// once, painting the half-blocks with `theme` (a theme's foreground
    /// colours the blocks, its background the blank cells; a palette cycles
    /// down the columns, so one glyph can show several colours at once).
    pub fn new(interval: Duration, theme: Theme) -> Self {
        Self {
            slices: Vec::new(),
            cells: Vec::new(),
            text: String::new(),
            line: Vec::new(),
            paint: FramePaint::from_theme(&theme),
            raster: Rasterizer::new(),
            pacing: Pacing::new(interval),
            rows: 0,
        }
    }

    /// Draws the engine's current frame as `6·scale` rows.
    ///
    /// The frame is clamped to the terminal's width, so a terminal that just
    /// shrank never receives a row wider than itself (the run loop resizes
    /// the engine on the same event, before the next frame; this is the
    /// belt-and-braces of the single-line renderer's clamp).
    pub fn draw(&mut self, out: &mut dyn Terminal, engine: &Engine) -> io::Result<()> {
        let StripKind::Big(strip) = engine.strip() else {
            panic!("BigRenderer draws big-font engines");
        };
        let scale = strip.scale();
        let rows = cell_rows(scale);
        let width = engine.width().min(out.width());
        if width == 0 {
            return Ok(());
        }
        // The panic hook erases this many rows if the run dies before finish().
        terminal::set_lines_taken(rows);
        self.rows = rows;

        engine.visible_glyphs(&mut self.slices);
        self.cells.resize(rows * width, Cell::Blank);
        self.cells.fill(Cell::Blank);
        for slice in &self.slices {
            let visible = slice.visible_cols();
            for row in 0..rows {
                let rastered = self.raster.render_row(&slice.glyph, scale, row);
                let start = slice.clip_left;
                let room = width.saturating_sub(slice.screen_col);
                let len = visible.min(room);
                let at = row * width + slice.screen_col;
                self.cells[at..at + len].copy_from_slice(&rastered[start..start + len]);
            }
        }

        self.line.clear();
        if out.supports_escape() {
            queue!(self.line, MoveToColumn(0))?;
            // The theme bytes precede the row walk so the very first clear
            // and every half-block is painted in the theme's colours; the
            // reset comes after the park, before the next frame.
            self.line.extend_from_slice(&self.paint.opening);
            queue!(
                self.line,
                MoveUp(rows_above(rows)),
                Clear(ClearType::CurrentLine)
            )?;
            for row in 0..rows {
                self.write_row(row, width);
                if let Paint::Banded { fg, bg, band } = &self.paint.paint {
                    append_banded(&mut self.line, &self.text, fg, bg, *band);
                } else {
                    queue!(self.line, Print(&self.text))?;
                }
                if row + 1 < rows {
                    queue!(
                        self.line,
                        MoveToColumn(0),
                        MoveDown(1),
                        Clear(ClearType::CurrentLine)
                    )?;
                }
            }
            // Park at the bottom row of the region: the next frame starts
            // from there, and finish() erases from there.
            queue!(self.line, MoveToColumn(0))?;
            self.line.extend_from_slice(&self.paint.closing);
        } else {
            for row in 0..rows {
                self.write_row(row, width);
                self.line.extend_from_slice(self.text.as_bytes());
                self.line.push(b'\n');
            }
        }

        out.write_all(&self.line)?;
        out.flush()?;
        self.pacing.schedule_next_frame();
        Ok(())
    }

    /// Erases the rows the marquee occupied and leaves the cursor at column 0
    /// of the line the shell left it on. Called on every way out of the run
    /// loop, including Ctrl+C.
    ///
    /// If no frame was ever drawn, nothing was taken over, so nothing is
    /// erased — the rows above the prompt belong to the user, not to us.
    /// Redirected output needs no cleanup: every frame it got ended with a
    /// newline.
    pub fn finish(&mut self, out: &mut dyn Terminal) -> io::Result<()> {
        let rows = self.rows;
        self.rows = 0;
        // Whatever is on screen now, it is the shell's again.
        terminal::set_lines_taken(1);
        if !out.supports_escape() || rows == 0 {
            return Ok(());
        }

        self.line.clear();
        queue!(self.line, MoveToColumn(0), MoveUp(rows_above(rows)))?;
        for row in 0..rows {
            queue!(self.line, Clear(ClearType::CurrentLine))?;
            if row + 1 < rows {
                queue!(self.line, MoveUp(1))?;
            }
        }
        // The erases walked up to the top row; come back down to the row the
        // marquee started on, at column 0.
        queue!(self.line, MoveDown(rows_above(rows)))?;
        out.write_all(&self.line)?;
        out.flush()
    }

    /// How long to wait before the next frame is due; zero means draw now.
    ///
    /// Doubles as the poll timeout for the run loop, so input is handled while
    /// the frame interval runs down.
    pub fn delay_until_next_frame(&self) -> Duration {
        self.pacing.delay_until_next_frame()
    }

    /// Turns cell row `row` of the `width`-wide frame into text.
    fn write_row(&mut self, row: usize, width: usize) {
        self.text.clear();
        let at = row * width;
        for cell in &self.cells[at..at + width] {
            self.text.push(cell.to_char());
        }
    }

    /// The reserved capacity of the reused buffers.
    #[cfg(test)]
    fn buffer_capacity(&self) -> (usize, usize, usize, usize) {
        (
            self.slices.capacity(),
            self.cells.capacity(),
            self.text.capacity(),
            self.line.capacity(),
        )
    }
}

/// The move-up distance to the top of a `rows`-row region: one less than the
/// row count, as a `u16` for crossterm's relative cursor moves.
fn rows_above(rows: usize) -> u16 {
    u16::try_from(rows - 1).unwrap_or(u16::MAX)
}

/// How a theme paints a frame: monochrome, one SGR wrap, or banded runs.
#[derive(Debug)]
enum Paint {
    /// No SGR at all: the monochrome marquee, byte for byte.
    Plain,
    /// One SGR sequence around the whole frame (opening/closing do it).
    Solid,
    /// The palette(s) cycle across the screen columns, one run per band.
    Banded {
        /// The foreground palette; empty when only `bg` is banded.
        fg: Vec<CtColor>,
        /// The banded background palette; a single-entry background stays
        /// in the opening instead and this is empty.
        bg: Vec<CtColor>,
        /// Display columns per palette entry.
        band: usize,
    },
}

/// A theme as the bytes and runs the renderer emits: the opening queued
/// before the frame's clear (so a background colour paints the erased line
/// too, on terminals with BCE), the closing reset after the frame — both
/// empty for a plain theme — and, for a palette, the banding the frame's
/// text is split into.
///
/// This is the whole boundary between the theme configuration layer and
/// the terminal: everything else consumes [`Theme`] as data.
#[derive(Debug)]
struct FramePaint {
    /// Which of the three shapes of painting this is.
    paint: Paint,
    /// The SGR bytes that open every frame.
    opening: Vec<u8>,
    /// The SGR bytes that close every frame.
    closing: Vec<u8>,
}

impl FramePaint {
    fn from_theme(theme: &Theme) -> Self {
        if theme.is_plain() {
            return Self {
                paint: Paint::Plain,
                opening: Vec::new(),
                closing: Vec::new(),
            };
        }
        let fg: Vec<CtColor> = theme.fg.iter().map(|&color| to_crossterm(color)).collect();
        let bg: Vec<CtColor> = theme.bg.iter().map(|&color| to_crossterm(color)).collect();
        let attributes = attribute_sgr(theme);

        // A solid theme: one colour (or none) per role, one wrap.
        if fg.len() <= 1 && bg.len() <= 1 {
            let mut opening = Vec::new();
            if let Some(&bg) = bg.first() {
                queue!(opening, SetBackgroundColor(bg)).expect("writing SGR to a Vec cannot fail");
            }
            if let Some(&fg) = fg.first() {
                queue!(opening, SetForegroundColor(fg)).expect("writing SGR to a Vec cannot fail");
            }
            opening.extend_from_slice(&attributes);
            if opening.is_empty() {
                return Self {
                    paint: Paint::Plain,
                    opening: Vec::new(),
                    closing: Vec::new(),
                };
            }
            let mut closing = Vec::new();
            queue!(closing, ResetColor).expect("writing SGR to a Vec cannot fail");
            return Self {
                paint: Paint::Solid,
                opening,
                closing,
            };
        }

        // A palette: the attributes (and a solid background, if any) open
        // the frame, then the palettes colour it band by band.
        let mut banded_bg = bg.clone();
        let mut opening = attributes;
        if let [solid] = banded_bg.as_slice() {
            queue!(opening, SetBackgroundColor(*solid)).expect("writing SGR to a Vec cannot fail");
            banded_bg.clear();
        }
        let mut closing = Vec::new();
        queue!(closing, ResetColor).expect("writing SGR to a Vec cannot fail");
        Self {
            paint: Paint::Banded {
                fg,
                bg: banded_bg,
                band: theme.band.max(1),
            },
            opening,
            closing,
        }
    }
}

/// The theme's attribute SGR bytes (bold, dim, italic, underline), empty
/// when none are set.
fn attribute_sgr(theme: &Theme) -> Vec<u8> {
    let mut attributes = Vec::new();
    for (enabled, attribute) in [
        (theme.bold, CtAttribute::Bold),
        (theme.dim, CtAttribute::Dim),
        (theme.italic, CtAttribute::Italic),
        (theme.underline, CtAttribute::Underlined),
    ] {
        if enabled {
            queue!(attributes, SetAttribute(attribute)).expect("writing SGR to a Vec cannot fail");
        }
    }
    attributes
}

/// Appends `text` to `out` as banded colour runs: each grapheme takes the
/// palette entry of its starting display column's band, so a wide cluster
/// is never split across colours, and consecutive graphemes of one band
/// share a single SGR sequence.
fn append_banded(out: &mut Vec<u8>, text: &str, fg: &[CtColor], bg: &[CtColor], band: usize) {
    let band = band.max(1);
    let mut column = 0usize;
    let mut run_start = 0usize;
    let mut run_band: Option<usize> = None;
    for (index, grapheme) in text.grapheme_indices(true) {
        let at = column / band;
        if run_band != Some(at) {
            if let Some(previous) = run_band {
                queue_run(out, fg, bg, previous);
                out.extend_from_slice(&text.as_bytes()[run_start..index]);
            }
            run_start = index;
            run_band = Some(at);
        }
        column += grapheme.width();
    }
    if let Some(previous) = run_band {
        queue_run(out, fg, bg, previous);
        out.extend_from_slice(&text.as_bytes()[run_start..]);
    }
}

/// The SGR for one band: each palette's entry, cycling by band index.
fn queue_run(out: &mut Vec<u8>, fg: &[CtColor], bg: &[CtColor], band: usize) {
    if !fg.is_empty() {
        queue!(out, SetForegroundColor(fg[band % fg.len()]))
            .expect("writing SGR to a Vec cannot fail");
    }
    if !bg.is_empty() {
        queue!(out, SetBackgroundColor(bg[band % bg.len()]))
            .expect("writing SGR to a Vec cannot fail");
    }
}

/// Maps a theme colour onto crossterm's. crossterm's plain colour names are
/// the *bright* palette entries (`Red` is bright red) and its `Dark…` names
/// are the classic eight, so the mapping is written out name by name to keep
/// the theme file's `red` meaning classic red.
fn to_crossterm(color: Color) -> CtColor {
    match color {
        Color::Black => CtColor::Black,
        Color::Red => CtColor::DarkRed,
        Color::Green => CtColor::DarkGreen,
        Color::Yellow => CtColor::DarkYellow,
        Color::Blue => CtColor::DarkBlue,
        Color::Magenta => CtColor::DarkMagenta,
        Color::Cyan => CtColor::DarkCyan,
        Color::White => CtColor::Grey,
        Color::BrightBlack => CtColor::DarkGrey,
        Color::BrightRed => CtColor::Red,
        Color::BrightGreen => CtColor::Green,
        Color::BrightYellow => CtColor::Yellow,
        Color::BrightBlue => CtColor::Blue,
        Color::BrightMagenta => CtColor::Magenta,
        Color::BrightCyan => CtColor::Cyan,
        Color::BrightWhite => CtColor::White,
        Color::Rgb { red, green, blue } => CtColor::Rgb {
            r: red,
            g: green,
            b: blue,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use crate::marquee::{Direction, Motion};
    use crate::terminal::{FakeTerminal, PlainTerminal};
    use crate::unicode::prepare;
    use clap::Parser;

    /// What every drawn frame starts with: column 0, then clear the line.
    const PREFIX: &str = "\x1b[1G\x1b[2K";

    fn engine(text: &str, width: usize) -> Engine {
        Engine::new(
            prepare(text),
            width,
            Motion {
                direction: Direction::Left,
                bounce: false,
                continuous: false,
                gap: 0,
                cycles: None,
            },
        )
    }

    /// Steps the engine to `offset` columns into its trip.
    fn at_offset(engine: &mut Engine, offset: usize) {
        for _ in 0..offset {
            engine.advance();
        }
    }

    /// The visible part of an emitted line: what the terminal would show.
    fn visible(emitted: &str) -> &str {
        emitted
            .strip_prefix(PREFIX)
            .unwrap_or_else(|| panic!("frame does not start with {PREFIX:?}: {emitted:?}"))
    }

    #[test]
    fn a_frame_is_a_cursor_move_a_clear_and_the_columns() {
        let mut terminal = FakeTerminal::new(6, 3);
        let mut renderer = Renderer::new(Duration::ZERO, Theme::default());
        let mut engine = engine("你好🚀", 6);
        at_offset(&mut engine, 6); // the whole text has entered the viewport

        renderer
            .draw(&mut terminal, &engine)
            .expect("a fake never fails");

        assert_eq!(terminal.frames(), ["\x1b[1G\x1b[2K你好🚀"]);
        assert_eq!(terminal.flush_count(), 1, "one flush per frame");
        assert_eq!(visible(&terminal.frames()[0]).width(), 6);
        assert!(
            !terminal.written().contains('\n'),
            "no newline churn: {:?}",
            terminal.written()
        );
    }

    #[test]
    fn ascii_cjk_and_emoji_frames_are_exactly_the_viewport_in_columns() {
        for (text, width) in [
            ("hello", 5),
            ("hello", 12),
            ("你好世界", 8),
            ("你好世界", 3),
            ("Hello 世界 🚀", 10),
            ("a\u{301} 👨‍👩‍👧‍👦 b", 7),
        ] {
            let source = prepare(text);
            let whole: Vec<&str> = source.cells().iter().map(|cell| cell.text()).collect();
            let mut engine = Engine::new(
                prepare(text),
                width,
                Motion {
                    direction: Direction::Left,
                    bounce: false,
                    continuous: false,
                    gap: 1,
                    cycles: None,
                },
            );
            let mut terminal = FakeTerminal::new(width, 3);
            let mut renderer = Renderer::new(Duration::ZERO, Theme::default());

            for _ in 0..engine.cycle() {
                renderer
                    .draw(&mut terminal, &engine)
                    .expect("a fake never fails");
                engine.advance();
            }

            let frames = terminal.frames();
            assert_eq!(frames.len(), engine.cycle(), "{text:?} at {width} columns");
            for emitted in frames {
                let shown = visible(emitted);
                assert_eq!(shown.width(), width, "{text:?} at {width}: {emitted:?}");
                assert!(
                    !emitted.contains('\n'),
                    "a frame never carries a newline: {emitted:?}"
                );
                for cluster in shown.graphemes(true) {
                    assert!(
                        cluster == " " || whole.contains(&cluster),
                        "split cluster {cluster:?} in {emitted:?}"
                    );
                }
            }
            assert_eq!(terminal.flush_count(), frames.len(), "one flush per frame");
        }
    }

    #[test]
    fn a_shrunk_terminal_never_receives_a_wider_line() {
        let mut terminal = FakeTerminal::new(10, 3);
        let mut renderer = Renderer::new(Duration::ZERO, Theme::default());
        let mut engine = engine("你好世界 🚀", 10);
        at_offset(&mut engine, 10); // fully entered

        renderer
            .draw(&mut terminal, &engine)
            .expect("a fake never fails");
        assert_eq!(visible(&terminal.frames()[0]).width(), 10);

        // The terminal narrows before the run loop can resize the engine: the
        // frame is clamped, and the clear erases the tail of the wider one.
        let clusters: Vec<String> = prepare("你好世界 🚀")
            .cells()
            .iter()
            .map(|cell| cell.text().to_string())
            .collect();
        terminal.set_size(6, 3);
        renderer
            .draw(&mut terminal, &engine)
            .expect("a fake never fails");
        let shrunk = visible(&terminal.frames()[1]);
        assert_eq!(shrunk.width(), 6, "{shrunk:?}");
        assert!(
            shrunk
                .graphemes(true)
                .all(|cluster| cluster == " " || clusters.iter().any(|whole| whole == cluster)),
            "clamping split a cluster: {shrunk:?}"
        );

        // Once the engine knows the new width, drawing carries on unchanged.
        engine.resize(6);
        renderer
            .draw(&mut terminal, &engine)
            .expect("a fake never fails");
        assert_eq!(visible(&terminal.frames()[2]).width(), 6);
        assert_eq!(terminal.flush_count(), 3, "still one flush per frame");
    }

    #[test]
    fn redirected_output_gets_one_plain_line_per_frame() {
        let mut terminal = PlainTerminal::new(Vec::new(), (8, 3));
        assert!(!terminal.supports_escape(), "a redirect is not a terminal");

        // What the same three frames should look like, straight from the
        // engine.
        let mut reference = engine("你好世界 🚀", 8);
        at_offset(&mut reference, 5);
        let mut expected = Vec::new();
        for _ in 0..3 {
            let mut frame = String::new();
            reference.draw(&mut frame);
            expected.push(frame);
            reference.advance();
        }

        let mut engine = engine("你好世界 🚀", 8);
        at_offset(&mut engine, 5);
        let mut renderer = Renderer::new(Duration::ZERO, Theme::default());
        for _ in 0..3 {
            renderer
                .draw(&mut terminal, &engine)
                .expect("a Vec never fails");
            engine.advance();
        }
        renderer.finish(&mut terminal).expect("a Vec never fails");

        let bytes = terminal.into_inner();
        let text = String::from_utf8(bytes).expect("plain frames are text");
        assert!(
            !text.contains('\u{1b}'),
            "an escape byte reached redirected output: {text:?}"
        );
        assert_eq!(text.lines().collect::<Vec<_>>(), expected, "{text:?}");
    }

    #[test]
    fn steady_state_frames_allocate_nothing() {
        let mut terminal = FakeTerminal::new(12, 3);
        let mut renderer = Renderer::new(Duration::ZERO, Theme::default());
        let mut engine = engine("你好世界 · Hello 🚀", 12);

        // One full cycle warms the buffers to the largest frame they will see;
        // after that the trip repeats, so nothing may grow again.
        for _ in 0..engine.cycle() {
            renderer
                .draw(&mut terminal, &engine)
                .expect("a fake never fails");
            engine.advance();
        }
        let warmed = renderer.buffer_capacity();

        for _ in 0..200 {
            renderer
                .draw(&mut terminal, &engine)
                .expect("a fake never fails");
            engine.advance();
        }
        assert_eq!(
            renderer.buffer_capacity(),
            warmed,
            "buffers were reallocated"
        );
    }

    #[test]
    fn finish_erases_the_line_and_leaves_the_cursor_at_column_zero() {
        let mut terminal = FakeTerminal::new(8, 3);
        let mut renderer = Renderer::new(Duration::ZERO, Theme::default());
        renderer.finish(&mut terminal).expect("a fake never fails");
        assert_eq!(terminal.frames(), [PREFIX]);
        assert_eq!(terminal.flush_count(), 1);
    }

    #[test]
    fn the_frame_interval_comes_from_speed_or_fps() {
        // The first frame is due immediately; after a draw, one interval.
        let mut renderer = Renderer::new(Duration::from_millis(200), Theme::default());
        assert!(
            renderer.delay_until_next_frame() < Duration::from_millis(5),
            "the first frame should not wait"
        );
        let mut terminal = FakeTerminal::new(4, 3);
        let engine = engine("abcd", 4);
        renderer
            .draw(&mut terminal, &engine)
            .expect("a fake never fails");
        let delay = renderer.delay_until_next_frame();
        assert!(
            delay > Duration::from_millis(150) && delay <= Duration::from_millis(200),
            "one interval left, got {delay:?}"
        );

        // Zero pacing never makes the loop wait.
        let renderer = Renderer::new(Duration::ZERO, Theme::default());
        assert_eq!(renderer.delay_until_next_frame(), Duration::ZERO);

        // ...and the interval is the one the command line asked for.
        for (flags, expected) in [
            (vec!["--speed", "120"], Duration::from_millis(120)),
            (vec!["--fps", "25"], Duration::from_millis(40)),
            (vec![], Duration::from_millis(50)),
        ] {
            let mut argv = vec!["marquee"];
            argv.extend(flags.iter().copied());
            argv.push("hi");
            let cli = Cli::try_parse_from(argv).expect("the command line should parse");
            assert_eq!(
                Renderer::new(cli.frame_interval(), Theme::default())
                    .pacing
                    .interval,
                expected,
                "{flags:?}"
            );
        }
    }

    #[test]
    fn waiting_the_reported_delay_paces_the_frames() {
        let interval = Duration::from_millis(30);
        let mut terminal = FakeTerminal::new(4, 3);
        let mut engine = engine("abcd", 4);
        let mut renderer = Renderer::new(interval, Theme::default());

        let started = Instant::now();
        for _ in 0..3 {
            std::thread::sleep(renderer.delay_until_next_frame());
            renderer
                .draw(&mut terminal, &engine)
                .expect("a fake never fails");
            engine.advance();
        }
        let elapsed = started.elapsed();
        assert_eq!(terminal.flush_count(), 3);
        assert!(
            elapsed >= interval * 2,
            "three frames should span at least two intervals, took {elapsed:?}"
        );
    }

    // ---- the theme layer (rendering side) ----

    /// A theme with a known SGR prefix, for golden-frame tests.
    fn theme() -> Theme {
        Theme {
            fg: vec![Color::Red],
            bg: vec![Color::Blue],
            bold: true,
            ..Theme::default()
        }
    }

    #[test]
    fn a_theme_wraps_the_frame_with_its_sgr_and_a_reset() {
        let mut terminal = FakeTerminal::new(6, 3);
        let mut renderer = Renderer::new(Duration::ZERO, theme());
        let mut engine = engine("你好🚀", 6);
        at_offset(&mut engine, 6);

        renderer
            .draw(&mut terminal, &engine)
            .expect("a fake never fails");

        // The theme bytes sit between the cursor move and the clear, so a
        // background colour paints the erased line too; the reset closes
        // the frame, so nothing after it is coloured. crossterm spells the
        // named colours as 256-colour indices: 48;5;4 is blue, 38;5;1 the
        // classic red (not the bright 9).
        let frame = &terminal.frames()[0];
        assert!(frame.starts_with("\x1b[1G"), "{frame:?}");
        let after_cursor = &frame["\x1b[1G".len()..];
        assert!(
            after_cursor.starts_with("\x1b[48;5;4m"),
            "bg first: {frame:?}"
        );
        assert!(
            after_cursor.contains("\x1b[38;5;1m"),
            "classic red, not bright: {frame:?}"
        );
        assert!(after_cursor.contains("\x1b[1m"), "bold: {frame:?}");
        assert!(
            frame.ends_with("\x1b[0m"),
            "reset after the columns: {frame:?}"
        );
        assert!(
            frame.contains("你好🚀"),
            "the theme never touches the columns: {frame:?}"
        );
        // One write, one flush, no newlines: the theme changes none of that.
        assert_eq!(terminal.flush_count(), 1);
        assert!(!terminal.written().contains('\n'));
    }

    #[test]
    fn a_plain_theme_emits_the_exact_bytes_of_a_monochrome_run() {
        let mut plain = FakeTerminal::new(6, 3);
        let mut themed = FakeTerminal::new(6, 3);
        let mut plain_renderer = Renderer::new(Duration::ZERO, Theme::default());
        let mut themed_renderer = Renderer::new(Duration::ZERO, Theme::default());
        let mut engine = engine("你好🚀", 6);
        at_offset(&mut engine, 6);

        plain_renderer
            .draw(&mut plain, &engine)
            .expect("a fake never fails");
        themed_renderer
            .draw(&mut themed, &engine)
            .expect("a fake never fails");

        assert_eq!(plain.written(), themed.written());
        assert_eq!(plain.frames(), themed.frames());
        assert_eq!(themed.frames(), ["\x1b[1G\x1b[2K你好🚀"]);
    }

    #[test]
    fn redirected_output_never_sees_the_theme_bytes() {
        let mut terminal = PlainTerminal::new(Vec::new(), (8, 3));
        let mut renderer = Renderer::new(Duration::ZERO, theme());
        let mut engine = engine("你好世界 🚀", 8);
        at_offset(&mut engine, 5);

        renderer
            .draw(&mut terminal, &engine)
            .expect("a Vec never fails");

        let text = String::from_utf8(terminal.into_inner()).expect("plain frames are text");
        assert!(
            !text.contains('\u{1b}'),
            "theme bytes reached redirected output: {text:?}"
        );
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn a_theme_wraps_the_whole_big_frame_once() {
        // The prefix precedes the row walk (and its clears) and one reset
        // closes the parked frame: the theme colours the half-blocks and
        // the blank cells, and the cleanup afterwards is uncoloured.
        let mut terminal = FakeTerminal::new(12, 24);
        let mut renderer = BigRenderer::new(Duration::ZERO, theme());
        let mut engine = big_engine("中", 1, 12);
        at_offset(&mut engine, 12);

        renderer
            .draw(&mut terminal, &engine)
            .expect("a fake never fails");

        let frame = &terminal.frames()[0];
        let head = "\x1b[1G".len();
        assert!(
            frame[head..].starts_with("\x1b[48;5;4m\x1b[38;5;1m\x1b[1m"),
            "the SGR prefix opens the frame: {frame:?}"
        );
        assert!(
            frame.ends_with("\x1b[1G\x1b[0m"),
            "park, then reset: {frame:?}"
        );
        assert_eq!(
            frame.matches("\x1b[48;5;4m").count(),
            1,
            "the prefix appears once"
        );
        assert_eq!(
            frame.matches("\x1b[0m").count(),
            1,
            "the reset appears once"
        );

        // The cleanup after a themed run erases in default colours.
        renderer.finish(&mut terminal).expect("a fake never fails");
        let cleanup = &terminal.frames()[1];
        assert!(!cleanup.contains("\x1b[3"), "{cleanup:?}");
        assert!(!cleanup.contains("\x1b[4"), "{cleanup:?}");
    }

    #[test]
    fn frame_paint_covers_the_attribute_set() {
        let paint = FramePaint::from_theme(&Theme {
            fg: vec![Color::BrightWhite],
            bg: vec![Color::Black],
            bold: true,
            dim: true,
            italic: true,
            underline: true,
            ..Theme::default()
        });
        assert!(matches!(paint.paint, Paint::Solid));
        assert_eq!(
            String::from_utf8_lossy(&paint.opening),
            "\x1b[48;5;0m\x1b[38;5;15m\x1b[1m\x1b[2m\x1b[3m\x1b[4m"
        );
        assert_eq!(String::from_utf8_lossy(&paint.closing), "\x1b[0m");

        let plain = FramePaint::from_theme(&Theme::default());
        assert!(matches!(plain.paint, Paint::Plain));
        assert!(plain.opening.is_empty());
        assert!(plain.closing.is_empty());
    }

    #[test]
    fn banded_runs_colour_each_band_and_never_split_a_cluster() {
        // 你好 are two 2-column clusters; with a 2-column band the first
        // takes band 0 (red), the second band 1 (blue) — each cluster one
        // colour, never split.
        let mut out = Vec::new();
        append_banded(
            &mut out,
            "你好",
            &[to_crossterm(Color::Red), to_crossterm(Color::Blue)],
            &[],
            2,
        );
        assert_eq!(
            String::from_utf8_lossy(&out),
            "\x1b[38;5;1m你\x1b[38;5;4m好"
        );

        // A cluster straddling a band boundary keeps its starting band: A
        // starts at column 0, 你 at column 1 — still band 0, so both share
        // one red run even though 你 crosses into band 1; a (column 3) is
        // the first band-1 blue.
        let mut out = Vec::new();
        append_banded(
            &mut out,
            "A你a",
            &[to_crossterm(Color::Red), to_crossterm(Color::Blue)],
            &[],
            2,
        );
        assert_eq!(
            String::from_utf8_lossy(&out),
            "\x1b[38;5;1mA你\x1b[38;5;4ma"
        );

        // The palette cycles: four single-column bands with two colours.
        let mut out = Vec::new();
        append_banded(
            &mut out,
            "abcd",
            &[to_crossterm(Color::Red), to_crossterm(Color::Blue)],
            &[],
            1,
        );
        assert_eq!(
            String::from_utf8_lossy(&out),
            "\x1b[38;5;1ma\x1b[38;5;4mb\x1b[38;5;1mc\x1b[38;5;4md"
        );

        // A background palette paints the padding spaces too; an empty
        // foreground palette emits no fg sequences.
        let mut out = Vec::new();
        append_banded(
            &mut out,
            "  ",
            &[],
            &[to_crossterm(Color::Green), to_crossterm(Color::Magenta)],
            1,
        );
        assert_eq!(String::from_utf8_lossy(&out), "\x1b[48;5;2m \x1b[48;5;5m ");

        // Nothing to paint, nothing emitted.
        let mut out = Vec::new();
        append_banded(&mut out, "", &[to_crossterm(Color::Red)], &[], 1);
        assert!(out.is_empty());
    }

    #[test]
    fn a_palette_wraps_the_frame_in_opening_and_reset() {
        // The end-to-end shape, golden: the opening (attributes, then a
        // solid background) sits before the clear, the bands paint the
        // columns, and one reset closes the frame. "你好" at offset 4 of a
        // left scroll starts at column 4, so the frame is "    你好" and a
        // 2-column band colours it red/blue/red/blue — the blanks included.
        let mut terminal = FakeTerminal::new(8, 3);
        let theme = Theme {
            fg: vec![Color::Red, Color::Blue],
            bg: vec![Color::Black],
            bold: true,
            band: 2,
            ..Theme::default()
        };
        let mut renderer = Renderer::new(Duration::ZERO, theme);
        let mut engine = engine("你好ab", 8);
        at_offset(&mut engine, 4);

        renderer
            .draw(&mut terminal, &engine)
            .expect("a fake never fails");

        assert_eq!(
            terminal.frames()[0],
            "\x1b[1G\x1b[1m\x1b[48;5;0m\x1b[2K\
             \x1b[38;5;1m  \x1b[38;5;4m  \x1b[38;5;1m你\x1b[38;5;4m好\x1b[0m"
        );
        assert_eq!(terminal.flush_count(), 1);
        assert!(!terminal.written().contains('\n'));
    }

    #[test]
    fn a_palette_bands_a_big_frame_column_by_column() {
        // Big frames band the same way: every row of cells is split into
        // per-band runs, so a glyph wider than a band shows several
        // colours — the rainbow pixel-art look.
        let mut terminal = FakeTerminal::new(12, 24);
        let theme = Theme {
            fg: vec![Color::Red, Color::Blue],
            band: 2,
            ..Theme::default()
        };
        let mut renderer = BigRenderer::new(Duration::ZERO, theme);
        let mut engine = big_engine("中", 1, 12);
        at_offset(&mut engine, 12);

        renderer
            .draw(&mut terminal, &engine)
            .expect("a fake never fails");

        let frame = &terminal.frames()[0];
        let glyph = big_face().resolve_char('中');
        // The expected frame: every row split at the band boundaries
        // (columns 0, 2, 4, …) and coloured red/blue alternately.
        let mut expected = String::from("\x1b[1G\x1b[5A\x1b[2K");
        for row in 0..6 {
            let rastered: Vec<char> = rastered_row(&glyph, 1, row).chars().collect();
            for (band, chunk) in rastered.chunks(2).enumerate() {
                let color = if band % 2 == 0 { "1" } else { "4" };
                expected.push_str(&format!("\x1b[38;5;{color}m"));
                expected.extend(chunk);
            }
            if row + 1 < 6 {
                expected.push_str(BETWEEN_ROWS);
            }
        }
        expected.push_str(PARK);
        expected.push_str("\x1b[0m");
        assert_eq!(frame, &expected);
        assert_eq!(
            frame.matches("\x1b[38;5;1m").count() + frame.matches("\x1b[38;5;4m").count(),
            6 * 6,
            "one SGR per band per row: {frame:?}"
        );
    }

    // ---- the big-font multi-row renderer (T-12) ----

    use crate::bigfont::BigFontFace;
    use crate::marquee::BigStrip;

    /// Between two rows: back to column 0, one row down, cleared.
    const BETWEEN_ROWS: &str = "\x1b[1G\x1b[1B\x1b[2K";
    /// Where every frame parks: column 0 of the bottom row.
    const PARK: &str = "\x1b[1G";

    fn big_face() -> BigFontFace<'static> {
        BigFontFace::embedded_zh_hans().expect("the committed asset parses")
    }

    fn big_engine(text: &str, scale: usize, width: usize) -> Engine {
        let strip = BigStrip::build(&prepare(text), &big_face(), scale);
        Engine::new_big(
            strip,
            width,
            Motion {
                direction: Direction::Left,
                bounce: false,
                continuous: false,
                gap: 0,
                cycles: None,
            },
        )
    }

    /// The whole cell row of `glyph` at `scale`, as text, straight from the
    /// rasterizer: what the renderer is expected to copy.
    fn rastered_row(glyph: &crate::bigfont::ResolvedGlyph, scale: usize, row: usize) -> String {
        let mut raster = Rasterizer::new();
        raster
            .render_row(glyph, scale, row)
            .iter()
            .copied()
            .map(Cell::to_char)
            .collect()
    }

    /// A big frame with `rows` rows of `row_texts`, laid out exactly as the
    /// renderer emits it.
    fn big_frame(row_texts: &[String]) -> String {
        let mut expected = format!("\x1b[1G\x1b[{}A\x1b[2K", row_texts.len() - 1);
        for (row, text) in row_texts.iter().enumerate() {
            expected.push_str(text);
            if row + 1 < row_texts.len() {
                expected.push_str(BETWEEN_ROWS);
            }
        }
        expected.push_str(PARK);
        expected
    }

    #[test]
    fn a_big_frame_is_a_row_walk_with_clears_and_one_flush() {
        // 中 at scale 1: 12 columns, 6 rows, fully inside the viewport.
        let mut terminal = FakeTerminal::new(12, 24);
        let mut renderer = BigRenderer::new(Duration::ZERO, Theme::default());
        let mut engine = big_engine("中", 1, 12);
        at_offset(&mut engine, 12); // start = 0, uncut

        renderer
            .draw(&mut terminal, &engine)
            .expect("a fake never fails");

        let glyph = big_face().resolve_char('中');
        let rows: Vec<String> = (0..6).map(|row| rastered_row(&glyph, 1, row)).collect();
        assert_eq!(terminal.frames(), [big_frame(&rows)]);
        assert_eq!(terminal.flush_count(), 1, "one flush per frame");
        assert!(
            !terminal.written().contains('\n'),
            "no newline churn: {:?}",
            terminal.written()
        );
    }

    #[test]
    fn a_viewport_cutting_a_big_glyph_clips_its_cell_columns() {
        // 中 spans [-1, 11) against a 5-wide viewport: clip_left 1, visible 5.
        let mut terminal = FakeTerminal::new(5, 24);
        let mut renderer = BigRenderer::new(Duration::ZERO, Theme::default());
        let mut engine = big_engine("中", 1, 5);
        at_offset(&mut engine, 6);

        renderer
            .draw(&mut terminal, &engine)
            .expect("a fake never fails");

        let glyph = big_face().resolve_char('中');
        let rows: Vec<String> = (0..6)
            .map(|row| rastered_row(&glyph, 1, row))
            .map(|row| row.chars().skip(1).take(5).collect())
            .collect();
        assert_eq!(terminal.frames(), [big_frame(&rows)]);
    }

    #[test]
    fn two_clipped_glyphs_tile_their_rows_without_a_gap() {
        // A [-3, 3) left-clipped, 中 [3, 15) right-clipped, width 4.
        let mut terminal = FakeTerminal::new(4, 24);
        let mut renderer = BigRenderer::new(Duration::ZERO, Theme::default());
        let mut engine = big_engine("A中", 1, 4);
        at_offset(&mut engine, 7);

        renderer
            .draw(&mut terminal, &engine)
            .expect("a fake never fails");

        let face = big_face();
        let a = face.resolve_char('A');
        let zhong = face.resolve_char('中');
        let rows: Vec<String> = (0..6)
            .map(|row| {
                let a_row: String = rastered_row(&a, 1, row).chars().skip(3).collect();
                let zhong_row: String = rastered_row(&zhong, 1, row).chars().take(1).collect();
                format!("{a_row}{zhong_row}")
            })
            .collect();
        assert_eq!(terminal.frames(), [big_frame(&rows)]);
    }

    #[test]
    fn big_finish_erases_the_rows_and_returns_to_the_starting_line() {
        let mut terminal = FakeTerminal::new(12, 24);
        let mut renderer = BigRenderer::new(Duration::ZERO, Theme::default());
        let engine = big_engine("中", 1, 12);
        renderer
            .draw(&mut terminal, &engine)
            .expect("a fake never fails");

        renderer.finish(&mut terminal).expect("a fake never fails");

        // Walk up to the top of the 6-row region, clearing each row on the
        // way, then back down to the row the marquee started on.
        let mut expected = String::from("\x1b[1G\x1b[5A");
        for row in 0..6 {
            expected.push_str("\x1b[2K");
            if row + 1 < 6 {
                expected.push_str("\x1b[1A");
            }
        }
        expected.push_str("\x1b[5B");
        assert_eq!(terminal.frames()[1], expected);
        assert_eq!(terminal.flush_count(), 2, "one flush for the cleanup");
    }

    #[test]
    fn big_finish_without_a_drawn_frame_erases_nothing() {
        // Nothing was ever taken over: the rows above the cursor are the
        // user's, and finish() must not clear them.
        let mut terminal = FakeTerminal::new(12, 24);
        let mut renderer = BigRenderer::new(Duration::ZERO, Theme::default());
        renderer.finish(&mut terminal).expect("a fake never fails");
        assert!(terminal.frames().is_empty());
        assert_eq!(terminal.flush_count(), 0);
    }

    #[test]
    fn big_frames_clamp_to_the_narrower_terminal() {
        // The engine is one frame ahead of a shrunken terminal: the rows are
        // cut to the terminal's width and blank-padded.
        let mut terminal = FakeTerminal::new(10, 24);
        let mut renderer = BigRenderer::new(Duration::ZERO, Theme::default());
        let mut engine = big_engine("中", 1, 20);
        at_offset(&mut engine, 24); // start = -4: clipped left 4, visible 8

        renderer
            .draw(&mut terminal, &engine)
            .expect("a fake never fails");

        let glyph = big_face().resolve_char('中');
        let rows: Vec<String> = (0..6)
            .map(|row| {
                let cut: String = rastered_row(&glyph, 1, row)
                    .chars()
                    .skip(4)
                    .take(8)
                    .collect();
                format!("{cut:<10}")
            })
            .collect();
        assert_eq!(terminal.frames(), [big_frame(&rows)]);
    }

    #[test]
    fn redirected_big_output_is_plain_rows_per_frame() {
        let mut terminal = PlainTerminal::new(Vec::new(), (12, 24));
        let mut renderer = BigRenderer::new(Duration::ZERO, Theme::default());
        let mut engine = big_engine("中", 1, 12);
        at_offset(&mut engine, 12);

        renderer
            .draw(&mut terminal, &engine)
            .expect("a Vec never fails");
        renderer.finish(&mut terminal).expect("a Vec never fails");

        let bytes = terminal.into_inner();
        let text = String::from_utf8(bytes).expect("plain frames are text");
        assert!(
            !text.contains('\u{1b}'),
            "an escape byte reached redirected output: {text:?}"
        );
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 6, "one line per cell row: {text:?}");
        assert!(
            lines.iter().all(|line| line.chars().count() == 12),
            "{text:?}"
        );
        // The rows are the rasterized glyph, uncut.
        let glyph = big_face().resolve_char('中');
        for (row, line) in lines.iter().enumerate() {
            assert_eq!(*line, rastered_row(&glyph, 1, row), "row {row}");
        }
    }

    #[test]
    fn steady_state_big_frames_allocate_nothing() {
        let mut terminal = FakeTerminal::new(12, 24);
        let mut renderer = BigRenderer::new(Duration::ZERO, Theme::default());
        let mut engine = big_engine("中A🚀", 2, 12);

        for _ in 0..engine.cycle() {
            renderer
                .draw(&mut terminal, &engine)
                .expect("a fake never fails");
            engine.advance();
        }
        let warmed = renderer.buffer_capacity();

        for _ in 0..100 {
            renderer
                .draw(&mut terminal, &engine)
                .expect("a fake never fails");
            engine.advance();
        }
        assert_eq!(
            renderer.buffer_capacity(),
            warmed,
            "buffers were reallocated"
        );
    }
}
