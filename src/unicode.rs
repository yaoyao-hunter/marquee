//! Unicode measurement: grapheme clusters and terminal display widths.
//!
//! Stub: stores the raw text unchanged. Task T-2 segments the text into
//! grapheme clusters (unicode-segmentation) and precomputes each cluster's
//! display width in terminal columns (unicode-width) exactly once.

/// Text prepared for scrolling: measured once at startup, read-only on the
/// render path.
pub struct PreparedText {
    pub text: String,
}

/// Prepares `text` for the scroll engine.
pub fn prepare(text: &str) -> PreparedText {
    PreparedText {
        text: text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_keeps_text_unchanged() {
        assert_eq!(prepare("你好 🚀").text, "你好 🚀");
    }
}
