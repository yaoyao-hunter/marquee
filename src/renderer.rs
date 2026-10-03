//! Frame drawing.
//!
//! Stub: prints one static frame with `println!`. Task T-6 replaces this
//! with single-line rendering via cursor positioning and line clearing
//! (crossterm), one flush per frame and minimal per-frame allocation.

use crate::marquee::Engine;

/// Draws the current frame. Stub: one plain line to stdout, capped to the
/// engine's width (naively by chars; T-5/T-6 make this column-exact).
pub fn render_frame(engine: &Engine) {
    let frame: String = engine.frame_text().chars().take(engine.width()).collect();
    println!("{frame}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::unicode::prepare;

    #[test]
    fn render_frame_does_not_panic() {
        render_frame(&Engine::new(prepare("你好"), 80));
    }
}
