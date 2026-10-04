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

use std::io::IsTerminal;
use std::process::ExitCode;

use clap::Parser;

/// Exit code for a usage error: bad flags (clap's own code) or no text to
/// scroll.
const EXIT_USAGE: u8 = 2;

/// Exit code for an I/O failure while drawing.
const EXIT_IO: u8 = 1;

fn main() -> ExitCode {
    let cli = cli::Cli::parse();

    let text = match cli::resolve_text(
        &cli,
        std::io::stdin().is_terminal(),
        std::io::stdin().lock(),
    ) {
        Ok(text) => text,
        Err(err) => {
            eprintln!("marquee: {err}");
            return ExitCode::from(EXIT_USAGE);
        }
    };

    let motion = marquee::Motion {
        direction: match cli.direction() {
            cli::Direction::Left => marquee::Direction::Left,
            cli::Direction::Right => marquee::Direction::Right,
        },
        bounce: cli.bounce(),
        gap: cli.gap() as usize,
        cycles: cli.cycles(),
    };

    // Stub: one static frame, after which dropping the terminal restores
    // cursor and mode. Frame 0 of a scroll is blank by design — the text sits
    // just off the edge it enters from — so this prints an empty line until
    // T-7 replaces it with the paced loop that honours the rest of `cli`
    // (frame interval, colour) and reacts to the resize and quit events the
    // terminal reports.
    let mut terminal = terminal::open();
    let engine = marquee::Engine::new(unicode::prepare(&text), terminal.width(), motion);
    if let Err(err) = renderer::render_frame(&mut terminal, &engine).and_then(|()| terminal.flush())
    {
        eprintln!("marquee: {err}");
        return ExitCode::from(EXIT_IO);
    }

    ExitCode::SUCCESS
}
