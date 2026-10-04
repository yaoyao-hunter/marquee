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
//! The engine keeps its own [`Direction`] so this core stays free of clap; the
//! run loop maps `--direction` onto it.

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

/// The scroll state machine: pure computation, no terminal access.
#[derive(Debug)]
pub struct Engine {
    prepared: PreparedText,
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
            prepared,
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

    /// The text's own width in columns.
    pub fn text_width(&self) -> usize {
        self.prepared.width()
    }

    /// How many columns of travel make up one cycle at the current width.
    pub fn cycle(&self) -> usize {
        let travel = self.width + self.prepared.width();
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
        let previous = u64::try_from(self.cycle()).unwrap_or(1).max(1);
        self.width = width;
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
    pub fn draw(&self, out: &mut String) {
        out.clear();
        let limit = self.width as i64;
        if limit <= 0 {
            return;
        }

        // Screen column of the text's first column; it may be off-screen.
        let mut column = self.text_start();
        // Screen columns filled so far, so the frame comes out exactly `width`.
        let mut filled = 0i64;

        for cell in self.prepared.cells() {
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

    /// The screen column the text starts at this frame, following the motion
    /// model in the module documentation.
    fn text_start(&self) -> i64 {
        let width = self.width as i64;
        let text_width = self.prepared.width() as i64;
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
}
