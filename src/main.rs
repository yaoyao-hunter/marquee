// The big-font face and glyph types are consumed by the scroll engine (T-11);
// the Rasterizer still waits for the T-12 renderer, so parts of the module
// tree remain dead until then (see vault note N-3).
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

use cli::Cli;
use marquee::{Direction, Engine, Motion};
use renderer::Renderer;
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
    let scrolled = scroll(
        prepare(&text),
        motion_for(&cli),
        cli.frame_interval(),
        &mut *terminal,
    );
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

/// The run loop: wait out the frame interval while listening to the terminal,
/// draw one frame, step the text one column, repeat.
///
/// The wait and the listening are the same call — the interval the renderer
/// still owes is exactly how long [`Terminal::poll`] may block — so a resize or
/// a Ctrl+C is handled during the pause rather than after it, and a frame is
/// never late by more than one event. A resize changes the engine's width
/// before the next frame is drawn, keeping the text at the same point of its
/// trip; the renderer clamps the frame in the meantime, so no frame can be
/// wider than the terminal it is drawn into.
///
/// Every way out — cycles finished, Ctrl+C, an I/O error — ends with
/// [`Renderer::finish`], which leaves a clean line and the cursor at column 0.
/// A broken pipe is not an error: the reader of a redirected run went away
/// (`marquee … | head -3`), or the terminal window did, and either way there is
/// nobody left to scroll for, so the run simply ends and exits 0 in silence.
fn scroll(
    prepared: PreparedText,
    motion: Motion,
    interval: Duration,
    terminal: &mut dyn Terminal,
) -> io::Result<()> {
    let mut engine = Engine::new(prepared, terminal.width(), motion);
    let mut renderer = Renderer::new(interval);

    let outcome = 'scroll: loop {
        match terminal.poll(renderer.delay_until_next_frame()) {
            Ok(events) => {
                for event in events {
                    match event {
                        TerminalEvent::Quit => break 'scroll Ok(()),
                        TerminalEvent::Resize { columns, .. } => {
                            // A zero-width report is a window that is hidden or
                            // still being laid out; the last real width is the
                            // better guess, and it keeps the ratio arithmetic
                            // in resize() meaningful.
                            if columns > 0 {
                                engine.resize(columns);
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
}
