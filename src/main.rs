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

    // Stub: one static frame, after which dropping the terminal restores
    // cursor and mode. T-7 replaces this with the paced scroll loop that
    // honours the rest of `cli` (direction, bounce, gap, align, cycles, frame
    // interval, colour) and reacts to the resize and quit events the terminal
    // reports.
    let mut terminal = terminal::open();
    let engine = marquee::Engine::new(unicode::prepare(&text), terminal.width());
    if let Err(err) = renderer::render_frame(&mut terminal, &engine).and_then(|()| terminal.flush())
    {
        eprintln!("marquee: {err}");
        return ExitCode::from(EXIT_IO);
    }

    ExitCode::SUCCESS
}
