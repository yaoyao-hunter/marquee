//! Frame drawing.
//!
//! Stub: writes the engine's frame as one plain line to any [`io::Write`].
//! Task T-6 replaces this with single-line rendering via cursor positioning
//! and line clearing (crossterm), one reused buffer, one flush per frame and
//! pacing from `--speed`/`--fps`.

use std::io::{self, Write};

use crate::marquee::Engine;

/// Draws the current frame to `out`.
///
/// The frame is exactly the viewport width in display columns and never
/// splits a grapheme cluster: [`Engine::draw`] guarantees both.
pub fn render_frame(out: &mut impl Write, engine: &Engine) -> io::Result<()> {
    // T-6 keeps one buffer alive across frames instead of allocating here.
    let mut frame = String::new();
    engine.draw(&mut frame);
    // Raw mode does not expand "\n" into a carriage return plus a line feed,
    // so the return is explicit — which is also correct for redirected output.
    writeln!(out, "{frame}\r")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marquee::{Direction, Motion};
    use crate::unicode::prepare;

    #[test]
    fn render_frame_writes_the_frame_as_one_line() {
        let mut engine = Engine::new(
            prepare("你好"),
            4,
            Motion {
                direction: Direction::Left,
                bounce: false,
                gap: 0,
                cycles: None,
            },
        );
        // Step until the text has fully entered the four-column viewport.
        for _ in 0..4 {
            engine.advance();
        }

        let mut out = Vec::new();
        render_frame(&mut out, &engine).expect("a Vec never fails");
        assert_eq!(out, "你好\r\n".as_bytes());
    }
}
