// The engine (T-11) and renderer (T-12) consume the face, glyphs, cells and
// rasterizer; from_bytes/render_text stay test- and diagnostics-only (N-3).
#[allow(dead_code)]
mod bigfont;
mod cli;
mod marquee;
mod renderer;
mod terminal;
mod unicode;

use std::io::{self, IsTerminal};
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;

use bigfont::BigFontFace;
use cli::Cli;
use marquee::{BigStrip, Direction, Engine, Motion, clamp_scale};
use renderer::{BigRenderer, Renderer};
use terminal::{Terminal, TerminalEvent};
use unicode::{PreparedText, prepare};

/// Exit code for an I/O failure while drawing.
const EXIT_IO: u8 = 1;

/// Exit code for a usage error: bad flags (clap's own code) or no text to
/// scroll.
const EXIT_USAGE: u8 = 2;

/// Scrolls `TEXT` — or piped stdin — across the terminal's current line until
/// it has run the cycles it was asked for, or Ctrl+C arrives.
///
/// Exit codes: `0` for a scroll that ran its cycles, one the user stopped with
/// Ctrl+C, and one whose reader went away (`marquee … | head`); `1` when
/// drawing failed for a real reason; `2` for a usage error (clap's own code for
/// bad flags).
fn main() -> ExitCode {
    let cli = Cli::parse();

    let text = match cli::resolve_text(&cli, io::stdin().is_terminal(), io::stdin().lock()) {
        Ok(text) => text,
        Err(err) => {
            eprintln!("marquee: {err}");
            return ExitCode::from(EXIT_USAGE);
        }
    };

    let mut terminal = terminal::open();
    let scrolled = if cli.big() {
        match big_font(cli.font()) {
            Ok(face) => scroll_big(
                prepare(&text),
                motion_for(&cli),
                cli.frame_interval(),
                cli.scale(),
                &face,
                &mut *terminal,
            ),
            Err(err) => Err(err),
        }
    } else {
        scroll(
            prepare(&text),
            motion_for(&cli),
            cli.frame_interval(),
            &mut *terminal,
        )
    };
    // Dropping the terminal shows the cursor and leaves raw mode, so anything
    // printed about the run lands on a terminal that is ours again.
    drop(terminal);

    match scrolled {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("marquee: {err}");
            ExitCode::from(EXIT_IO)
        }
    }
}

/// The packaged atlas the command line asked for.
fn big_font(font: cli::Font) -> io::Result<BigFontFace<'static>> {
    match font {
        cli::Font::ZhHans => BigFontFace::embedded_zh_hans().map_err(|err| {
            io::Error::other(format!("the embedded big-font asset is damaged: {err}"))
        }),
    }
}

/// The motion the command line asked for, in the engine's own terms: the core
/// stays free of clap.
fn motion_for(cli: &Cli) -> Motion {
    Motion {
        direction: match cli.direction() {
            cli::Direction::Left => Direction::Left,
            cli::Direction::Right => Direction::Right,
        },
        bounce: cli.bounce(),
        gap: cli.gap() as usize,
        cycles: cli.cycles(),
    }
}

/// What the run loop needs from either renderer: pace the frames, draw one,
/// clean up on the way out.
trait FrameSink {
    /// How long to wait before the next frame is due; zero means draw now.
    fn delay_until_next_frame(&self) -> Duration;
    /// Draws the engine's current frame as one buffered write and one flush.
    fn draw(&mut self, out: &mut dyn Terminal, engine: &Engine) -> io::Result<()>;
    /// Leaves the taken lines clean and the cursor parked.
    fn finish(&mut self, out: &mut dyn Terminal) -> io::Result<()>;
}

impl FrameSink for Renderer {
    fn delay_until_next_frame(&self) -> Duration {
        self.delay_until_next_frame()
    }
    fn draw(&mut self, out: &mut dyn Terminal, engine: &Engine) -> io::Result<()> {
        self.draw(out, engine)
    }
    fn finish(&mut self, out: &mut dyn Terminal) -> io::Result<()> {
        self.finish(out)
    }
}

impl FrameSink for BigRenderer {
    fn delay_until_next_frame(&self) -> Duration {
        self.delay_until_next_frame()
    }
    fn draw(&mut self, out: &mut dyn Terminal, engine: &Engine) -> io::Result<()> {
        self.draw(out, engine)
    }
    fn finish(&mut self, out: &mut dyn Terminal) -> io::Result<()> {
        self.finish(out)
    }
}

/// The run loop: wait out the frame interval while listening to the terminal,
/// draw one frame, step the text one column, repeat.
///
/// The wait and the listening are the same call — the interval the renderer
/// still owes is exactly how long [`Terminal::poll`] may block — so a resize or
/// Ctrl+C is handled during the pause rather than after it, and a frame is
/// never late by more than one event. A resize reaches `on_resize` before the
/// next frame is drawn, which re-sizes the engine — and, in big mode,
/// re-clamps the scale — keeping the text at the same point of its trip.
///
/// Every way out — cycles finished, Ctrl+C, an I/O error — ends with
/// [`FrameSink::finish`], which leaves a clean line and the cursor at column 0.
/// A broken pipe is not an error: the reader of a redirected run went away
/// (`marquee … | head -3`), or the terminal window did, and either way there is
/// nobody left to scroll for, so the run simply ends and exits 0 in silence.
fn run_loop(
    mut engine: Engine,
    mut renderer: impl FrameSink,
    terminal: &mut dyn Terminal,
    mut on_resize: impl FnMut(usize, usize, &mut Engine),
) -> io::Result<()> {
    let outcome = 'scroll: loop {
        match terminal.poll(renderer.delay_until_next_frame()) {
            Ok(events) => {
                for event in events {
                    match event {
                        TerminalEvent::Quit => break 'scroll Ok(()),
                        TerminalEvent::Resize { columns, rows } => {
                            // A zero-width report is a window that is hidden or
                            // still being laid out; the last real width is the
                            // better guess, and it keeps the ratio arithmetic
                            // in resize() meaningful.
                            if columns > 0 {
                                on_resize(columns, rows, &mut engine);
                            }
                        }
                    }
                }
            }
            Err(err) => break 'scroll Err(err),
        }

        if let Err(err) = renderer.draw(terminal, &engine) {
            break 'scroll Err(err);
        }
        engine.advance();
        if engine.is_finished() {
            break 'scroll Ok(());
        }
    };

    match outcome.and(renderer.finish(terminal)) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(err) => Err(err),
    }
}

/// Scrolls one line of text.
fn scroll(
    prepared: PreparedText,
    motion: Motion,
    interval: Duration,
    terminal: &mut dyn Terminal,
) -> io::Result<()> {
    let engine = Engine::new(prepared, terminal.width(), motion);
    let renderer = Renderer::new(interval);
    run_loop(engine, renderer, terminal, |columns, _rows, engine| {
        engine.resize(columns);
    })
}

/// Scrolls the text as big pixel glyphs over `6·scale` rows.
///
/// The requested scale is fitted to the terminal height (§4: `6·scale ≤
/// term_rows − 1`) before the first frame, and re-fitted on every resize —
/// each time it actually changes, a notice goes to stderr so the user knows
/// why their `--scale` was reduced.
fn scroll_big(
    prepared: PreparedText,
    motion: Motion,
    interval: Duration,
    requested_scale: usize,
    face: &BigFontFace<'static>,
    terminal: &mut dyn Terminal,
) -> io::Result<()> {
    let (columns, rows) = terminal.size();
    let clamp = clamp_scale(requested_scale, rows);
    if clamp.clamped {
        clamp_notice(clamp.requested, clamp.applied, rows);
    }
    let strip = BigStrip::build(&prepared, face, clamp.applied);
    let engine = Engine::new_big(strip, columns, motion);
    let renderer = BigRenderer::new(interval);
    let mut applied = clamp.applied;

    run_loop(engine, renderer, terminal, |columns, rows, engine| {
        let clamp = engine.resize_big(columns, rows);
        if clamp.applied != applied {
            clamp_notice(clamp.requested, clamp.applied, rows);
            applied = clamp.applied;
        }
    })
}

/// The stderr notice for a `--scale` the terminal cannot fit.
fn clamp_notice(requested: usize, applied: usize, rows: usize) {
    eprintln!(
        "marquee: --scale {requested} does not fit {rows} terminal rows; scrolling at --scale {applied}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{FakeTerminal, PlainTerminal};
    use std::io::Write;
    use unicode_width::UnicodeWidthStr;

    /// What every frame starts with, and all that [`Renderer::finish`] emits.
    const CLEAN_LINE: &str = "\x1b[1G\x1b[2K";

    /// Runs the real loop with `args` against a fake terminal. The text comes
    /// from the command line, never from stdin: a test must not depend on how
    /// it was started.
    fn scroll_with(args: &[&str], terminal: &mut dyn Terminal) -> io::Result<()> {
        let cli = Cli::try_parse_from(std::iter::once("marquee").chain(args.iter().copied()))
            .expect("the command line should parse");
        let text = cli.text().expect("a test always passes a TEXT");
        scroll(
            prepare(text),
            motion_for(&cli),
            cli.frame_interval(),
            terminal,
        )
    }

    /// The engine the loop will build for `args` in a `columns`-wide terminal:
    /// how many frames one run owes, without repeating the arithmetic here.
    fn reference(args: &[&str], columns: usize) -> Engine {
        let cli = Cli::try_parse_from(std::iter::once("marquee").chain(args.iter().copied()))
            .expect("the command line should parse");
        Engine::new(
            prepare(cli.text().expect("a test always passes a TEXT")),
            columns,
            motion_for(&cli),
        )
    }

    /// The visible part of an emitted frame.
    fn visible(frame: &str) -> &str {
        frame
            .strip_prefix(CLEAN_LINE)
            .unwrap_or_else(|| panic!("frame does not start with {CLEAN_LINE:?}: {frame:?}"))
    }

    #[test]
    fn once_scrolls_exactly_one_cycle_and_leaves_a_clean_line() {
        let args = ["--speed", "1", "--once", "你好世界"];
        let mut terminal = FakeTerminal::new(10, 3);
        let expected = reference(&args, 10).cycle();

        scroll_with(&args, &mut terminal).expect("a fake never fails");

        // One frame per column of travel, plus the cleanup that erases the line.
        assert_eq!(terminal.flush_count(), expected + 1);
        assert_eq!(terminal.frames().len(), expected + 1);
        assert_eq!(
            terminal.frames()[expected],
            CLEAN_LINE,
            "the line is left clean"
        );
        for frame in &terminal.frames()[..expected] {
            assert_eq!(visible(frame).width(), 10, "{frame:?}");
        }
        assert!(
            !terminal.written().contains('\n'),
            "a scroll on a terminal never emits a newline"
        );
    }

    #[test]
    fn repeat_n_scrolls_exactly_n_cycles() {
        for cycles in [1u32, 2, 5] {
            let wanted = cycles.to_string();
            let args = ["--speed", "1", "--repeat", &wanted, "--gap", "0", "ab"];
            let mut terminal = FakeTerminal::new(6, 3);
            let one_cycle = reference(&args, 6).cycle();

            scroll_with(&args, &mut terminal).expect("a fake never fails");

            assert_eq!(
                terminal.frames().len(),
                one_cycle * usize::try_from(cycles).unwrap() + 1,
                "{cycles} cycles of {one_cycle} frames"
            );
        }
    }

    #[test]
    fn ctrl_c_stops_the_scroll_mid_trip_and_leaves_a_clean_line() {
        // No --once and no --repeat: this loop only ends because the user said
        // so. Three frames in, Ctrl+C arrives.
        let mut terminal = FakeTerminal::new(12, 3);
        terminal.push_event_at(4, TerminalEvent::Quit);

        scroll_with(&["--speed", "1", "你好 · Hello 🚀"], &mut terminal)
            .expect("Ctrl+C is a clean exit, not an error");

        assert_eq!(terminal.frames().len(), 4, "three frames plus the cleanup");
        assert_eq!(terminal.frames()[3], CLEAN_LINE);
        assert_eq!(
            terminal.poll_count(),
            4,
            "one poll per frame, then the quit"
        );
        assert!(
            terminal.frames()[..3]
                .iter()
                .all(|frame| visible(frame).width() == 12),
            "{:?}",
            terminal.frames()
        );
    }

    #[test]
    fn a_resize_mid_scroll_takes_effect_on_the_very_next_frame() {
        let mut terminal = FakeTerminal::new(120, 30);
        // A real terminal reports the new size and the event together.
        terminal.push_resize_at(4, 70, 24);

        scroll_with(
            &["--speed", "1", "--repeat", "1", "你好世界"],
            &mut terminal,
        )
        .expect("a fake never fails");

        let frames = terminal.frames();
        assert!(
            frames.len() > 5,
            "the scroll should have run on: {frames:?}"
        );
        for frame in &frames[..3] {
            assert_eq!(visible(frame).width(), 120, "{frame:?}");
        }
        // Frame 3 is drawn after the fourth poll, which is when the resize
        // arrives: from there on every frame is exactly the new width.
        for frame in &frames[3..frames.len() - 1] {
            assert_eq!(visible(frame).width(), 70, "{frame:?}");
        }
        assert_eq!(frames[frames.len() - 1], CLEAN_LINE);
    }

    #[test]
    fn an_infinite_scroll_keeps_going_until_stopped() {
        let mut terminal = FakeTerminal::new(10, 3);
        let one_cycle = reference(&["--speed", "1", "abcd"], 10).cycle();
        // Stop it well past the point where a --once run would have ended.
        terminal.push_event_at(one_cycle * 3, TerminalEvent::Quit);

        scroll_with(&["--speed", "1", "abcd"], &mut terminal).expect("a fake never fails");

        // The quit lands on that poll, so one frame fewer is drawn — and the
        // cleanup frame brings the count back to exactly three cycles' worth.
        assert_eq!(
            terminal.frames().len(),
            one_cycle * 3,
            "the default is to loop forever"
        );
    }

    /// A terminal whose every write fails, as a closed pipe does.
    struct Failing {
        columns: usize,
        kind: io::ErrorKind,
    }

    impl Write for Failing {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(self.kind, "the reader went away"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Terminal for Failing {
        fn size(&self) -> (usize, usize) {
            (self.columns, 3)
        }

        fn poll(&mut self, _timeout: Duration) -> io::Result<Vec<TerminalEvent>> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn a_broken_pipe_ends_the_scroll_quietly() {
        // `marquee "..." | head -3`: the reader closes, every write fails, and
        // that is a normal stop rather than something to report.
        let mut terminal = Failing {
            columns: 10,
            kind: io::ErrorKind::BrokenPipe,
        };
        scroll_with(&["--speed", "1", "abcd"], &mut terminal)
            .expect("a closed pipe is not an error");
    }

    #[test]
    fn any_other_write_failure_is_still_reported() {
        let mut terminal = Failing {
            columns: 10,
            kind: io::ErrorKind::WriteZero,
        };
        let err = scroll_with(&["--speed", "1", "--once", "abcd"], &mut terminal)
            .expect_err("a real I/O failure must not be swallowed");
        assert_eq!(err.kind(), io::ErrorKind::WriteZero);
    }

    #[test]
    fn redirected_output_is_plain_lines_and_still_stops_after_its_cycles() {
        let args = ["--speed", "1", "--once", "--gap", "0", "你好"];
        let expected = reference(&args, 12).cycle();
        let mut terminal = PlainTerminal::new(Vec::new(), (12, 3));

        scroll_with(&args, &mut terminal).expect("a Vec never fails");

        let text = String::from_utf8(terminal.into_inner()).expect("plain frames are text");
        assert!(
            !text.contains('\u{1b}'),
            "an escape byte reached redirected output: {text:?}"
        );
        assert_eq!(text.lines().count(), expected, "{text:?}");
        assert!(
            text.lines().all(|line| line.width() == 12),
            "every line fills the width: {text:?}"
        );
    }

    // ---- the big-font run loop (T-12) ----

    use crate::bigfont::BigFontFace;

    /// Runs the real big loop with `args` against a fake terminal.
    fn scroll_big_with(args: &[&str], terminal: &mut dyn Terminal) -> io::Result<()> {
        let cli = Cli::try_parse_from(std::iter::once("marquee").chain(args.iter().copied()))
            .expect("the command line should parse");
        let text = cli.text().expect("a test always passes a TEXT");
        let face = BigFontFace::embedded_zh_hans().expect("the committed asset parses");
        scroll_big(
            prepare(text),
            motion_for(&cli),
            cli.frame_interval(),
            cli.scale(),
            &face,
            terminal,
        )
    }

    /// A frame with the escape sequences stripped: just the printed text.
    fn strip_escapes(frame: &str) -> String {
        let mut plain = String::new();
        let mut rest = frame;
        while let Some(at) = rest.find('\x1b') {
            plain.push_str(&rest[..at]);
            let after = &rest[at + 1..];
            let end = after
                .find(['A', 'B', 'G', 'K', 'h'])
                .map(|i| i + 1)
                .unwrap_or(after.len());
            rest = &after[end..];
        }
        plain.push_str(rest);
        plain
    }

    #[test]
    fn big_once_scrolls_one_cycle_of_rows_and_cleans_up() {
        // 你好 at scale 1: 24 cell columns; a 12-wide viewport, no gap.
        let args = ["--big", "--once", "--gap", "0", "--speed", "1", "你好"];
        let mut terminal = FakeTerminal::new(12, 24);
        let expected = 12 + 24; // one cycle of travel

        scroll_big_with(&args, &mut terminal).expect("a fake never fails");

        let frames = terminal.frames();
        assert_eq!(
            frames.len(),
            expected + 1,
            "one frame per column plus cleanup"
        );
        for frame in &frames[..expected] {
            assert!(
                frame.starts_with("\x1b[1G\x1b[5A\x1b[2K"),
                "six rows from the top of the region: {frame:?}"
            );
            assert_eq!(frame.matches("\x1b[2K").count(), 6, "one clear per row");
            assert_eq!(
                strip_escapes(frame).chars().count(),
                72,
                "6 rows × 12 columns"
            );
        }
        // The cleanup: six cleared rows, no text.
        assert_eq!(strip_escapes(&frames[expected]).chars().count(), 0);
        assert_eq!(frames[expected].matches("\x1b[2K").count(), 6);
        assert!(
            !terminal.written().contains('\n'),
            "a big scroll never emits a newline: {:?}",
            terminal.written()
        );
    }

    #[test]
    fn big_scale_is_clamped_to_the_terminal_height() {
        // 20 rows fit a scale of at most (20−1)/6 = 3: 18 rows of cells.
        let args = [
            "--big", "--once", "--gap", "0", "--speed", "1", "--scale", "4", "中",
        ];
        let mut terminal = FakeTerminal::new(12, 20);

        scroll_big_with(&args, &mut terminal).expect("a fake never fails");

        let frames = terminal.frames();
        for frame in &frames[..frames.len() - 1] {
            assert!(
                frame.starts_with("\x1b[1G\x1b[17A\x1b[2K"),
                "18 rows from the top of the region: {frame:?}"
            );
            assert_eq!(frame.matches("\x1b[2K").count(), 18);
            assert_eq!(strip_escapes(frame).chars().count(), 18 * 12);
        }
    }

    #[test]
    fn a_big_resize_adapts_the_width_and_reclamps_the_scale_live() {
        // Scale 2 (12 rows) in a 40-row terminal; the window then shrinks to
        // 8 rows, which fits only scale 1: 6 rows from the next frame on.
        let args = [
            "--big", "--repeat", "1", "--gap", "0", "--speed", "1", "--scale", "2", "中",
        ];
        let mut terminal = FakeTerminal::new(60, 40);
        terminal.push_resize_at(4, 30, 8);

        scroll_big_with(&args, &mut terminal).expect("a fake never fails");

        let frames = terminal.frames();
        assert!(
            frames.len() > 5,
            "the scroll should have run on: {frames:?}"
        );
        for frame in &frames[..3] {
            assert_eq!(frame.matches("\x1b[2K").count(), 12, "scale 2 is 12 rows");
            assert_eq!(strip_escapes(frame).chars().count(), 12 * 60, "{frame:?}");
        }
        for frame in &frames[3..frames.len() - 1] {
            assert_eq!(
                frame.matches("\x1b[2K").count(),
                6,
                "the re-clamped scale 1 is 6 rows: {frame:?}"
            );
            assert_eq!(strip_escapes(frame).chars().count(), 6 * 30, "{frame:?}");
        }
    }

    #[test]
    fn big_ctrl_c_stops_mid_scroll_and_leaves_the_rows_clean() {
        let mut terminal = FakeTerminal::new(12, 24);
        terminal.push_event_at(4, TerminalEvent::Quit);

        scroll_big_with(&["--big", "--speed", "1", "你好世界"], &mut terminal)
            .expect("Ctrl+C is a clean exit, not an error");

        let frames = terminal.frames();
        assert_eq!(frames.len(), 4, "three frames plus the cleanup");
        for frame in &frames[..3] {
            assert_eq!(frame.matches("\x1b[2K").count(), 6, "{frame:?}");
        }
        assert_eq!(strip_escapes(&frames[3]).chars().count(), 0);
        assert_eq!(
            frames[3].matches("\x1b[2K").count(),
            6,
            "all six rows erased"
        );
        assert!(
            !terminal.written().contains('\n'),
            "{:?}",
            terminal.written()
        );
    }

    #[test]
    fn every_main_mode_option_keeps_working_in_big_mode() {
        // Bounce, direction, repeat, align and no-color all parse and run:
        // a bouncing run does there-and-back, so exactly one cycle is
        // 2×(travel + gap) frames.
        let args = [
            "--big",
            "--bounce",
            "--direction",
            "right",
            "--align",
            "center",
            "--no-color",
            "--repeat",
            "1",
            "--gap",
            "0",
            "--speed",
            "1",
            "你好",
        ];
        let mut terminal = FakeTerminal::new(12, 24);

        scroll_big_with(&args, &mut terminal).expect("a fake never fails");

        let frames = terminal.frames();
        assert_eq!(
            frames.len(),
            2 * (12 + 24) + 1,
            "one bounce cycle plus cleanup"
        );
        for frame in &frames[..frames.len() - 1] {
            assert_eq!(frame.matches("\x1b[2K").count(), 6, "{frame:?}");
        }
    }

    #[test]
    fn a_broken_pipe_ends_a_big_scroll_quietly() {
        let mut terminal = Failing {
            columns: 10,
            kind: io::ErrorKind::BrokenPipe,
        };
        scroll_big_with(&["--big", "--speed", "1", "abcd"], &mut terminal)
            .expect("a closed pipe is not an error");
    }

    #[test]
    fn any_other_big_write_failure_is_still_reported() {
        let mut terminal = Failing {
            columns: 10,
            kind: io::ErrorKind::WriteZero,
        };
        let err = scroll_big_with(&["--big", "--once", "--speed", "1", "abcd"], &mut terminal)
            .expect_err("a real I/O failure must not be swallowed");
        assert_eq!(err.kind(), io::ErrorKind::WriteZero);
    }
}
