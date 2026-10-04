//! Command-line input handling.
//!
//! Two jobs, both terminal-free so they can be unit-tested:
//!
//! 1. [`Cli`] — parse and validate the flags with clap.
//! 2. [`resolve_text`] — decide where the text to scroll comes from.
//!
//! # Input precedence (the rule the README must state — T-8)
//!
//! Exactly one rule, in this order:
//!
//! 1. **A positional `TEXT` argument always wins.** `marquee hi < notes.txt`
//!    scrolls `hi` and never reads stdin, so the tool cannot block on a pipe
//!    nobody meant to feed it.
//! 2. **With no `TEXT`, piped or redirected stdin is read to EOF.**
//!    `echo "正在部署..." | marquee` scrolls the piped text. Piped input is
//!    flattened to a single line: each line is trimmed, blank lines are
//!    dropped and the rest are joined with one space.
//! 3. **With no `TEXT` and a terminal on stdin, marquee fails with a usage
//!    error (exit 2)** instead of waiting forever for input that will never
//!    come.
//!
//! Text given as `TEXT` is scrolled exactly as written; text that resolves to
//! nothing but whitespace is refused in either case — there would be no
//! visible marquee to scroll.

use std::fmt;
use std::io::{self, BufRead};
use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, ValueEnum};

/// Pacing when neither `--speed` nor `--fps` is given: one column step every
/// 50 ms, i.e. 20 steps per second.
const DEFAULT_SPEED_MS: u64 = 50;

/// Blank columns between the end of one cycle and the start of the next.
const DEFAULT_GAP: u32 = 8;

/// Scroll direction: which edge the text enters from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Direction {
    /// Text enters at the right edge and leaves at the left (the classic
    /// marquee).
    Left,
    /// Text enters at the left edge and leaves at the right.
    Right,
}

/// Where text shorter than the terminal sits while it is on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// Which packaged pixel-font atlas `--big` renders with. One variant today;
/// `ja`/`zh-hant` are planned once their atlases are generated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum Font {
    /// Simplified Chinese, ASCII and a tofu fallback (the packaged atlas)
    #[value(name = "zh-hans")]
    ZhHans,
}

/// The parsed command line.
///
/// Accessors are the contract for the engine (T-5), the renderer (T-6) and
/// the run loop (T-7).
// The consumers arrive with T-5..T-7; until then most accessors have no
// caller in the binary.
#[allow(dead_code)]
#[derive(Debug, Parser)]
#[command(
    name = "marquee",
    version,
    about = "Scroll text across the terminal, measured in display columns",
    long_about = None
)]
pub struct Cli {
    /// Text to scroll. If omitted, marquee reads piped stdin instead.
    #[arg(value_name = "TEXT")]
    text: Option<String>,

    /// Milliseconds per column step (1-10000)
    #[arg(
        long,
        value_name = "MS",
        value_parser = clap::value_parser!(u64).range(1..=10_000),
        conflicts_with = "fps"
    )]
    speed: Option<u64>,

    /// Frames per second (1-1000); cannot be combined with --speed
    #[arg(
        long,
        value_name = "FPS",
        value_parser = clap::value_parser!(u32).range(1..=1_000),
        conflicts_with = "speed"
    )]
    fps: Option<u32>,

    /// Scroll direction
    #[arg(long, value_enum, default_value_t = Direction::Left)]
    direction: Direction,

    /// Reverse at both edges instead of wrapping around
    #[arg(long)]
    bounce: bool,

    /// Scroll exactly N cycles, then exit
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..), conflicts_with = "once")]
    repeat: Option<u32>,

    /// Blank columns between cycles
    #[arg(long, value_name = "COLUMNS", default_value_t = DEFAULT_GAP)]
    gap: u32,

    /// Placement while text shorter than the terminal is on screen
    #[arg(long, value_enum, default_value_t = Align::Left)]
    align: Align,

    /// Never emit colour escape sequences
    #[arg(long)]
    no_color: bool,

    /// Start the next copy of the text before the previous one has left
    /// the screen, `--gap` columns behind it; cannot be combined with
    /// --bounce
    #[arg(long, conflicts_with = "bounce")]
    continuous: bool,

    /// Colour theme for the text: a built-in name, or one defined in the
    /// theme file
    #[arg(long, value_name = "NAME", default_value = "default")]
    theme: String,

    /// Read user themes from this file instead of the default location
    /// ($XDG_CONFIG_HOME/marquee/themes.toml)
    #[arg(long, value_name = "PATH")]
    theme_file: Option<PathBuf>,

    /// Scroll exactly one cycle, then exit (same as --repeat 1)
    #[arg(long, conflicts_with = "repeat")]
    once: bool,

    /// Scroll the text as big pixel glyphs spanning several terminal rows
    /// (fusion-pixel font, half-block cells)
    #[arg(long)]
    big: bool,

    /// Pixel magnification for --big: 1-32; the terminal must be at least
    /// 12·N columns and 6·N+1 rows, or the run is refused
    #[arg(
        long,
        value_name = "N",
        value_parser = clap::value_parser!(u32).range(1..=32),
        requires = "big",
        default_value_t = 1
    )]
    scale: u32,

    /// Which packaged pixel font --big renders with
    #[arg(long, value_enum, requires = "big", default_value_t = Font::ZhHans)]
    font: Font,
}

// Accessors consumed by T-5 (engine), T-6 (renderer) and T-7 (run loop).
#[allow(dead_code)]
impl Cli {
    /// The positional `TEXT`, if one was given.
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }

    /// How long one frame lasts: `--speed` in milliseconds, `--fps`
    /// converted to a period, or [`DEFAULT_SPEED_MS`] when neither is given.
    /// The parse step already rejected `--speed` together with `--fps`.
    pub fn frame_interval(&self) -> Duration {
        if let Some(ms) = self.speed {
            Duration::from_millis(ms)
        } else if let Some(fps) = self.fps {
            // Round to the nearest nanosecond period; fps >= 1, so no divide
            // by zero and no zero-length frame.
            let nanos = (1_000_000_000 + u64::from(fps) / 2) / u64::from(fps);
            Duration::from_nanos(nanos)
        } else {
            Duration::from_millis(DEFAULT_SPEED_MS)
        }
    }

    /// Which edge the text enters from.
    pub fn direction(&self) -> Direction {
        self.direction
    }

    /// Whether the text reverses at the edges instead of wrapping.
    pub fn bounce(&self) -> bool {
        self.bounce
    }

    /// Blank columns left between two cycles.
    pub fn gap(&self) -> u32 {
        self.gap
    }

    /// Placement of text shorter than the terminal.
    pub fn align(&self) -> Align {
        self.align
    }

    /// How many cycles to scroll: `Some(1)` for `--once`, `Some(n)` for
    /// `--repeat n`, `None` to loop forever.
    pub fn cycles(&self) -> Option<u32> {
        if self.once { Some(1) } else { self.repeat }
    }

    /// Whether colour escape sequences may be emitted. The run loop (T-7)
    /// additionally turns colour off when stdout is not a terminal.
    pub fn color_enabled(&self) -> bool {
        !self.no_color
    }

    /// Whether the next copy of the text may follow the previous one onto
    /// the screen instead of waiting for it to leave.
    pub fn continuous(&self) -> bool {
        self.continuous
    }

    /// The `--theme` name: built-in or from the theme file.
    pub fn theme(&self) -> &str {
        &self.theme
    }

    /// The `--theme-file` path, when one was given.
    pub fn theme_file(&self) -> Option<&std::path::Path> {
        self.theme_file.as_deref()
    }

    /// Whether the text scrolls as big pixel glyphs over multiple rows.
    pub fn big(&self) -> bool {
        self.big
    }

    /// The `--scale` pixel magnification for big mode.
    pub fn scale(&self) -> usize {
        self.scale as usize
    }

    /// Which packaged pixel font big mode renders with.
    pub fn font(&self) -> Font {
        self.font
    }
}

/// Why the text to scroll could not be determined.
#[derive(Debug)]
pub enum InputError {
    /// No `TEXT` argument and stdin is a terminal: nothing to scroll.
    Missing,
    /// Input was found but held nothing printable.
    Blank,
    /// Reading stdin failed.
    Io(io::Error),
}

impl fmt::Display for InputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InputError::Missing => write!(
                f,
                "no text to scroll: pass it as an argument (marquee \"你好\") or pipe it in (echo \"你好\" | marquee)"
            ),
            InputError::Blank => write!(f, "no text to scroll: the input is empty"),
            InputError::Io(err) => write!(f, "cannot read stdin: {err}"),
        }
    }
}

impl std::error::Error for InputError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            InputError::Io(err) => Some(err),
            InputError::Missing | InputError::Blank => None,
        }
    }
}

impl From<io::Error> for InputError {
    fn from(err: io::Error) -> Self {
        InputError::Io(err)
    }
}

/// Resolves the text to scroll, following the precedence rule documented at
/// the top of this module.
///
/// `stdin_is_terminal` and `stdin` are parameters rather than global state so
/// the rule can be tested without a terminal; the run loop passes
/// `io::stdin().is_terminal()` and a locked stdin.
pub fn resolve_text(
    cli: &Cli,
    stdin_is_terminal: bool,
    mut stdin: impl BufRead,
) -> Result<String, InputError> {
    // Rule 1: an argument wins, and stdin is left untouched.
    if let Some(text) = cli.text() {
        return non_blank(text.to_string());
    }
    // Rule 3: never block on a terminal waiting for input.
    if stdin_is_terminal {
        return Err(InputError::Missing);
    }
    // Rule 2: read the pipe to EOF and flatten it to one line.
    let mut piped = String::new();
    stdin.read_to_string(&mut piped)?;
    non_blank(flatten(&piped))
}

/// Refuses input that would scroll nothing visible.
fn non_blank(text: String) -> Result<String, InputError> {
    if text.trim().is_empty() {
        Err(InputError::Blank)
    } else {
        Ok(text)
    }
}

/// Flattens piped input to the single line a marquee scrolls: trim each line,
/// drop blank ones, join the rest with one space.
fn flatten(piped: &str) -> String {
    piped
        .split(['\n', '\r'])
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;
    use std::io::{Cursor, Read};

    /// Parse a command line, or return clap's error.
    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("marquee").chain(args.iter().copied()))
    }

    fn parsed(args: &[&str]) -> Cli {
        parse(args).unwrap_or_else(|err| panic!("{args:?} should parse: {err}"))
    }

    /// A stdin that fails the test if anything reads it.
    struct UnreadableStdin;

    impl Read for UnreadableStdin {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            panic!("stdin must not be read when TEXT is given");
        }
    }

    impl BufRead for UnreadableStdin {
        fn fill_buf(&mut self) -> io::Result<&[u8]> {
            panic!("stdin must not be read when TEXT is given");
        }

        fn consume(&mut self, _amt: usize) {}
    }

    #[test]
    fn every_documented_flag_parses() {
        let cli = parsed(&[
            "hello 世界",
            "--big",
            "--scale",
            "3",
            "--font",
            "zh-hans",
            "--speed",
            "120",
            "--direction",
            "right",
            "--gap",
            "3",
            "--align",
            "center",
            "--no-color",
            "--repeat",
            "4",
            "--continuous",
            "--theme",
            "matrix",
            "--theme-file",
            "/tmp/themes.toml",
        ]);
        assert_eq!(cli.text(), Some("hello 世界"));
        assert!(cli.big());
        assert_eq!(cli.scale(), 3);
        assert_eq!(cli.font(), Font::ZhHans);
        assert_eq!(cli.direction(), Direction::Right);
        assert!(!cli.bounce(), "bounce conflicts with continuous");
        assert_eq!(cli.gap(), 3);
        assert_eq!(cli.align(), Align::Center);
        assert!(!cli.color_enabled());
        assert_eq!(cli.cycles(), Some(4));
        assert_eq!(cli.frame_interval(), Duration::from_millis(120));
        assert!(cli.continuous());
        assert_eq!(cli.theme(), "matrix");
        assert_eq!(
            cli.theme_file(),
            Some(std::path::Path::new("/tmp/themes.toml"))
        );
    }

    #[test]
    fn defaults_are_a_left_scrolling_forever_marquee() {
        let cli = parsed(&["hi"]);
        assert_eq!(cli.direction(), Direction::Left);
        assert_eq!(cli.align(), Align::Left);
        assert!(!cli.bounce());
        assert_eq!(cli.gap(), DEFAULT_GAP);
        assert_eq!(cli.cycles(), None, "no --once/--repeat loops forever");
        assert_eq!(
            cli.frame_interval(),
            Duration::from_millis(DEFAULT_SPEED_MS)
        );
        assert!(cli.color_enabled());
        assert!(!cli.big(), "big mode is opt-in");
        assert_eq!(cli.scale(), 1, "the default magnification is 1");
        assert_eq!(
            cli.font(),
            Font::ZhHans,
            "the packaged atlas is the default"
        );
        assert!(!cli.continuous(), "continuous is opt-in");
        assert_eq!(cli.theme(), "default", "the plain theme is the default");
        assert_eq!(cli.theme_file(), None, "no explicit theme file");
    }

    #[test]
    fn continuous_and_bounce_are_contradictions() {
        // A bounce reverses at the edges; continuous follows itself around
        // them. Both at once is a usage error naming both.
        let err = parse(&["--continuous", "--bounce", "hi"])
            .expect_err("--continuous and --bounce are mutually exclusive");
        assert_eq!(err.kind(), ErrorKind::ArgumentConflict);
        let message = err.render().to_string();
        assert!(message.contains("--continuous"), "{message}");
        assert!(message.contains("--bounce"), "{message}");
        assert_eq!(
            message.matches("cannot be used with").count(),
            1,
            "one error, not one per option: {message}"
        );

        // Either one alone is fine.
        assert!(parsed(&["--continuous", "hi"]).continuous());
        assert!(parsed(&["--bounce", "hi"]).bounce());
    }

    #[test]
    fn a_theme_name_is_free_form_until_the_registry_judges_it() {
        // clap cannot know the theme names (users define their own), so
        // anything parses; resolution and the error listing come later.
        assert_eq!(parsed(&["--theme", "whatever", "hi"]).theme(), "whatever");
        assert_eq!(parsed(&["--theme", "matrix", "hi"]).theme(), "matrix");
    }

    #[test]
    fn big_mode_flags_only_make_sense_with_big() {
        for flag in ["--scale 2", "--font zh-hans"] {
            let args: Vec<&str> = flag.split(' ').chain(["hi"]).collect();
            let err = parse(&args).expect_err(&format!("{flag} needs --big"));
            assert_eq!(
                err.kind(),
                ErrorKind::MissingRequiredArgument,
                "{flag} without --big"
            );
            let message = err.render().to_string();
            assert!(message.contains("--big"), "{message}");
        }
        // ...and with it, they parse.
        assert_eq!(parsed(&["--big", "--scale", "2", "hi"]).scale(), 2);
    }

    #[test]
    fn scale_is_validated_and_the_font_list_is_what_is_packaged() {
        for bad in ["0", "33"] {
            let err =
                parse(&["--big", "--scale", bad, "hi"]).expect_err("--scale must be in range");
            assert_eq!(err.kind(), ErrorKind::ValueValidation, "--scale {bad}");
        }
        // Only the packaged font parses; the planned variants do not, and
        // clap's message says what is available.
        let err = parse(&["--big", "--font", "ja", "hi"]).expect_err("no ja atlas is packaged");
        assert_eq!(err.kind(), ErrorKind::InvalidValue);
        let message = err.render().to_string();
        assert!(message.contains("ja"), "{message}");
        assert!(message.contains("zh-hans"), "{message}");
    }

    #[test]
    fn direction_rejects_unknown_values_with_a_clear_error() {
        let err =
            parse(&["--direction", "sideways", "hi"]).expect_err("sideways is not a direction");
        assert_eq!(err.kind(), ErrorKind::InvalidValue);
        let message = err.render().to_string();
        assert!(message.contains("--direction"), "{message}");
        assert!(message.contains("left"), "{message}");
        assert!(message.contains("right"), "{message}");
        assert!(message.contains("sideways"), "{message}");

        assert_eq!(
            parsed(&["--direction", "left", "hi"]).direction(),
            Direction::Left
        );
        assert_eq!(
            parsed(&["--direction", "right", "hi"]).direction(),
            Direction::Right
        );
    }

    #[test]
    fn speed_and_fps_fail_with_one_error_naming_both() {
        let err = parse(&["--speed", "80", "--fps", "25", "hi"])
            .expect_err("--speed and --fps are mutually exclusive");
        assert_eq!(err.kind(), ErrorKind::ArgumentConflict);
        let message = err.render().to_string();
        assert!(message.contains("--speed"), "{message}");
        assert!(message.contains("--fps"), "{message}");
        assert_eq!(
            message.matches("cannot be used with").count(),
            1,
            "one error, not one per option: {message}"
        );
    }

    #[test]
    fn pacing_comes_from_speed_or_fps() {
        assert_eq!(
            parsed(&["--speed", "200", "hi"]).frame_interval(),
            Duration::from_millis(200)
        );
        assert_eq!(
            parsed(&["--fps", "25", "hi"]).frame_interval(),
            Duration::from_millis(40)
        );
        assert_eq!(
            parsed(&["--fps", "3", "hi"]).frame_interval(),
            Duration::from_nanos(333_333_333)
        );
        // Out-of-range pacing is refused rather than clamped.
        assert!(parse(&["--speed", "0", "hi"]).is_err());
        assert!(parse(&["--fps", "0", "hi"]).is_err());
    }

    #[test]
    fn once_and_repeat_control_the_cycle_count() {
        assert_eq!(parsed(&["--once", "hi"]).cycles(), Some(1));
        assert_eq!(parsed(&["--repeat", "3", "hi"]).cycles(), Some(3));
        assert_eq!(parsed(&["hi"]).cycles(), None);

        // Asking for both is a contradiction; zero cycles would draw nothing.
        assert_eq!(
            parse(&["--once", "--repeat", "3", "hi"])
                .expect_err("--once and --repeat are mutually exclusive")
                .kind(),
            ErrorKind::ArgumentConflict
        );
        let zero = parse(&["--repeat", "0", "hi"]).expect_err("0 cycles is not a marquee");
        assert_eq!(zero.kind(), ErrorKind::ValueValidation);
        assert!(zero.render().to_string().contains("--repeat"));
    }

    #[test]
    fn help_and_version_are_available_in_both_forms() {
        for flag in ["-h", "--help"] {
            // clap reports help through its error path, with an exit code of 0.
            let err = parse(&[flag]).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::DisplayHelp);
            let help = err.render().to_string();
            for documented in [
                "TEXT",
                "--speed",
                "--fps",
                "--direction",
                "--bounce",
                "--repeat",
                "--gap",
                "--align",
                "--no-color",
                "--once",
                "--continuous",
                "--theme",
                "--theme-file",
                "--big",
                "--scale",
                "--font",
            ] {
                assert!(
                    help.contains(documented),
                    "{flag} omits {documented}:\n{help}"
                );
            }
        }
        for flag in ["-V", "--version"] {
            let err = parse(&[flag]).unwrap_err();
            assert_eq!(err.kind(), ErrorKind::DisplayVersion);
            assert!(err.render().to_string().contains(env!("CARGO_PKG_VERSION")));
        }
    }

    #[test]
    fn an_argument_wins_and_stdin_is_never_read() {
        let cli = parsed(&["from-argument"]);
        let text = resolve_text(&cli, false, UnreadableStdin).expect("TEXT resolves without stdin");
        assert_eq!(text, "from-argument");

        // Even a piped stdin is ignored, and the argument is kept verbatim.
        let cli = parsed(&["  spaced  "]);
        let piped = Cursor::new("from-pipe\n".as_bytes());
        assert_eq!(
            resolve_text(&cli, false, piped).expect("TEXT wins"),
            "  spaced  "
        );
    }

    #[test]
    fn piped_stdin_is_read_and_flattened_to_one_line() {
        let cli = parsed(&[]);
        let piped = Cursor::new("正在部署...\n".as_bytes());
        assert_eq!(
            resolve_text(&cli, false, piped).expect("piped input resolves"),
            "正在部署..."
        );

        let cli = parsed(&[]);
        let piped = Cursor::new("  first \r\n\nsecond\tline\n".as_bytes());
        assert_eq!(
            resolve_text(&cli, false, piped).expect("multi-line input resolves"),
            "first second\tline"
        );
    }

    #[test]
    fn a_terminal_on_stdin_without_an_argument_is_a_usage_error() {
        let cli = parsed(&[]);
        let err = resolve_text(&cli, true, Cursor::new("never read".as_bytes()))
            .expect_err("marquee must not block on an interactive stdin");
        assert!(matches!(err, InputError::Missing));
        let message = err.to_string();
        assert!(message.contains("marquee"), "{message}");
        assert!(
            message.contains("| marquee"),
            "shows the pipe form: {message}"
        );
    }

    #[test]
    fn blank_input_is_refused() {
        let cli = parsed(&["   "]);
        assert!(matches!(
            resolve_text(&cli, true, UnreadableStdin),
            Err(InputError::Blank)
        ));

        let cli = parsed(&[]);
        assert!(matches!(
            resolve_text(&cli, false, Cursor::new("\n  \n".as_bytes())),
            Err(InputError::Blank)
        ));
    }

    #[test]
    fn a_failing_stdin_is_reported_not_panicked() {
        struct Broken;

        impl Read for Broken {
            fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::UnexpectedEof, "pipe closed"))
            }
        }

        impl BufRead for Broken {
            fn fill_buf(&mut self) -> io::Result<&[u8]> {
                Err(io::Error::new(io::ErrorKind::UnexpectedEof, "pipe closed"))
            }

            fn consume(&mut self, _amt: usize) {}
        }

        let cli = parsed(&[]);
        let err = resolve_text(&cli, false, Broken).expect_err("a broken pipe is an error");
        assert!(matches!(err, InputError::Io(_)));
        assert!(err.to_string().contains("cannot read stdin"), "{err}");
    }
}
