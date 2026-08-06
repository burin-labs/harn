//! Source text paired with the line index that positions are resolved against.
//!
//! LSP speaks in `(line, UTF-16 column)` pairs while every Harn compiler span
//! is a byte offset, so each request converts between the two representations
//! many times over: once per semantic token, twice per diagnostic. Scanning the
//! document from byte zero for each of those conversions is quadratic in file
//! size on a path that runs on every keystroke.
//!
//! [`SourceText`] closes that by construction: text and index are built
//! together and the text is immutable afterwards, so the index cannot be stale
//! and there is nowhere to accidentally rebuild it. Conversions become a binary
//! search over line starts. `Deref<Target = str>` keeps the ordinary string
//! operations (slicing, `find`, `len`) reading exactly as they did before.

use std::ops::Deref;

use line_index::{LineIndex, TextSize, WideEncoding, WideLineCol};
use tower_lsp::lsp_types::Position;

/// LSP positions are UTF-16 code unit offsets, not characters and not bytes.
const LSP_ENCODING: WideEncoding = WideEncoding::Utf16;

/// An immutable source text and its precomputed line index.
#[derive(Debug, Clone)]
pub(crate) struct SourceText {
    text: String,
    index: LineIndex,
}

impl SourceText {
    pub(crate) fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let index = LineIndex::new(&text);
        Self { text, index }
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.text
    }

    /// Convert a byte offset into a 0-based LSP position.
    ///
    /// Offsets past the end clamp to the end, and an offset landing inside a
    /// multi-byte character resolves to that character's start.
    pub(crate) fn position(&self, offset: usize) -> Position {
        let offset = self.snap_to_char_boundary(offset);
        let line_col = self.index.line_col(TextSize::new(offset as u32));
        match self.index.to_wide(LSP_ENCODING, line_col) {
            Some(wide) => Position::new(wide.line, wide.col),
            None => Position::new(line_col.line, line_col.col),
        }
    }

    /// Convert a 0-based LSP position into a byte offset.
    ///
    /// A line past the end of the document yields the end of the document, and
    /// a column past the end of its line yields the end of that line, so a
    /// cursor an editor reports optimistically still lands somewhere valid.
    pub(crate) fn offset(&self, position: Position) -> usize {
        let Some((line_start, line_end)) = self.line_range(position.line) else {
            return self.text.len();
        };
        let wide = WideLineCol {
            line: position.line,
            col: position.character,
        };
        let Some(line_col) = self.index.to_utf8(LSP_ENCODING, wide) else {
            return line_end;
        };
        let offset = self.index.offset(line_col).map_or(line_end, usize::from);
        self.snap_to_char_boundary(offset.clamp(line_start, line_end))
    }

    /// Byte range of a 0-based line, excluding its terminating newline.
    /// `None` when the line is past the end of the document.
    pub(crate) fn line_range(&self, line: u32) -> Option<(usize, usize)> {
        let range = self.index.line(line)?;
        let start = usize::from(range.start());
        let mut end = usize::from(range.end());
        if end > start && self.text.as_bytes()[end - 1] == b'\n' {
            end -= 1;
        }
        Some((start, end))
    }

    /// The largest valid offset at or below `offset`.
    ///
    /// `LineIndex::try_line_col` rejects exactly the offsets that fall inside a
    /// multi-byte character, which is what a cursor placed between the halves
    /// of a surrogate pair maps to.
    fn snap_to_char_boundary(&self, offset: usize) -> usize {
        let mut offset = offset.min(self.text.len());
        while offset > 0
            && self
                .index
                .try_line_col(TextSize::new(offset as u32))
                .is_none()
        {
            offset -= 1;
        }
        offset
    }
}

impl Deref for SourceText {
    type Target = str;

    fn deref(&self) -> &str {
        &self.text
    }
}

impl From<String> for SourceText {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

impl From<&str> for SourceText {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

/// Length of `text` in UTF-16 code units — the unit LSP measures lengths in.
pub(crate) fn utf16_len(text: &str) -> u32 {
    LSP_ENCODING.measure(text) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every offset in `source` must survive a round trip through the LSP
    /// position representation, landing back on the character it started in.
    fn assert_round_trips(source: &str) {
        let text = SourceText::new(source);
        for offset in 0..=source.len() {
            if !source.is_char_boundary(offset) {
                continue;
            }
            let position = text.position(offset);
            assert_eq!(
                text.offset(position),
                offset,
                "offset {offset} of {source:?} round-trips through {position:?}"
            );
        }
    }

    #[test]
    fn ascii_positions_round_trip() {
        assert_round_trips("pipeline main() {\n  log(1)\n}\n");
    }

    #[test]
    fn multi_byte_positions_round_trip() {
        assert_round_trips("const café = 1\nconst naïve = 2\n");
    }

    #[test]
    fn cjk_positions_round_trip() {
        assert_round_trips("const 名前 = \"日本語\"\nlog(名前)\n");
    }

    #[test]
    fn astral_positions_round_trip() {
        assert_round_trips("const mood = \"😀\"\nlog(\"🎉🚀\")\n");
    }

    #[test]
    fn columns_count_utf16_code_units() {
        let source = "let 😀name = \"é\"\nnext";
        let text = SourceText::new(source);
        let name = source.find("name").unwrap();

        // `let ` is four units and the emoji is a surrogate pair, so `name`
        // starts at column six even though it starts at byte eight.
        assert_eq!(text.position(name), Position::new(0, 6));
        assert_eq!(text.offset(Position::new(0, 6)), name);
    }

    #[test]
    fn position_inside_a_surrogate_pair_resolves_to_the_character_start() {
        let source = "\"😀\"";
        let text = SourceText::new(source);
        // Column two is the low surrogate of the emoji — half of a character
        // no byte offset can name.
        assert_eq!(text.offset(Position::new(0, 2)), 1);
    }

    #[test]
    fn offsets_past_the_end_clamp_to_the_end() {
        let text = SourceText::new("abc\ndef");
        assert_eq!(text.position(999), Position::new(1, 3));
        assert_eq!(text.offset(Position::new(9, 0)), 7);
        assert_eq!(text.offset(Position::new(0, 99)), 3);
    }

    #[test]
    fn line_ranges_exclude_the_terminating_newline() {
        let text = SourceText::new("abc\n\ndef");
        assert_eq!(text.line_range(0), Some((0, 3)));
        assert_eq!(text.line_range(1), Some((4, 4)));
        assert_eq!(text.line_range(2), Some((5, 8)));
        assert_eq!(text.line_range(3), None);
    }

    #[test]
    fn trailing_newline_opens_a_final_empty_line() {
        let text = SourceText::new("abc\n");
        assert_eq!(text.line_range(1), Some((4, 4)));
        assert_eq!(text.position(4), Position::new(1, 0));
    }

    #[test]
    fn utf16_len_measures_code_units() {
        assert_eq!(utf16_len("abc"), 3);
        assert_eq!(utf16_len("café"), 4);
        assert_eq!(utf16_len("日本語"), 3);
        assert_eq!(utf16_len("😀"), 2);
    }
}
