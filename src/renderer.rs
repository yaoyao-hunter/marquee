//! Single-line frame drawing and pacing.
//!
//! The renderer owns everything that reaches the screen: it takes the
//! engine's frame, wraps it in the two sequences that put it on the terminal's
//! current line — move to column 0, clear the line — and writes that as one
//! buffer with one flush. No newlines are emitted per frame, the cursor never
//! moves to another line, and no alternate screen is involved, so the marquee
//! occupies exactly the line the shell left it on.
//!
//! Both buffers (the frame, and the bytes it is wrapped into) are reused across
//! frames, so a steady-state frame performs no allocation: [`Engine::draw`]
//! clears and refills the frame buffer, and the escape sequences are queued
//! into a byte buffer that already has the capacity.
//!
//! Redirected output is the exception: with no terminal there is no cursor to
//! reposition, and escape bytes in a file are just noise, so a frame goes out
//! as one plain line and [`Renderer::finish`] has nothing to erase.
//!
//! Pacing lives here too: [`Renderer::delay_until_next_frame`] is how long the
//! run loop should wait before the next frame is due — which is also the right
//! timeout for [`crate::terminal::Terminal::poll`], so a resize or Ctrl+C is
//! handled during the wait instead of after it.
//!
//! Colour is not emitted yet: the palette for main mode is undecided (see
//! board note N-6), so `--no-color` currently has nothing to turn off.

use std::io;
use std::time::{Duration, Instant};

use crossterm::cursor::MoveToColumn;
use crossterm::queue;
use crossterm::style::Print;
use crossterm::terminal::{Clear, ClearType};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::marquee::Engine;
use crate::terminal::Terminal;

/// Draws engine frames onto the terminal's current line, one write and one
/// flush per frame, at the pace the command line asked for.
#[derive(Debug)]
pub struct Renderer {
    /// The engine's frame, reused across frames.
    frame: String,
    /// The frame wrapped in cursor and clear sequences: the single byte
    /// buffer written per frame.
    line: Vec<u8>,
    /// How long one frame lasts, from `--speed`/`--fps`.
    interval: Duration,
    /// When the next frame is due.
    next_due: Instant,
}

impl Renderer {
    /// A renderer pacing one frame per `interval`, the first one due at once.
    pub fn new(interval: Duration) -> Self {
        Self {
            frame: String::new(),
            line: Vec::new(),
            interval,
            next_due: Instant::now(),
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
            queue!(
                self.line,
                MoveToColumn(0),
                Clear(ClearType::CurrentLine),
                Print(&self.frame)
            )?;
        } else {
            self.line.extend_from_slice(self.frame.as_bytes());
            self.line.push(b'\n');
        }

        out.write_all(&self.line)?;
        out.flush()?;
        self.schedule_next_frame();
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
        self.next_due.saturating_duration_since(Instant::now())
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

    /// The reserved capacity of the two reused buffers.
    #[cfg(test)]
    fn buffer_capacity(&self) -> (usize, usize) {
        (self.frame.capacity(), self.line.capacity())
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
        let mut renderer = Renderer::new(Duration::ZERO);
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
                    gap: 1,
                    cycles: None,
                },
            );
            let mut terminal = FakeTerminal::new(width, 3);
            let mut renderer = Renderer::new(Duration::ZERO);

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
        let mut renderer = Renderer::new(Duration::ZERO);
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
        let mut renderer = Renderer::new(Duration::ZERO);
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
        let mut renderer = Renderer::new(Duration::ZERO);
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
        let mut renderer = Renderer::new(Duration::ZERO);
        renderer.finish(&mut terminal).expect("a fake never fails");
        assert_eq!(terminal.frames(), [PREFIX]);
        assert_eq!(terminal.flush_count(), 1);
    }

    #[test]
    fn the_frame_interval_comes_from_speed_or_fps() {
        // The first frame is due immediately; after a draw, one interval.
        let mut renderer = Renderer::new(Duration::from_millis(200));
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
        let renderer = Renderer::new(Duration::ZERO);
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
                Renderer::new(cli.frame_interval()).interval,
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
        let mut renderer = Renderer::new(interval);

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
}
