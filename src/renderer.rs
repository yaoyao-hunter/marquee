//! Frame drawing.
//!
//! Stub: writes one static line to any [`io::Write`]. Task T-6 replaces this
//! with single-line rendering via cursor positioning and line clearing
//! (crossterm), one flush per frame and minimal per-frame allocation.

use std::io::{self, Write};

use crate::marquee::Engine;

/// Draws the current frame to `out`. Stub: one plain line, capped to the
/// engine's width (naively by chars; T-5/T-6 make this column-exact).
pub fn render_frame(out: &mut impl Write, engine: &Engine) -> io::Result<()> {
    let frame: String = engine.frame_text().chars().take(engine.width()).collect();
    // Raw mode does not expand "\n" into a carriage return plus a line feed,
    // so the return is explicit — which is also correct for redirected output.
    writeln!(out, "{frame}\r")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::unicode::prepare;

    #[test]
    fn render_frame_writes_one_line() {
        let mut out = Vec::new();
        render_frame(&mut out, &Engine::new(prepare("你好"), 80)).expect("a Vec never fails");
        assert_eq!(out, "你好\r\n".as_bytes());
    }
}
