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

    // Stub: one frame, then a clean line. Frame 0 of a scroll is blank by
    // design (the text sits just off the edge it enters from), so this shows
    // nothing until T-7 replaces it with the paced loop: poll the terminal for
    // `renderer.delay_until_next_frame()`, handle resize and quit, draw,
    // advance, and stop when `engine.is_finished()`.
    let mut terminal = terminal::open();
    let engine = marquee::Engine::new(unicode::prepare(&text), terminal.width(), motion);
    let mut renderer = renderer::Renderer::new(cli.frame_interval());

    let drawn = renderer
        .draw(&mut *terminal, &engine)
        .and_then(|()| renderer.finish(&mut *terminal));
    if let Err(err) = drawn {
        eprintln!("marquee: {err}");
        return ExitCode::from(EXIT_IO);
    }

    ExitCode::SUCCESS
}
