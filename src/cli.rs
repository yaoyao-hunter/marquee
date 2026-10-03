//! Command-line input handling.
//!
//! Stub: joins the command-line arguments into the text to scroll.
//! Task T-3 replaces this with clap parsing (`--speed`/`--fps` conflict
//! errors, `--direction`, `--bounce`, ...) and the documented
//! stdin-versus-positional precedence rule.
pub fn input_text() -> String {
    std::env::args().skip(1).collect::<Vec<_>>().join(" ")
}
