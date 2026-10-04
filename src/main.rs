// The engine (T-11) and renderer (T-12) consume the face, glyphs, cells and
// rasterizer; from_bytes/render_text stay test- and diagnostics-only (N-3).
#[allow(dead_code)]
mod bigfont;
mod cli;
mod marquee;
mod renderer;
mod terminal;
mod theme;
mod unicode;

use std::io::{self, IsTerminal};
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;

use bigfont::BigFontFace;
use cli::Cli;
use marquee::{BigStrip, Direction, Engine, Motion, min_term_size};
use renderer::{BigRenderer, Renderer};
use terminal::{Terminal, TerminalEvent};
use theme::{Registry, Theme, ThemeError};
use unicode::{PreparedText, prepare};

/// Exit code for an I/O failure while drawing.
const EXIT_IO: u8 = 1;

/// Exit code for a usage error: bad flags (clap's own code), no text to
/// scroll, a theme that cannot be resolved, or a terminal too small for
/// the big mode asked for.
const EXIT_USAGE: u8 = 2;

/// Scrolls `TEXT` — or piped stdin — across the terminal's current line until
/// it has run the cycles it was asked for, or Ctrl+C arrives.
///
/// Exit codes: `0` for a scroll that ran its cycles, one the user stopped with
/// Ctrl+C, and one whose reader went away (`marquee … | head`); `1` when
/// drawing failed for a real reason; `2` for a usage error (clap's own code for
/// bad flags, no text, an unusable theme, or a terminal smaller than big
/// mode needs).
fn main() -> ExitCode {
    let cli = Cli::parse();

    let text = match cli::resolve_text(&cli, io::stdin().is_terminal(), io::stdin().lock()) {
        Ok(text) => text,
        Err(err) => {
            eprintln!("marquee: {err}");
            return ExitCode::from(EXIT_USAGE);
        }
    };

    let theme = match resolve_theme(&cli, no_color_from_env()) {
        Ok(theme) => theme,
        Err(err) => {
            eprintln!("marquee: {err}");
            return ExitCode::from(EXIT_USAGE);
        }
    };

    let mut terminal = terminal::open();
    let scrolled = if cli.big() {
        // The size gate: big mode names the terminal it needs, and anything
        // smaller is refused before a single frame takes the line over.
        let (columns, rows) = terminal.size();
        if let Some(message) = big_size_error(cli.scale(), columns, rows) {
            drop(terminal);
            eprintln!("marquee: {message}");
            return ExitCode::from(EXIT_USAGE);
        }
        match big_font(cli.font()) {
            Ok(face) => scroll_big(
                prepare(&text),
                motion_for(&cli),
                cli.frame_interval(),
                theme,
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
            theme,
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

/// The theme `--theme` asked for, from the registry of built-ins merged with
/// the theme file — or the plain theme when colour is off. The name is
/// validated either way, so a typo in `--theme nope --no-color` is still
/// reported rather than silently ignored.
fn resolve_theme(cli: &Cli, no_color: bool) -> Result<Theme, ThemeError> {
    let mut registry = Registry::builtins();
    let file = match cli.theme_file() {
        // An explicit file must exist and parse; the default location is
        // optional — no file simply means the built-ins.
        Some(path) => Some((path.to_path_buf(), true)),
        None => theme::default_theme_path().map(|path| (path, false)),
    };
    if let Some((path, required)) = file {
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let user = theme::parse_theme_file(&text)?;
                registry.extend(user);
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound && !required => {}
            Err(err) => {
                return Err(ThemeError::Read {
                    path: path.to_path_buf(),
                    err,
                });
            }
        }
    }

    let theme = registry
        .lookup(cli.theme())
        .cloned()
        .ok_or_else(|| ThemeError::UnknownTheme {
            name: cli.theme().to_string(),
            available: registry.names().into_iter().map(str::to_string).collect(),
        })?;
    if no_color || !cli.color_enabled() {
        // The user asked for no colour at all: the theme resolves to the
        // plain one, which emits nothing.
        return Ok(Theme::default());
    }
    Ok(theme)
}

/// Whether the environment says colour is off: a non-empty `NO_COLOR` (the
/// https://no-color.org convention — an empty value means "not set").
fn no_color_from_env() -> bool {
    std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty())
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
        continuous: cli.continuous(),
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
    theme: Theme,
    terminal: &mut dyn Terminal,
) -> io::Result<()> {
    let engine = Engine::new(prepared, terminal.width(), motion);
    let renderer = Renderer::new(interval, theme);
    run_loop(engine, renderer, terminal, |columns, _rows, engine| {
        engine.resize(columns);
    })
}

/// Scrolls the text as big pixel glyphs over `6·scale` rows.
///
/// The caller has already refused a terminal smaller than big mode needs
/// (see [`big_size_error`]); here only a resize can break the rule, and
/// that is tolerated rather than fatal: the scale re-clamps to the height —
/// down when the window shrinks, back up when it grows again — and one
/// notice goes to stderr naming the size the run needs, without stopping
/// the scroll.
#[allow(clippy::too_many_arguments)]
fn scroll_big(
    prepared: PreparedText,
    motion: Motion,
    interval: Duration,
    theme: Theme,
    requested_scale: usize,
    face: &BigFontFace<'static>,
    terminal: &mut dyn Terminal,
) -> io::Result<()> {
    let strip = BigStrip::build(&prepared, face, requested_scale);
    let engine = Engine::new_big(strip, terminal.width(), motion);
    let renderer = BigRenderer::new(interval, theme);
    // Whether the too-small notice is already standing on stderr, so a run
    // that dips below the limit says it once, not once per resize event.
    let mut noticed = false;

    run_loop(engine, renderer, terminal, |columns, rows, engine| {
        let clamp = engine.resize_big(columns, rows);
        let (min_columns, min_rows) = min_term_size(requested_scale);
        if columns >= min_columns && rows >= min_rows {
            // Fits again: the next dip below the limit is news again.
            noticed = false;
        } else if !noticed {
            eprintln!(
                "marquee: {}",
                too_small_notice(requested_scale, clamp.applied)
            );
            noticed = true;
        }
    })
}

/// Why big mode cannot start at `scale` on a `columns × rows` terminal:
/// smaller than one whole full-width glyph, or shorter than the glyph rows
/// plus a line of margin (§4, [`min_term_size`]). `None` means it fits.
fn big_size_error(scale: usize, columns: usize, rows: usize) -> Option<String> {
    let (min_columns, min_rows) = min_term_size(scale);
    (columns < min_columns || rows < min_rows).then(|| {
        format!(
            "the terminal is too small for --big --scale {scale}: \
             needs at least {min_columns} columns × {min_rows} rows, \
             this terminal is {columns}×{rows}"
        )
    })
}

/// The stderr notice for a terminal that shrank below what the run needs:
/// the scroll carries on degraded (scale re-clamped, glyphs clipped), so
/// the user is told the size that brings it back.
fn too_small_notice(scale: usize, applied: usize) -> String {
    let (min_columns, min_rows) = min_term_size(scale);
    format!(
        "the terminal became too small for --big --scale {scale}: \
         needs at least {min_columns} columns × {min_rows} rows, \
         scrolling at --scale {applied} until it fits again"
    )
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
    /// it was started, nor on whatever themes the developer's home holds —
    /// the plain theme is what the run gets.
    fn scroll_with(args: &[&str], terminal: &mut dyn Terminal) -> io::Result<()> {
        let cli = Cli::try_parse_from(std::iter::once("marquee").chain(args.iter().copied()))
            .expect("the command line should parse");
        let text = cli.text().expect("a test always passes a TEXT");
        scroll(
            prepare(text),
            motion_for(&cli),
            cli.frame_interval(),
            Theme::default(),
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

    // ---- continuous scrolling (--continuous) ----

    #[test]
    fn continuous_scrolls_periods_and_repeats_count_them() {
        for periods in [1u32, 3] {
            let wanted = periods.to_string();
            let args = ["--speed", "1", "--continuous", "--repeat", &wanted, "ab"];
            let mut terminal = FakeTerminal::new(6, 3);
            // One period: "ab" (2) + gap 8 = 10 frames.
            let period = reference(&args, 6).cycle();

            scroll_with(&args, &mut terminal).expect("a fake never fails");

            assert_eq!(
                terminal.frames().len(),
                period * usize::try_from(periods).unwrap() + 1,
                "{periods} periods of {period} frames plus cleanup"
            );
            assert_eq!(terminal.frames().last(), Some(&CLEAN_LINE.to_string()));
            assert!(
                !terminal.written().contains('\n'),
                "a continuous scroll never emits a newline"
            );
        }
    }

    #[test]
    fn a_continuous_run_never_shows_a_blank_frame_once_started() {
        // 6-wide viewport, "abcd" with gap 4: the gap is narrower than the
        // viewport, so after the first column enters every frame shows
        // something — which is the whole point of --continuous.
        let args = [
            "--speed",
            "1",
            "--continuous",
            "--gap",
            "4",
            "--repeat",
            "3",
            "abcd",
        ];
        let mut terminal = FakeTerminal::new(6, 3);

        scroll_with(&args, &mut terminal).expect("a fake never fails");

        let frames = &terminal.frames()[..terminal.frames().len() - 1];
        let entered = frames
            .iter()
            .position(|frame| visible(frame).trim() != "")
            .expect("the text enters");
        assert!(
            frames[entered..]
                .iter()
                .all(|frame| visible(frame).trim() != ""),
            "a blank frame after column {entered}: {:?}",
            &frames[entered..]
        );
    }

    // ---- the theme layer (--theme, --theme-file, NO_COLOR) ----

    /// A theme file in the temp dir, with a per-test name so parallel tests
    /// cannot collide. Returned as the `--theme-file` path.
    fn theme_file(name: &str, contents: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("marquee-theme-{name}-{}.toml", std::process::id()));
        std::fs::write(&path, contents).expect("the temp dir is writable");
        path
    }

    fn cli_with(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("marquee").chain(args.iter().copied()))
            .expect("the command line should parse")
    }

    #[test]
    fn a_theme_file_overrides_builtins_and_adds_new_themes() {
        let path = theme_file(
            "user",
            "[alarm]\nfg = cyan\n\n[mine]\nfg = \"#7fd4ff\"\nbg = blue\nbold = true\n",
        );
        let cli = cli_with(&[
            "--theme-file",
            path.to_str().expect("a temp path is UTF-8"),
            "--theme",
            "mine",
            "hi",
        ]);

        let theme = resolve_theme(&cli, false).expect("the file resolves");
        assert_eq!(
            theme,
            Theme {
                fg: vec![theme::Color::Rgb {
                    red: 0x7f,
                    green: 0xd4,
                    blue: 0xff
                }],
                bg: vec![theme::Color::Blue],
                bold: true,
                ..Theme::default()
            }
        );

        // A built-in overridden by the file resolves to the file's version.
        let cli = cli_with(&[
            "--theme-file",
            path.to_str().expect("a temp path is UTF-8"),
            "--theme",
            "alarm",
            "hi",
        ]);
        assert_eq!(
            resolve_theme(&cli, false).expect("alarm resolves"),
            Theme {
                fg: vec![theme::Color::Cyan],
                ..Theme::default()
            }
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_built_in_theme_resolves_without_any_file() {
        // An empty theme file contributes nothing, so the built-ins stand:
        // testing them through a file keeps the test off the developer's
        // real default location.
        let path = theme_file("empty", "");
        let file = path.to_str().expect("a temp path is UTF-8");

        let cli = cli_with(&["--theme-file", file, "--theme", "matrix", "hi"]);
        let theme = resolve_theme(&cli, false).expect("matrix is built in");
        assert_eq!(
            theme,
            Theme {
                fg: vec![theme::Color::BrightGreen],
                bold: true,
                ..Theme::default()
            }
        );

        let cli = cli_with(&["--theme-file", file, "--theme", "rainbow", "hi"]);
        let theme = resolve_theme(&cli, false).expect("rainbow is built in");
        assert_eq!(theme.fg.len(), 6, "a six-colour palette: {theme:?}");

        let cli = cli_with(&["--theme-file", file, "hi"]);
        assert_eq!(
            resolve_theme(&cli, false).expect("default is the plain theme"),
            Theme::default()
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn an_unknown_theme_is_a_usage_error_listing_the_registry() {
        let path = theme_file("listing", "[mine]\nfg = red\n");
        let cli = cli_with(&[
            "--theme-file",
            path.to_str().expect("a temp path is UTF-8"),
            "--theme",
            "nope",
            "hi",
        ]);
        let err = resolve_theme(&cli, false).expect_err("nope is nowhere");
        let message = err.to_string();
        assert!(message.contains("nope"), "{message}");
        assert!(message.contains("mine"), "{message}");
        assert!(message.contains("matrix"), "{message}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_bad_theme_file_is_reported_with_its_line() {
        let path = theme_file("broken", "[a]\nwhat = 1\n");
        let cli = cli_with(&[
            "--theme-file",
            path.to_str().expect("a temp path is UTF-8"),
            "hi",
        ]);
        let err = resolve_theme(&cli, false).expect_err("the file is malformed");
        let message = err.to_string();
        assert!(message.contains("line 2"), "{message}");
        assert!(message.contains("unknown key"), "{message}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_explicit_theme_file_is_reported_not_silently_skipped() {
        let missing = std::env::temp_dir().join("marquee-theme-absent.toml");
        let _ = std::fs::remove_file(&missing);
        let cli = cli_with(&[
            "--theme-file",
            missing.to_str().expect("a temp path is UTF-8"),
            "hi",
        ]);
        let err = resolve_theme(&cli, false).expect_err("the file must exist");
        assert!(matches!(err, ThemeError::Read { .. }), "{err}");
        assert!(err.to_string().contains("cannot read"), "{err}");
    }

    #[test]
    fn no_color_forces_the_plain_theme_but_validates_the_name() {
        // A typo is still reported even when colour is off: fail fast on
        // the flag the user actually typed.
        let cli = cli_with(&["--no-color", "--theme", "nope", "hi"]);
        assert!(resolve_theme(&cli, false).is_err(), "the name is validated");

        let cli = cli_with(&["--no-color", "--theme", "matrix", "hi"]);
        assert_eq!(
            resolve_theme(&cli, false).expect("resolves, then discards"),
            Theme::default()
        );

        // NO_COLOR from the environment does the same to --theme.
        let cli = cli_with(&["--theme", "alarm", "hi"]);
        assert_eq!(
            resolve_theme(&cli, true).expect("resolves, then discards"),
            Theme::default()
        );
    }

    #[test]
    fn a_themed_run_paints_every_frame_and_ends_uncoloured() {
        // End to end through the real loop: the frames carry the theme's
        // SGR, the cleanup does not, and the run still ends clean.
        let path = theme_file("e2e", "[testy]\nfg = red\nbold = true\n");
        let args = [
            "--speed",
            "1",
            "--once",
            "--gap",
            "0",
            "--theme-file",
            path.to_str().expect("a temp path is UTF-8"),
            "--theme",
            "testy",
            "你好",
        ];
        let cli = cli_with(&args);
        let theme = resolve_theme(&cli, false).expect("testy resolves");
        let mut terminal = FakeTerminal::new(10, 3);

        scroll(
            prepare(cli.text().expect("a TEXT is given")),
            motion_for(&cli),
            cli.frame_interval(),
            theme,
            &mut terminal,
        )
        .expect("a fake never fails");

        let frames = terminal.frames();
        let drawn = &frames[..frames.len() - 1];
        assert!(!drawn.is_empty());
        for frame in drawn {
            assert!(frame.contains("\x1b[38;5;1m"), "fg missing: {frame:?}");
            assert!(frame.contains("\x1b[1m"), "bold missing: {frame:?}");
            assert!(frame.ends_with("\x1b[0m"), "reset missing: {frame:?}");
        }
        assert_eq!(frames.last(), Some(&CLEAN_LINE.to_string()));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_palette_run_bands_the_frames_with_the_user_theme() {
        // A theme file with a colour list: the run resolves it and every
        // frame is painted band by band, with one reset closing it.
        let path = theme_file(
            "palette",
            "[flow]\nfg = [\"red\", \"#ffd194\"]\nband = 2\nbold = true\n",
        );
        let args = [
            "--speed",
            "1",
            "--once",
            "--gap",
            "0",
            "--theme-file",
            path.to_str().expect("a temp path is UTF-8"),
            "--theme",
            "flow",
            "你好",
        ];
        let cli = cli_with(&args);
        let theme = resolve_theme(&cli, false).expect("flow resolves");
        assert_eq!(theme.fg.len(), 2);
        assert_eq!(theme.band, 2);
        let mut terminal = FakeTerminal::new(10, 3);

        scroll(
            prepare(cli.text().expect("a TEXT is given")),
            motion_for(&cli),
            cli.frame_interval(),
            theme,
            &mut terminal,
        )
        .expect("a fake never fails");

        let frames = terminal.frames();
        let drawn = &frames[..frames.len() - 1];
        assert!(!drawn.is_empty());
        for frame in drawn {
            assert!(
                frame.contains("\x1b[38;5;1m"),
                "the classic red band: {frame:?}"
            );
            assert!(
                frame.contains("\x1b[38;2;255;209;148m"),
                "the #ffd194 band: {frame:?}"
            );
            assert!(frame.contains("\x1b[1m"), "bold: {frame:?}");
            assert!(frame.ends_with("\x1b[0m"), "reset missing: {frame:?}");
        }
        assert_eq!(frames.last(), Some(&CLEAN_LINE.to_string()));
        let _ = std::fs::remove_file(&path);
    }

    // ---- the big-font run loop (T-12) ----

    use crate::bigfont::BigFontFace;

    /// Runs the real big loop with `args` against a fake terminal. Like
    /// [`scroll_with`], always with the plain theme.
    fn scroll_big_with(args: &[&str], terminal: &mut dyn Terminal) -> io::Result<()> {
        let cli = Cli::try_parse_from(std::iter::once("marquee").chain(args.iter().copied()))
            .expect("the command line should parse");
        let text = cli.text().expect("a test always passes a TEXT");
        let face = BigFontFace::embedded_zh_hans().expect("the committed asset parses");
        scroll_big(
            prepare(text),
            motion_for(&cli),
            cli.frame_interval(),
            Theme::default(),
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
    fn big_mode_names_the_terminal_it_needs_and_refuses_smaller_ones() {
        // §4 via min_term_size: 12·scale columns (one whole glyph) and
        // 6·scale+1 rows (glyph rows plus a line of margin). The exact
        // minimum is legal.
        for (scale, columns, rows) in [(1, 12, 7), (2, 24, 13), (4, 48, 25), (32, 384, 193)] {
            assert!(
                big_size_error(scale, columns, rows).is_none(),
                "scale {scale} on {columns}×{rows} is the exact minimum"
            );
        }
        for (scale, columns, rows) in [
            (1, 11, 24), // narrower than one glyph
            (1, 80, 6),  // shorter than the glyph rows plus margin
            (2, 24, 12), // one row short of the minimum
            (4, 12, 20), // both at once — the old behaviour clamped this to 3
        ] {
            let message = big_size_error(scale, columns, rows)
                .unwrap_or_else(|| panic!("scale {scale} on {columns}×{rows} must be refused"));
            let (min_columns, min_rows) = min_term_size(scale);
            assert!(
                message.contains(&format!("{min_columns} columns × {min_rows} rows")),
                "the minimum is named: {message}"
            );
            assert!(
                message.contains(&format!("{columns}×{rows}")),
                "what we got is named: {message}"
            );
            assert!(
                message.contains(&format!("--scale {scale}")),
                "the request is named: {message}"
            );
        }
    }

    #[test]
    fn a_shrunk_window_gets_one_notice_naming_the_size_that_restores_it() {
        let message = too_small_notice(2, 1);
        assert!(
            message.contains("became too small for --big --scale 2"),
            "{message}"
        );
        assert!(
            message.contains("needs at least 24 columns × 13 rows"),
            "{message}"
        );
        assert!(
            message.contains("scrolling at --scale 1 until it fits again"),
            "{message}"
        );
    }

    #[test]
    fn a_big_run_survives_a_too_small_window_and_restores_the_scale() {
        // Scale 2 needs 24×13; the window dips to 8×8 (both limits broken)
        // and comes back: the scroll never stops, the clamped scale 1 stands
        // in while small, and scale 2 is restored when the window fits again.
        let args = [
            "--big", "--repeat", "1", "--gap", "0", "--speed", "1", "--scale", "2", "中",
        ];
        let mut terminal = FakeTerminal::new(60, 40);
        terminal.push_resize_at(4, 8, 8);
        terminal.push_resize_at(10, 60, 40);

        scroll_big_with(&args, &mut terminal).expect("a too-small window never kills the scroll");

        let frames = terminal.frames();
        assert!(
            frames.len() > 12,
            "the scroll should have run on: {frames:?}"
        );
        for frame in &frames[..3] {
            assert_eq!(frame.matches("\x1b[2K").count(), 12, "scale 2 is 12 rows");
        }
        for frame in &frames[3..9] {
            assert_eq!(
                frame.matches("\x1b[2K").count(),
                6,
                "the clamped scale 1 stands in: {frame:?}"
            );
        }
        for frame in &frames[9..frames.len() - 1] {
            assert_eq!(
                frame.matches("\x1b[2K").count(),
                12,
                "scale 2 restored when the window grew back: {frame:?}"
            );
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
