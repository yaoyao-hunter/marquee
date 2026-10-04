//! The scroll engine: pure, terminal-free motion and framing.
//!
//! The engine owns the precomputed cells from [`crate::unicode`] plus the
//! current viewport width, and turns them into the exact columns to draw for
//! each frame. It never touches a terminal: [`crate::renderer`] draws what
//! [`Engine::draw`] produces, and the run loop steps time with
//! [`Engine::advance`] and feeds resizes in through [`Engine::resize`].
//!
//! # Motion
//!
//! The text travels from just off one edge to just off the other, which is
//! `width + text_width` columns, and one *cycle* is that trip plus `gap`
//! columns of blank screen:
//!
//! ```text
//! travel = width + text_width
//! cycle  = travel + gap          scrolling, left or right
//! cycle  = 2 * (travel + gap)    bouncing: there and back, dwelling off
//!                                each edge for the gap
//! ```
//!
//! `--once` is one cycle, `--repeat n` is n cycles, and no flag loops forever.
//! One [`Engine::advance`] moves the text by exactly one column, so no frame
//! skips or repeats a visible column — including at a bounce turnaround, where
//! only the off-screen dwell repeats.
//!
//! # Resize
//!
//! [`Engine::resize`] keeps the phase by column ratio, `offset' = offset *
//! cycle' / cycle`, so the text stays at the same point of its trip. The
//! offset is always less than the current cycle, which is what keeps a resize
//! from wrapping the text around or stranding it off-screen.
//!
//! # Framing
//!
//! A frame is exactly `width` display columns. A grapheme cluster is never
//! split: one that straddles an edge is dropped and the columns it would have
//! covered stay blank, because that is all a terminal can draw.
//!
//! # Big-font strips (docs/big-font-mode.md §5)
//!
//! A [`StripKind::Big`] strip swaps what travels, not how: the strip width is
//! the summed glyph advances in cells (`Σ cell_cols × scale`, one resolved
//! glyph per grapheme cluster), and every motion rule above — travel, cycle,
//! gap, bounce, repeat, the resize phase rule — applies unchanged, because
//! the engine only ever asks the strip for its width.
//!
//! The frame itself is a *viewport slice*: [`Engine::visible_glyphs`] returns
//! the glyphs intersecting `[x, x + width)` with `clip_left`/`clip_right`
//! counts, so a viewport edge cutting through a glyph clips it by whole cell
//! columns instead of dropping it — one column of travel shifts every clip by
//! at most one column, which is what "no column jumps between frames" means.
//! The T-12 renderer turns the slices into half-block cell rows.
//!
//! The engine keeps its own [`Direction`] so this core stays free of clap; the
//! run loop maps `--direction` onto it.

use crate::bigfont::raster::cell_cols;
use crate::bigfont::{BigFontFace, ResolvedGlyph};
use crate::unicode::PreparedText;

/// Which way the text travels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Enters at the right edge, leaves at the left.
    Left,
    /// Enters at the left edge, leaves at the right.
    Right,
}

/// How the text moves: everything the engine needs besides the text itself
/// and the viewport width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Motion {
    /// Which edge the text enters from.
    pub direction: Direction,
    /// Reverse at both edges instead of starting over.
    pub bounce: bool,
    /// Blank columns between one cycle and the next.
    pub gap: usize,
    /// How many cycles to run; `None` loops forever.
    pub cycles: Option<u32>,
}

/// What travels through the viewport. Both kinds only ever expose a width
/// in columns to the motion core — that is the whole of §5's "big mode
/// changes the shader, not the kinematics".
#[derive(Debug)]
pub enum StripKind {
    /// Grapheme clusters measured in display columns; drawn by
    /// [`Engine::draw`] as whole clusters.
    Text(PreparedText),
    /// One resolved glyph per cluster, measured in half-block cell columns;
    /// drawn by the T-12 renderer from [`Engine::visible_glyphs`] slices.
    Big(BigStrip),
}

impl StripKind {
    /// The travelling width in columns: display columns for text, cell
    /// columns for a big strip.
    fn text_width(&self) -> usize {
        match self {
            Self::Text(prepared) => prepared.width(),
            Self::Big(strip) => strip.strip_width(),
        }
    }
}

/// The big-font strip (§5): each cluster of the input resolved to one
/// glyph (§3.4, tofu when missing) and magnified by `scale`. Glyph advances
/// are in terminal cell columns: `cell_cols(glyph.width_px(), scale)`.
#[derive(Debug)]
pub struct BigStrip {
    glyphs: Vec<ResolvedGlyph<'static>>,
    scale: usize,
}

// Wired into the engine here; the T-12 renderer is still the only future
// consumer of slices at scale, so parts stay dead until then (N-3 style).
#[allow(dead_code)]
impl BigStrip {
    /// Resolves every cluster of `prepared` through `face` (§3.4) at
    /// `scale` ≥ 1.
    pub fn build(prepared: &PreparedText, face: &BigFontFace<'static>, scale: usize) -> Self {
        debug_assert!(scale >= 1);
        let glyphs = prepared
            .cells()
            .iter()
            .map(|cell| face.resolve_cluster(cell.text()))
            .collect();
        Self { glyphs, scale }
    }

    /// The glyphs in strip order, one per cluster.
    pub fn glyphs(&self) -> &[ResolvedGlyph<'static>] {
        &self.glyphs
    }

    /// The magnification factor; each glyph pixel is `scale` terminal cells.
    pub fn scale(&self) -> usize {
        self.scale
    }

    /// Σ glyph advances in cell columns — what the engine scrolls.
    pub fn strip_width(&self) -> usize {
        self.glyphs
            .iter()
            .map(|g| cell_cols(g.width_px(), self.scale))
            .sum()
    }

    fn set_scale(&mut self, scale: usize) {
        self.scale = scale;
    }
}

/// Result of fitting a requested `--scale` to the terminal height (§4):
/// `6·scale ≤ term_rows − 1` (one row of margin). The run loop turns a
/// `clamped` result into the stderr notice; the engine itself stays
/// terminal-free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScaleClamp {
    /// What the user asked for.
    pub requested: usize,
    /// What will actually run (`≥ 1`, even on a terminal too short for it).
    pub applied: usize,
    /// Whether the request exceeded the terminal height and was reduced.
    pub clamped: bool,
}

/// Fits `requested` to `term_rows` per §4: `6·scale ≤ term_rows − 1`.
/// A terminal shorter than 7 rows cannot honour any scale, so the floor
/// stays 1 (the text then overflows, and the caller's notice says so).
pub fn clamp_scale(requested: usize, term_rows: usize) -> ScaleClamp {
    let max = term_rows.saturating_sub(1) / 6;
    let applied = requested.min(max).max(1);
    ScaleClamp {
        requested,
        applied,
        clamped: requested > max,
    }
}

/// One visible piece of a big strip: the glyph, clipped to the viewport by
/// whole cell columns (§5). The renderer draws the un-clipped middle part,
/// starting at `screen_col`.
///
/// The fields are read by the T-12 renderer; until then the allow keeps the
/// gate green (N-3 convention).
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub struct BigGlyphSlice {
    /// The glyph this slice shows.
    pub glyph: ResolvedGlyph<'static>,
    /// Cell columns of the glyph hidden off the left edge.
    pub clip_left: usize,
    /// Cell columns of the glyph hidden off the right edge.
    pub clip_right: usize,
    /// Screen column where the visible part starts.
    pub screen_col: usize,
    /// The strip scale the engine clipped this slice at.
    pub scale: usize,
}

// Read by the T-12 renderer; N-3 convention until then.
#[allow(dead_code)]
impl BigGlyphSlice {
    /// Cell columns actually visible: `advance − clip_left − clip_right`.
    pub fn visible_cols(&self) -> usize {
        cell_cols(self.glyph.width_px(), self.scale) - self.clip_left - self.clip_right
    }
}

/// The scroll state machine: pure computation, no terminal access.
#[derive(Debug)]
pub struct Engine {
    strip: StripKind,
    width: usize,
    motion: Motion,
    /// Columns travelled in the current cycle; always less than [`Engine::cycle`].
    offset: usize,
    completed: u32,
}

// Everything the run loop (T-7) drives is still uncalled in the binary.
#[allow(dead_code)]
impl Engine {
    /// An engine that scrolls `prepared` through a viewport `width` columns
    /// wide, moving the way `motion` says.
    pub fn new(prepared: PreparedText, width: usize, motion: Motion) -> Self {
        Self {
            strip: StripKind::Text(prepared),
            width,
            motion,
            offset: 0,
            completed: 0,
        }
    }

    /// The big-font twin of [`Engine::new`]: scrolls a big strip through a
    /// viewport `width` *cell columns* wide. `scale` must already satisfy the
    /// §4 height rule — see [`clamp_scale`], which the run loop applies.
    pub fn new_big(strip: BigStrip, width: usize, motion: Motion) -> Self {
        debug_assert!(strip.scale >= 1);
        Self {
            strip: StripKind::Big(strip),
            width,
            motion,
            offset: 0,
            completed: 0,
        }
    }

    /// The viewport width in columns: every frame is exactly this wide.
    pub fn width(&self) -> usize {
        self.width
    }

    /// The travelling width in columns: display columns for text, cell
    /// columns for a big strip.
    pub fn text_width(&self) -> usize {
        self.strip.text_width()
    }

    /// The strip being scrolled.
    pub fn strip(&self) -> &StripKind {
        &self.strip
    }

    /// How many columns of travel make up one cycle at the current width.
    pub fn cycle(&self) -> usize {
        let travel = self.width + self.strip.text_width();
        let cycle = if self.motion.bounce {
            2 * (travel + self.motion.gap)
        } else {
            travel + self.motion.gap
        };
        // Even an empty text in a zero-width terminal has a cycle, so that
        // advancing and counting cycles stay well defined.
        cycle.max(1)
    }

    /// Columns travelled so far in the current cycle.
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// Cycles completed so far.
    pub fn completed_cycles(&self) -> u32 {
        self.completed
    }

    /// Whether the requested number of cycles has run; always false for a
    /// marquee that loops forever.
    pub fn is_finished(&self) -> bool {
        self.motion
            .cycles
            .is_some_and(|cycles| self.completed >= cycles)
    }

    /// Moves the text one column further along its trip, closing and counting
    /// the cycle when the trip is over. Does nothing once finished.
    pub fn advance(&mut self) {
        if self.is_finished() {
            return;
        }
        self.offset += 1;
        let cycle = self.cycle();
        if self.offset >= cycle {
            self.offset -= cycle;
            self.completed += 1;
        }
    }

    /// Adapts to a viewport now `width` columns wide, keeping the text at the
    /// same point of its trip (phase by column ratio).
    pub fn resize(&mut self, width: usize) {
        if width == self.width {
            return;
        }
        self.reflow(|engine| engine.width = width);
    }

    /// The big-font resize (§5): a new viewport width *and* terminal height.
    /// The height re-clamps the strip scale via [`clamp_scale`], which may
    /// change the strip width — both are folded into the same phase-keeping
    /// rule as [`Engine::resize`]. Returns the [`ScaleClamp`] the run loop
    /// needs for its stderr notice.
    pub fn resize_big(&mut self, width: usize, term_rows: usize) -> ScaleClamp {
        let StripKind::Big(strip) = &mut self.strip else {
            panic!("resize_big on a text engine");
        };
        let clamp = clamp_scale(strip.scale, term_rows);
        if width == self.width && !clamp.clamped {
            return clamp;
        }
        let new_scale = clamp.applied;
        self.reflow(|engine| {
            engine.width = width;
            if let StripKind::Big(strip) = &mut engine.strip {
                strip.set_scale(new_scale);
            }
        });
        clamp
    }

    /// Applies `mutate`, then keeps the scroll phase by column ratio across
    /// the cycle change (the rule `Engine::resize` has always used).
    fn reflow(&mut self, mutate: impl FnOnce(&mut Self)) {
        let previous = u64::try_from(self.cycle()).unwrap_or(1).max(1);
        mutate(self);
        let current = u64::try_from(self.cycle()).unwrap_or(1).max(1);
        let offset = u64::try_from(self.offset).unwrap_or(u64::MAX);
        let scaled = offset.saturating_mul(current) / previous;
        // The ratio already lands below `current`; the clamp states the
        // invariant "never stranded beyond the end of a cycle" explicitly.
        self.offset = usize::try_from(scaled.min(current - 1)).unwrap_or(0);
    }

    /// Writes the current frame to `out`: exactly [`Engine::width`] display
    /// columns, whole grapheme clusters only.
    ///
    /// `out` is a parameter so the renderer can hand over one reused buffer
    /// and a steady-state frame allocates nothing.
    ///
    /// Big-font engines do not draw this way: their frames are multi-row cell
    /// grids that the T-12 renderer composes from [`Engine::visible_glyphs`].
    /// A single-line frame from a big engine is a correctly sized blank, so a
    /// careless caller gets an obviously-wrong picture rather than a panic.
    pub fn draw(&self, out: &mut String) {
        out.clear();
        let limit = self.width as i64;
        if limit <= 0 {
            return;
        }

        let StripKind::Text(prepared) = &self.strip else {
            blank(out, limit);
            return;
        };

        // Screen column of the text's first column; it may be off-screen.
        let mut column = self.text_start();
        // Screen columns filled so far, so the frame comes out exactly `width`.
        let mut filled = 0i64;

        for cell in prepared.cells() {
            let cell_width = cell.width() as i64;

            // A zero-width cluster takes no column: keep it only where it
            // would have stood, had that been on screen.
            if cell_width == 0 {
                if column >= filled && column < limit {
                    blank(out, column - filled);
                    filled = column;
                    out.push_str(cell.text());
                }
                continue;
            }

            if column + cell_width <= 0 {
                // Entirely off the left edge.
                column += cell_width;
                continue;
            }
            if column >= limit {
                // Entirely off the right edge, and so is everything after it.
                break;
            }

            let from = column.max(filled);
            blank(out, from - filled);

            let to = (column + cell_width).min(limit);
            if column >= 0 && column + cell_width <= limit {
                out.push_str(cell.text());
            } else {
                // Straddles an edge: a cluster is never split, so the columns
                // it covers on screen stay blank.
                blank(out, to - from);
            }
            filled = to;
            column += cell_width;
        }

        blank(out, limit - filled);
    }

    /// The big-font frame as viewport slices (§5): every glyph whose strip
    /// span intersects `[text_start, text_start + width)`, in strip order,
    /// clipped by whole cell columns at both edges. `out` is caller-owned and
    /// reused, so a steady-state frame allocates nothing.
    ///
    /// Slices tile their part of the viewport without gaps or overlaps: the
    /// clip arithmetic guarantees `screen_col − clip_left` equals the glyph's
    /// (possibly negative) screen start, and one [`Engine::advance`] shifts
    /// every clip by exactly one column — no column jumps between frames.
    pub fn visible_glyphs(&self, out: &mut Vec<BigGlyphSlice>) {
        out.clear();
        let StripKind::Big(strip) = &self.strip else {
            panic!("visible_glyphs on a text engine");
        };
        let limit = self.width as i64;
        if limit <= 0 {
            return;
        }

        let start = self.text_start();
        let scale = strip.scale();
        // Strip column of the glyph currently being examined.
        let mut strip_col: i64 = 0;
        for glyph in strip.glyphs() {
            let advance = cell_cols(glyph.width_px(), scale) as i64;
            // Screen span of this glyph: [glyph_start, glyph_end).
            let glyph_start = start + strip_col;
            let glyph_end = glyph_start + advance;

            if glyph_end <= 0 {
                // Entirely off the left edge.
                strip_col += advance;
                continue;
            }
            if glyph_start >= limit {
                // Entirely off the right edge, and so is everything after it.
                break;
            }

            // The span intersects the viewport: clip by whole columns.
            let clip_left = (-glyph_start).max(0) as usize;
            let clip_right = (glyph_end - limit).max(0) as usize;
            out.push(BigGlyphSlice {
                glyph: *glyph,
                clip_left,
                clip_right,
                screen_col: glyph_start.max(0) as usize,
                scale,
            });
            strip_col += advance;
        }
    }

    /// The screen column the text starts at this frame, following the motion
    /// model in the module documentation.
    fn text_start(&self) -> i64 {
        let width = self.width as i64;
        let text_width = self.strip.text_width() as i64;
        let travel = width + text_width;
        let gap = self.motion.gap as i64;
        let offset = self.offset as i64;

        if !self.motion.bounce {
            return match self.motion.direction {
                Direction::Left => width - offset,
                Direction::Right => offset - text_width,
            };
        }

        // One leg is the trip plus the dwell off the far edge; the next leg
        // comes back the way it went.
        let leg = travel + gap;
        let (returning, into) = if offset < leg {
            (false, offset)
        } else {
            (true, offset - leg)
        };
        // During the dwell the text stays off-screen at the end of the trip.
        let moved = into.min(travel);
        match (self.motion.direction, returning) {
            (Direction::Left, false) | (Direction::Right, true) => width - moved,
            (Direction::Left, true) | (Direction::Right, false) => moved - text_width,
        }
    }
}

/// Appends `columns` spaces.
fn blank(out: &mut String, columns: i64) {
    for _ in 0..columns.max(0) {
        out.push(' ');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::unicode::prepare;
    use std::collections::BTreeSet;
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;

    fn motion(direction: Direction, bounce: bool, gap: usize) -> Motion {
        Motion {
            direction,
            bounce,
            gap,
            cycles: None,
        }
    }

    fn scrolling(text: &str, width: usize, gap: usize) -> Engine {
        Engine::new(prepare(text), width, motion(Direction::Left, false, gap))
    }

    /// Every frame of one cycle, in order, by walking [`Engine::advance`].
    fn frames_of_a_cycle(engine: &mut Engine) -> Vec<String> {
        let mut frame = String::new();
        let mut frames = Vec::new();
        for _ in 0..engine.cycle() {
            engine.draw(&mut frame);
            frames.push(frame.clone());
            engine.advance();
        }
        frames
    }

    /// The frame at one particular point of the trip.
    fn frame_at(engine: &mut Engine, offset: usize) -> String {
        engine.offset = offset;
        let mut frame = String::new();
        engine.draw(&mut frame);
        frame
    }

    #[test]
    fn left_scroll_enters_at_the_right_and_leaves_at_the_left() {
        let mut engine = scrolling("abc", 5, 2);
        assert_eq!(engine.cycle(), 5 + 3 + 2, "travel plus gap");
        assert_eq!(
            frames_of_a_cycle(&mut engine),
            vec![
                "     ", // just off the right edge
                "    a", "   ab", "  abc", " abc ", "abc  ", "bc   ", "c    ",
                "     ", // just off the left edge
                "     ", // the gap
            ]
        );
        // ...and the next cycle starts over at the right edge.
        assert_eq!(frame_at(&mut engine, 0), "     ");
        assert_eq!(frame_at(&mut engine, 1), "    a");
    }

    #[test]
    fn right_scroll_mirrors_left_scroll() {
        let mut rightward = Engine::new(prepare("abc"), 5, motion(Direction::Right, false, 2));
        assert_eq!(rightward.cycle(), 10);
        assert_eq!(
            frames_of_a_cycle(&mut rightward),
            vec![
                "     ", // just off the left edge
                "c    ", "bc   ", "abc  ", " abc ", "  abc", "   ab", "    a",
                "     ", // just off the right edge
                "     ", // the gap
            ]
        );

        // Mirroring a right scroll gives the left scroll of the same text
        // reversed, frame for frame.
        let mut reversed = scrolling("cba", 5, 2);
        for offset in 0..rightward.cycle() {
            let right = frame_at(&mut rightward, offset);
            let mirrored: String = right.graphemes(true).rev().collect();
            assert_eq!(mirrored, frame_at(&mut reversed, offset), "offset {offset}");
        }
    }

    #[test]
    fn a_wider_terminal_shows_the_whole_text_and_still_cycles() {
        let mut engine = scrolling("abc", 10, 2);
        assert_eq!(engine.cycle(), 10 + 3 + 2);
        let frames = frames_of_a_cycle(&mut engine);
        let blank = " ".repeat(10);

        // The whole text is on screen at once, at every position it fits in.
        assert_eq!(
            frames.iter().filter(|frame| frame.contains("abc")).count(),
            10 - 3 + 1
        );
        // It still cycles: the trip starts and ends off-screen, and exactly
        // `gap` blank frames close the cycle.
        assert_eq!(frames[0], blank);
        assert_eq!(&frames[frames.len() - 2..], &[blank.clone(), blank]);
    }

    #[test]
    fn a_cluster_straddling_an_edge_is_dropped_never_split() {
        // 你 is two columns wide, so it straddles an edge whenever the
        // viewport cuts it in half.
        let mut engine = Engine::new(prepare("你a"), 3, motion(Direction::Left, false, 0));
        assert_eq!(engine.text_width(), 3);
        assert_eq!(
            frames_of_a_cycle(&mut engine),
            vec![
                "   ", // 你 starts at column 3: off the right edge
                "   ", // 你 straddles the right edge, so it is dropped
                " 你", // 你 fits exactly in columns 1..3
                "你a", // both fit, filling the viewport
                " a ", // 你 straddles the left edge, so it is dropped
                "a  ", // 你 is off the left edge
            ]
        );
    }

    #[test]
    fn every_frame_is_exactly_the_width_in_columns_of_whole_clusters() {
        let text = "你好🚀ab e\u{301}";
        let prepared = prepare(text);
        let whole: Vec<&str> = prepared.cells().iter().map(|cell| cell.text()).collect();

        for width in [0, 1, 2, 3, 5, 8, 13, 40] {
            for bounce in [false, true] {
                for direction in [Direction::Left, Direction::Right] {
                    let mut engine =
                        Engine::new(prepare(text), width, motion(direction, bounce, 3));
                    for frame in frames_of_a_cycle(&mut engine) {
                        assert_eq!(
                            frame.width(),
                            width,
                            "{width} columns, bounce {bounce}, {frame:?}"
                        );
                        for cluster in frame.graphemes(true) {
                            assert!(
                                cluster == " " || whole.contains(&cluster),
                                "split cluster {cluster:?} in {frame:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn bounce_reverses_at_both_edges_without_skipping_or_repeating() {
        let mut engine = Engine::new(prepare("abc"), 5, motion(Direction::Left, true, 1));
        assert_eq!(
            engine.cycle(),
            2 * (5 + 3 + 1),
            "there and back, dwelling off each edge for the gap"
        );
        assert_eq!(
            frames_of_a_cycle(&mut engine),
            vec![
                "     ", "    a", "   ab", "  abc", " abc ", "abc  ", "bc   ", "c    ",
                "     ", // fully out at the left edge
                "     ", // the dwell there
                "c    ", "bc   ", "abc  ", " abc ", "  abc", "   ab", "    a",
                "     ", // back where it started: fully out at the right edge
            ]
        );

        // Nothing is skipped or doubled: each visible frame appears exactly
        // once per leg, i.e. twice per cycle.
        let frames = frames_of_a_cycle(&mut engine);
        let visible: Vec<&String> = frames.iter().filter(|frame| frame.trim() != "").collect();
        let distinct: BTreeSet<&String> = visible.iter().copied().collect();
        assert_eq!(visible.len(), 14);
        assert_eq!(distinct.len(), visible.len() / 2, "{visible:?}");
    }

    #[test]
    fn bounce_to_the_right_starts_off_the_left_edge() {
        let mut engine = Engine::new(prepare("abc"), 5, motion(Direction::Right, true, 1));
        assert_eq!(
            frames_of_a_cycle(&mut engine),
            vec![
                "     ", "c    ", "bc   ", "abc  ", " abc ", "  abc", "   ab", "    a",
                "     ", // fully out at the right edge
                "     ", // the dwell there
                "    a", "   ab", "  abc", " abc ", "abc  ", "bc   ", "c    ", "     ",
            ]
        );
    }

    #[test]
    fn a_resize_keeps_the_phase_and_never_strands_the_text() {
        let text = "hello 世界";
        let mut engine = scrolling(text, 120, 8);
        let before = engine.cycle();
        let quarter = before / 4;
        for _ in 0..quarter {
            engine.advance();
        }
        assert_eq!(engine.offset(), quarter);

        // 120 -> 70 columns: the same fraction of the new, shorter trip.
        engine.resize(70);
        assert_eq!(engine.width(), 70);
        assert_eq!(engine.cycle(), 70 + engine.text_width() + 8);
        assert_eq!(engine.offset(), quarter * engine.cycle() / before);
        assert!(engine.offset() < engine.cycle());

        // Whatever the new width, the text still appears within the next
        // cycle: nothing is stuck off-screen and nothing wrapped around.
        for width in [1, 3, 40, 70, 200] {
            let mut engine = scrolling(text, 120, 8);
            for _ in 0..quarter {
                engine.advance();
            }
            engine.resize(width);
            assert!(engine.offset() < engine.cycle(), "offset past the cycle");
            let frames = frames_of_a_cycle(&mut engine);
            assert_eq!(frames.len(), engine.cycle());
            assert!(
                frames.iter().any(|frame| frame.trim() != ""),
                "text never appears after resizing to {width}"
            );
            assert!(
                frames.iter().all(|frame| frame.width() == width),
                "a resized frame is still exactly {width} columns"
            );
        }

        // Resizing to the width it already has changes nothing.
        let settled = engine.offset();
        engine.resize(70);
        assert_eq!(engine.offset(), settled);
    }

    #[test]
    fn cycles_stop_after_exactly_the_requested_count() {
        // --once: one cycle of frames, then finished and no further movement.
        let mut engine = Engine::new(
            prepare("abc"),
            5,
            Motion {
                cycles: Some(1),
                ..motion(Direction::Left, false, 2)
            },
        );
        let cycle = engine.cycle();
        assert!(!engine.is_finished());
        for step in 0..cycle {
            assert!(!engine.is_finished(), "finished early at step {step}");
            engine.advance();
        }
        assert!(engine.is_finished());
        assert_eq!(engine.completed_cycles(), 1);
        let settled = engine.offset();
        engine.advance();
        engine.advance();
        assert_eq!(engine.offset(), settled, "a finished marquee stops moving");
        assert_eq!(engine.completed_cycles(), 1);

        // --repeat 3: exactly three cycles.
        let mut engine = Engine::new(
            prepare("abc"),
            5,
            Motion {
                cycles: Some(3),
                ..motion(Direction::Left, false, 2)
            },
        );
        for _ in 0..3 * cycle {
            assert!(!engine.is_finished());
            engine.advance();
        }
        assert!(engine.is_finished());
        assert_eq!(engine.completed_cycles(), 3);

        // No flag: forever, and the trip really does start over.
        let mut engine = scrolling("abc", 5, 2);
        for _ in 0..50 * cycle {
            engine.advance();
        }
        assert!(!engine.is_finished());
        assert_eq!(engine.completed_cycles(), 50);
        assert_eq!(engine.offset(), 0, "back where it started");
    }

    #[test]
    fn empty_text_and_a_zero_width_viewport_are_harmless() {
        let mut engine = scrolling("", 5, 2);
        assert_eq!(engine.text_width(), 0);
        assert_eq!(engine.cycle(), 7);
        assert_eq!(frames_of_a_cycle(&mut engine), vec!["     "; 7]);

        let mut engine = scrolling("abc", 0, 2);
        assert_eq!(engine.cycle(), 5);
        assert_eq!(frames_of_a_cycle(&mut engine), vec![""; 5]);

        // Nothing to draw and nowhere to draw it: still a well-defined cycle.
        let mut engine = Engine::new(prepare(""), 0, motion(Direction::Left, true, 0));
        assert_eq!(engine.cycle(), 1);
        assert_eq!(frames_of_a_cycle(&mut engine), vec![""]);
    }

    #[test]
    fn a_zero_width_cluster_takes_no_column() {
        let mut engine = Engine::new(prepare("a\u{200b}b"), 3, motion(Direction::Left, false, 0));
        assert_eq!(engine.text_width(), 2);
        let frames = frames_of_a_cycle(&mut engine);
        assert_eq!(frames.len(), 5);
        for frame in &frames {
            assert_eq!(frame.width(), 3, "{frame:?}");
        }
        // The zero-width cluster travels with its neighbours.
        assert!(frames.iter().any(|frame| frame.contains("a\u{200b}b")));
    }

    // ---- big-font strips (T-11, docs/big-font-mode.md §5) ----

    use crate::bigfont::BigFontFace;

    fn face() -> BigFontFace<'static> {
        BigFontFace::embedded_zh_hans().expect("the committed asset parses")
    }

    fn big_scrolling(text: &str, scale: usize, width: usize, gap: usize) -> Engine {
        let strip = BigStrip::build(&prepare(text), &face(), scale);
        Engine::new_big(strip, width, motion(Direction::Left, false, gap))
    }

    /// The slices at one point of the trip, driven by the private `offset`
    /// the way the text tests use it.
    fn slices_at(engine: &mut Engine, offset: usize) -> Vec<BigGlyphSlice> {
        engine.offset = offset;
        let mut slices = Vec::new();
        engine.visible_glyphs(&mut slices);
        slices
    }

    /// `(strip column, glyph)` for every cluster of `text` at `scale`, the
    /// geometry `Engine::visible_glyphs` is supposed to reproduce.
    fn strip_positions(text: &str, scale: usize) -> Vec<(i64, ResolvedGlyph<'static>)> {
        let prepared = prepare(text);
        let face = face();
        let mut column = 0i64;
        let mut out = Vec::new();
        for cell in prepared.cells() {
            let glyph = face.resolve_cluster(cell.text());
            out.push((column, glyph));
            column += cell_cols(glyph.width_px(), scale) as i64;
        }
        out
    }

    #[test]
    fn big_strip_width_sums_glyph_advances_for_mixed_text() {
        for scale in [1, 2, 3] {
            // 中=12px, A=6px, 🚀=tofu 12px, space=6px, 口=12px.
            let strip = BigStrip::build(&prepare("中A🚀 口"), &face(), scale);
            assert_eq!(
                strip.strip_width(),
                (12 + 6 + 12 + 6 + 12) * scale,
                "scale {scale}"
            );
            assert_eq!(strip.scale(), scale);
            let engine = Engine::new_big(strip, 7, motion(Direction::Left, false, 2));
            assert_eq!(engine.text_width(), 48 * scale);
            assert_eq!(
                engine.cycle(),
                7 + 48 * scale + 2,
                "motion reuses the strip width"
            );
        }
        // A combining cluster maps to one halfwidth glyph (§3.4), not two.
        let strip = BigStrip::build(&prepare("e\u{301}"), &face(), 1);
        assert_eq!(strip.strip_width(), 6);
    }

    #[test]
    fn bounce_kinematics_reuse_the_pure_core() {
        // 中 at scale 2 is 24 cells; bounce needs "there, dwell, and back".
        let strip = BigStrip::build(&prepare("中"), &face(), 2);
        let engine = Engine::new_big(strip, 10, motion(Direction::Left, true, 3));
        assert_eq!(engine.cycle(), 2 * (10 + 24 + 3));
    }

    #[test]
    fn a_viewport_cutting_a_glyph_clips_it_column_by_column() {
        let mut engine = big_scrolling("中", 1, 5, 0); // strip 12, cycle 17
        assert_eq!(engine.cycle(), 5 + 12);

        // Left scroll: the strip starts at `5 - offset`; 中 spans
        // [start, start+12). Golden values at the interesting offsets:
        //   offset 1: starts at col 4, one column visible, 11 clipped right
        //   offset 5: starts at col 0, five visible, uncut
        //   offset 6: starts at col -1, one clipped left, six clipped right
        for offset in 0..17 {
            let start = 5 - offset as i64;
            let slices = slices_at(&mut engine, offset);
            if start >= 5 || start + 12 <= 0 {
                assert!(slices.is_empty(), "offset {offset}");
                continue;
            }
            assert_eq!(slices.len(), 1, "offset {offset}");
            let slice = &slices[0];
            let clip_left = (-start).max(0) as usize;
            let clip_right = (start + 12 - 5).max(0) as usize;
            assert_eq!(
                (slice.clip_left, slice.clip_right, slice.screen_col),
                (clip_left, clip_right, start.max(0) as usize),
                "offset {offset}"
            );
            assert_eq!(slice.visible_cols(), 12 - clip_left - clip_right);
        }
    }

    #[test]
    fn mixed_width_strips_clip_both_glyphs_correctly() {
        let mut engine = big_scrolling("A中", 1, 4, 0); // A=6, 中=12, strip 18

        // A is entering, 中 still off-screen: A spans [3, 9), clipped right.
        let slices = slices_at(&mut engine, 1);
        assert_eq!(slices.len(), 1);
        assert_eq!(
            (
                slices[0].clip_left,
                slices[0].clip_right,
                slices[0].screen_col
            ),
            (0, 5, 3)
        );
        assert_eq!(slices[0].visible_cols(), 1);

        // A clipped on both edges at once: [-1, 5) against a 4-wide viewport.
        let slices = slices_at(&mut engine, 5);
        assert_eq!(slices.len(), 1);
        assert_eq!(
            (
                slices[0].clip_left,
                slices[0].clip_right,
                slices[0].screen_col
            ),
            (1, 1, 0)
        );
        assert_eq!(slices[0].visible_cols(), 4);

        // A exactly fills the viewport; 中 not yet visible (starts AT the
        // right edge, i.e. outside it).
        let slices = slices_at(&mut engine, 6);
        assert_eq!(slices.len(), 1);
        assert_eq!(
            (
                slices[0].clip_left,
                slices[0].clip_right,
                slices[0].screen_col
            ),
            (2, 0, 0)
        );

        // A leaves while 中 enters: A [-3, 3) left-clipped, 中 [3, 15)
        // right-clipped, together tiling all four columns.
        let slices = slices_at(&mut engine, 7);
        assert_eq!(slices.len(), 2);
        assert_eq!(
            (
                slices[0].clip_left,
                slices[0].clip_right,
                slices[0].screen_col
            ),
            (3, 0, 0)
        );
        assert_eq!(slices[0].visible_cols(), 3);
        assert_eq!(
            (
                slices[1].clip_left,
                slices[1].clip_right,
                slices[1].screen_col
            ),
            (0, 11, 3)
        );
        assert_eq!(slices[1].visible_cols(), 1);
    }

    #[test]
    fn a_one_column_viewport_sweeps_every_strip_column_exactly_once() {
        // The strictest no-column-jump check: a 1-wide viewport, one frame
        // at a time, must show strip columns 0, 1, 2 … 17 in order — never
        // skipping one, never repeating one, never showing two.
        let face = face();
        let a = face.resolve_char('A');
        let zhong = face.resolve_char('中');
        let mut engine = big_scrolling("A中", 1, 1, 0); // strip 18, cycle 19

        for offset in 0..engine.cycle() {
            let slices = slices_at(&mut engine, offset);
            if offset == 0 {
                assert!(slices.is_empty(), "only the left edge at offset 0");
                continue;
            }
            let column = offset - 1; // the strip column under the viewport
            assert_eq!(slices.len(), 1, "offset {offset}");
            let slice = &slices[0];
            let (glyph, glyph_column) = if column < 6 {
                (a, column)
            } else {
                (zhong, column - 6)
            };
            assert_eq!(slice.glyph, glyph, "offset {offset}");
            assert_eq!(slice.clip_left, glyph_column, "offset {offset}");
            assert_eq!(slice.screen_col, 0);
            assert_eq!(slice.visible_cols(), 1);
        }
    }

    #[test]
    fn slices_tile_the_viewport_without_gaps_overlaps_or_jumps() {
        let text = "中A🚀口 e\u{301}";
        for width in [0, 1, 2, 3, 5, 8, 13, 40] {
            let positions = strip_positions(text, 2);
            let strip_width: usize = positions
                .last()
                .map(|(column, glyph)| *column as usize + cell_cols(glyph.width_px(), 2))
                .unwrap_or(0);
            let mut engine = big_scrolling(text, 2, width, 3);
            let cycle = engine.cycle();
            assert_eq!(cycle, width + strip_width + 3);

            for offset in 0..cycle {
                let slices = slices_at(&mut engine, offset);
                let start = engine.text_start();

                // Independent reconstruction of the spec: the slices are
                // exactly the positions intersecting the viewport, clipped
                // by whole columns at both edges, in strip order.
                let expected: Vec<_> = if width == 0 {
                    Vec::new()
                } else {
                    positions
                        .iter()
                        .filter_map(|(column, glyph)| {
                            let glyph_start = start + column;
                            let glyph_end = glyph_start + cell_cols(glyph.width_px(), 2) as i64;
                            if glyph_end <= 0 || glyph_start >= width as i64 {
                                return None;
                            }
                            Some((
                                (-glyph_start).max(0) as usize,
                                (glyph_end - width as i64).max(0) as usize,
                                glyph_start.max(0) as usize,
                                *glyph,
                            ))
                        })
                        .collect()
                };
                assert_eq!(
                    slices.len(),
                    expected.len(),
                    "offset {offset}, width {width}"
                );
                for (slice, (clip_left, clip_right, screen_col, glyph)) in
                    slices.iter().zip(&expected)
                {
                    assert_eq!(
                        (
                            slice.clip_left,
                            slice.clip_right,
                            slice.screen_col,
                            slice.glyph
                        ),
                        (*clip_left, *clip_right, *screen_col, *glyph),
                        "offset {offset}, width {width}"
                    );
                }

                // Structural tiling: consecutive slices abut exactly, the
                // covered run stays inside the viewport, nothing empty.
                let mut covered_to: Option<usize> = None;
                for (index, slice) in slices.iter().enumerate() {
                    let end = slice.screen_col + slice.visible_cols();
                    if let Some(previous_end) = covered_to {
                        assert_eq!(
                            slice.screen_col, previous_end,
                            "gap or overlap at offset {offset}, width {width}, slice {index}"
                        );
                    }
                    assert!(slice.visible_cols() > 0, "offset {offset}, slice {index}");
                    assert!(end <= width, "run off at offset {offset}, width {width}");
                    covered_to = Some(end);
                }
            }
        }
    }

    #[test]
    fn scale_clamp_enforces_one_row_of_margin() {
        // 6·scale ≤ term_rows − 1; a terminal under 7 rows fits no scale at
        // all, so the floor stays 1 and the result is flagged clamped.
        for (requested, rows, applied, clamped) in [
            (1, 7, 1, false), // 6 ≤ 6: the boundary itself is legal
            (2, 7, 1, true),
            (1, 6, 1, true),
            (1, 3, 1, true),
            (4, 25, 4, false), // 24 ≤ 24
            (5, 25, 4, true),
            (2, 13, 2, false), // (13−1)/6 = 2
            (3, 13, 2, true),
            (16, 100, 16, false), // (100−1)/6 = 16
            (17, 100, 16, true),
        ] {
            assert_eq!(
                clamp_scale(requested, rows),
                ScaleClamp {
                    requested,
                    applied,
                    clamped
                },
                "requested {requested}, rows {rows}"
            );
        }
    }

    #[test]
    fn a_big_resize_keeps_the_phase_when_only_the_width_changes() {
        let mut engine = big_scrolling("中A", 2, 120, 8); // strip 36
        let before = engine.cycle();
        let quarter = before / 4;
        for _ in 0..quarter {
            engine.advance();
        }

        let clamp = engine.resize_big(70, 100); // scale 2 fits 100 rows
        assert!(!clamp.clamped);
        assert_eq!(engine.width(), 70);
        assert_eq!(engine.text_width(), 36); // scale untouched
        assert_eq!(engine.cycle(), 70 + 36 + 8);
        assert_eq!(engine.offset(), quarter * engine.cycle() / before);
        assert!(engine.offset() < engine.cycle());
    }

    #[test]
    fn a_big_resize_reclamps_scale_and_keeps_the_phase() {
        let mut engine = big_scrolling("中A", 4, 200, 8); // strip 72
        let before = engine.cycle();
        let third = before / 3;
        for _ in 0..third {
            engine.advance();
        }

        // Rows 8 → max scale (8−1)/6 = 1: the strip shrinks from 72 to 18
        // cells and the trip re-proportions around the same phase.
        let clamp = engine.resize_big(80, 8);
        assert_eq!(
            clamp,
            ScaleClamp {
                requested: 4,
                applied: 1,
                clamped: true
            }
        );
        assert_eq!(engine.text_width(), 18);
        assert_eq!(engine.width(), 80);
        assert_eq!(engine.cycle(), 80 + 18 + 8);
        assert_eq!(engine.offset(), third * engine.cycle() / before);
        assert!(engine.offset() < engine.cycle());

        // The text still turns up within a cycle after the reflow.
        let cycle = engine.cycle();
        assert!((0..cycle).any(|offset| !slices_at(&mut engine, offset).is_empty()));
    }

    #[test]
    fn a_big_resize_to_the_same_geometry_changes_nothing() {
        let mut engine = big_scrolling("中A", 2, 70, 4);
        for _ in 0..5 {
            engine.advance();
        }
        let settled = engine.offset();

        let clamp = engine.resize_big(70, 100); // scale 2 fits 100 rows
        assert!(!clamp.clamped);
        assert_eq!(engine.offset(), settled);
    }

    #[test]
    fn a_text_style_draw_of_a_big_engine_is_a_blank_frame() {
        // Big engines have no single-line frame; a careless caller gets an
        // obviously-blank, correctly-sized one rather than a panic.
        let mut engine = big_scrolling("中A", 2, 9, 1);
        engine.offset = 3;
        let mut frame = String::new();
        engine.draw(&mut frame);
        assert_eq!(frame, "         ");
        assert_eq!(frame.width(), 9);
    }

    #[test]
    fn an_empty_big_strip_is_harmless() {
        let engine = big_scrolling("", 2, 5, 2);
        assert_eq!(engine.text_width(), 0);
        assert_eq!(engine.cycle(), 7);
        let mut slices = Vec::new();
        engine.visible_glyphs(&mut slices);
        assert!(slices.is_empty());
    }

    #[test]
    fn visible_glyphs_reuses_the_callers_buffer() {
        let mut engine = big_scrolling("A中", 2, 6, 0);
        let mut slices = Vec::new();
        engine.offset = 3;
        engine.visible_glyphs(&mut slices);
        let first: Vec<_> = slices
            .iter()
            .map(|slice| (slice.screen_col, slice.clip_left, slice.clip_right))
            .collect();
        // A second call on the same buffer must replace, not append.
        engine.visible_glyphs(&mut slices);
        let second: Vec<_> = slices
            .iter()
            .map(|slice| (slice.screen_col, slice.clip_left, slice.clip_right))
            .collect();
        assert_eq!(first, second);
        assert!(slices.len() <= 2);
    }
}
