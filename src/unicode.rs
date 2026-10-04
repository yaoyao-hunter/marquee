//! Unicode measurement: grapheme clusters and terminal display widths.
//!
//! A marquee scrolls by terminal *columns*, not by bytes, `char`s or
//! `String::len()`. [`prepare`] therefore does all the Unicode work exactly
//! once, at startup: it segments the input into grapheme clusters and stores
//! each cluster together with the number of columns a terminal will draw it
//! in. Everything downstream (the scroll engine, the renderer) reads those
//! precomputed [`Cell`]s and never segments or measures again.
//!
//! The widths follow what terminals actually do, via `unicode-width`:
//!
//! | input        | clusters | columns            |
//! |--------------|----------|--------------------|
//! | `hello`      | 5        | 5                  |
//! | `你好`       | 2        | 4 (2 each)         |
//! | `👨‍👩‍👧‍👦`      | 1        | 2 (ZWJ sequence)   |
//! | `🇯🇵`       | 1        | 2 (flag pair)      |
//! | `é` (combining) | 1     | 1                  |
//!
//! A cluster can also be zero columns wide (a stray combining mark, a
//! zero-width space); such cells are kept, with `width == 0`, so the text
//! that is scrolled out is byte-for-byte the text that was given.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// One grapheme cluster and the number of terminal columns it occupies.
///
/// This is the unit the scroll engine moves: a cell is never split, however
/// many codepoints or columns it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    text: String,
    width: usize,
}

impl Cell {
    /// The cluster's own text — one or more codepoints, always kept whole.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// How many terminal columns this cluster occupies; `0` for combining
    /// marks and other zero-width clusters.
    pub fn width(&self) -> usize {
        self.width
    }
}

/// Text prepared for scrolling: measured once at startup, read-only on the
/// render path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedText {
    text: String,
    cells: Vec<Cell>,
    width: usize,
}

impl PreparedText {
    /// The original text, unchanged.
    #[allow(dead_code)] // kept for diagnostics; nothing reads it yet
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The grapheme clusters in scroll order, each carrying its width.
    ///
    /// This borrows the precomputed cells; calling it does no Unicode work.
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    /// Total width of the text in terminal columns: the sum of the cell
    /// widths. Together with the viewport width this is what a scroll cycle
    /// is measured in (see `crate::marquee`).
    pub fn width(&self) -> usize {
        self.width
    }

    /// Whether there is nothing to scroll.
    #[allow(dead_code)] // consumed by the run loop (T-7)
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }
}

/// Segments `text` into grapheme clusters and measures each one in terminal
/// columns. This is the only place in marquee that touches Unicode layout.
pub fn prepare(text: &str) -> PreparedText {
    let cells: Vec<Cell> = text
        .graphemes(true)
        .map(|grapheme| Cell {
            text: grapheme.to_string(),
            width: grapheme.width(),
        })
        .collect();
    let width = cells.iter().map(Cell::width).sum();
    PreparedText {
        text: text.to_string(),
        cells,
        width,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Asserts that `text` segments into exactly these `(cluster, columns)`
    /// cells, in order.
    fn assert_cells(text: &str, expected: &[(&str, usize)]) {
        let prepared = prepare(text);
        let actual: Vec<(&str, usize)> = prepared
            .cells()
            .iter()
            .map(|cell| (cell.text(), cell.width()))
            .collect();
        assert_eq!(actual, expected.to_vec(), "clusters of {text:?}");
    }

    #[test]
    fn widths_match_the_corpus() {
        // "e" + U+0301: the combining form of é.
        let corpus = [
            ("hello", 5),
            ("你好", 4),
            ("Hello 世界", 10),
            ("👨‍👩‍👧‍👦", 2),
            ("e\u{301}", 1),
            ("🇯🇵", 2),
        ];
        for (text, expected) in corpus {
            let prepared = prepare(text);
            assert_eq!(prepared.width(), expected, "total columns of {text:?}");
            assert_eq!(
                prepared.cells().iter().map(Cell::width).sum::<usize>(),
                expected,
                "cell columns of {text:?} must sum to the total"
            );
        }
    }

    #[test]
    fn emoji_clusters_are_never_split_into_codepoints() {
        // A ZWJ family is seven codepoints, a flag two regional indicators:
        // each stays one cell of two columns.
        assert_cells("👨‍👩‍👧‍👦", &[("👨‍👩‍👧‍👦", 2)]);
        assert_cells("🇯🇵", &[("🇯🇵", 2)]);
        // Skin-tone modifiers and variation selectors stay inside their cell.
        assert_cells("👩🏽‍🚀", &[("👩🏽‍🚀", 2)]);
        assert_cells("⚛️", &[("⚛️", 2)]);

        let family = prepare("👨‍👩‍👧‍👦");
        assert_eq!(family.cells().len(), 1);
        assert_eq!(family.cells()[0].text().chars().count(), 7);
    }

    #[test]
    fn mixed_text_segments_into_the_expected_cells() {
        assert_cells(
            "你好 🚀 World",
            &[
                ("你", 2),
                ("好", 2),
                (" ", 1),
                ("🚀", 2),
                (" ", 1),
                ("W", 1),
                ("o", 1),
                ("r", 1),
                ("l", 1),
                ("d", 1),
            ],
        );
        assert_eq!(prepare("你好 🚀 World").width(), 13);
    }

    #[test]
    fn combining_marks_join_their_base_and_add_no_column() {
        assert_cells("e\u{301}b", &[("e\u{301}", 1), ("b", 1)]);
        // A zero-width cluster on its own is kept, measured at zero columns.
        assert_cells("\u{200b}", &[("\u{200b}", 0)]);
        assert_eq!(prepare("\u{200b}").width(), 0);
    }

    #[test]
    fn cells_are_stored_once_not_recomputed_per_read() {
        let prepared = prepare("你好 🚀");
        // Repeated reads hand back the very same storage: nothing is
        // re-segmented or re-measured on the render path.
        let first_read = prepared.cells().as_ptr();
        let second_read = prepared.cells().as_ptr();
        assert_eq!(first_read, second_read);
        assert_eq!(prepared.cells().len(), 4);
        assert_eq!(prepared.width(), 7);
        assert_eq!(prepared.text(), "你好 🚀");
        assert!(!prepared.is_empty());
    }

    #[test]
    fn empty_text_prepares_to_nothing_to_scroll() {
        let prepared = prepare("");
        assert!(prepared.is_empty());
        assert!(prepared.cells().is_empty());
        assert_eq!(prepared.width(), 0);
        assert_eq!(prepared.text(), "");
    }
}
