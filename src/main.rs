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

    // Stub: one static frame. T-7 turns this into the paced scroll loop that
    // honours the rest of `cli` (direction, bounce, gap, align, cycles,
    // frame interval, colour) and adapts to resizes.
    let prepared = unicode::prepare(&text);
    let engine = marquee::Engine::new(prepared, terminal::width());
    renderer::render_frame(&engine);

    ExitCode::SUCCESS
}
