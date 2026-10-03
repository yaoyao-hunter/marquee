//! Terminal state: width detection, resize events, restore-on-exit.
//!
//! Stub: a fixed fallback width. Task T-4 adds crossterm-based width
//! detection, live resize observation, cursor management without alternate
//! screen, and terminal restoration on Ctrl+C and panic.

/// Fallback used when the real width is unknown (piped stdout, CI).
const FALLBACK_WIDTH: usize = 80;

/// Current terminal width in columns.
pub fn width() -> usize {
    FALLBACK_WIDTH
}

#[cfg(test)]
mod tests {
    #[test]
    fn fallback_width_is_positive() {
        assert!(super::width() > 0);
    }
}
