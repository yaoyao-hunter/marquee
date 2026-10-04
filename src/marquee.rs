//! Core scroll algorithm, decoupled from terminal I/O.
//!
//! Stub: holds the prepared text and terminal width. Task T-5 implements
//! column-based left/right/bounce motion, gap, repeat/once and resize-safe
//! position handling on top of the precomputed cells from `crate::unicode`.

use crate::unicode::PreparedText;

/// The scroll state machine: pure computation, no terminal access.
pub struct Engine {
    prepared: PreparedText,
    width: usize,
}

impl Engine {
    pub fn new(prepared: PreparedText, width: usize) -> Self {
        Self { prepared, width }
    }

    /// The text of the current frame. Stub: always the full text.
    pub fn frame_text(&self) -> &str {
        self.prepared.text()
    }

    /// The terminal width in columns the engine renders into.
    pub fn width(&self) -> usize {
        self.width
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::unicode::prepare;

    #[test]
    fn engine_exposes_text_and_width() {
        let engine = Engine::new(prepare("hi"), 42);
        assert_eq!(engine.frame_text(), "hi");
        assert_eq!(engine.width(), 42);
    }
}
