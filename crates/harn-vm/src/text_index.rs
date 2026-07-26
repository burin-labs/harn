//! Positions in a model response that scanners test repeatedly.
//!
//! Two questions come up over and over while walking a response: is this offset
//! inside a markdown fence, and is it the first non-blank thing on its line?
//! Answering either by rescanning from the start of the text costs time
//! proportional to the offset, so a scan that asks at every position is
//! quadratic in the length of the response. Building the positions once and
//! binary-searching them makes each lookup logarithmic.
//!
//! This is also the single owner of "inside a fence". The answer used to be
//! reimplemented per module, and the copies had drifted apart: one counted
//! fence markers and called an odd count "inside", which lets an unbalanced
//! fence earlier in a response swallow real content after it.

/// Byte offsets of the structural positions in a piece of text.
pub(crate) struct TextIndex {
    /// Non-overlapping ```` ``` ```` markers, ascending.
    fence_markers: Vec<usize>,
    /// `\n` bytes, ascending.
    newlines: Vec<usize>,
}

impl TextIndex {
    pub(crate) fn build(text: &str) -> Self {
        let mut fence_markers = Vec::new();
        let mut cursor = 0;
        while let Some(relative) = text[cursor..].find("```") {
            let position = cursor + relative;
            fence_markers.push(position);
            cursor = position + 3;
        }

        let newlines = text
            .bytes()
            .enumerate()
            .filter(|(_, byte)| *byte == b'\n')
            .map(|(offset, _)| offset)
            .collect();

        Self {
            fence_markers,
            newlines,
        }
    }

    /// Is `offset` enclosed by a *closed* markdown fence?
    ///
    /// A fence encloses the offset only when the fence that is open there is
    /// matched by a marker at or after it. That makes a closed example fence
    /// skippable while leaving an unbalanced *trailing* fence harmless — it has
    /// no business swallowing content that follows it.
    pub(crate) fn inside_markdown_fence(&self, offset: usize) -> bool {
        let markers_before = self.fence_markers.partition_point(|pos| *pos < offset);
        markers_before < self.fence_markers.len() && markers_before % 2 == 1
    }

    /// Is everything between the start of `offset`'s line and `offset` blank?
    ///
    /// `text` must be the same text the index was built from.
    pub(crate) fn is_line_leading(&self, text: &str, offset: usize) -> bool {
        let line_start = match self.newlines.partition_point(|pos| *pos < offset) {
            0 => 0,
            preceding => self.newlines[preceding - 1] + 1,
        };
        text[line_start..offset]
            .chars()
            .all(|ch| matches!(ch, ' ' | '\t' | '\r'))
    }
}

#[cfg(test)]
mod tests {
    use super::TextIndex;

    /// The pre-index spelling of "inside a fence", kept as the reference the
    /// indexed lookup must agree with at every offset.
    fn reference_inside_fence(text: &str, offset: usize) -> bool {
        let mut open_before_offset = false;
        let mut scan = 0;
        while let Some(relative) = text[scan..].find("```") {
            let position = scan + relative;
            if position >= offset {
                return open_before_offset;
            }
            open_before_offset = !open_before_offset;
            scan = position + 3;
        }
        false
    }

    fn reference_line_leading(text: &str, offset: usize) -> bool {
        let line_start = text[..offset].rfind('\n').map(|pos| pos + 1).unwrap_or(0);
        text[line_start..offset]
            .chars()
            .all(|ch| matches!(ch, ' ' | '\t' | '\r'))
    }

    #[test]
    fn indexed_lookups_match_the_rescanning_reference() {
        for text in [
            "",
            "no fences here",
            "```\nfenced\n```\nafter",
            // Unbalanced trailing fence: content after it is NOT fenced.
            "before\n```\ntrailing open fence\n<tool_call>look({})</tool_call>",
            // Two closed fences with a real block between them.
            "```a```\n<tool_call>x({})</tool_call>\n```b```",
            "  \t<tool_call>\nindented block\n</tool_call>",
            "``````\nsix backticks\n```",
        ] {
            let index = TextIndex::build(text);
            for offset in 0..=text.len() {
                if !text.is_char_boundary(offset) {
                    continue;
                }
                assert_eq!(
                    index.inside_markdown_fence(offset),
                    reference_inside_fence(text, offset),
                    "fence lookup diverged at offset {offset} of {text:?}"
                );
                assert_eq!(
                    index.is_line_leading(text, offset),
                    reference_line_leading(text, offset),
                    "line-leading lookup diverged at offset {offset} of {text:?}"
                );
            }
        }
    }

    #[test]
    fn unbalanced_leading_fence_does_not_swallow_later_content() {
        // The divergence that motivated a single owner: a stray ``` earlier in
        // the response must not mark a later real block as fenced narration.
        let text = "```\n<user_response>hi</user_response>";
        let index = TextIndex::build(text);
        let block = text.find("<user_response>").expect("block present");
        assert!(!index.inside_markdown_fence(block));
    }
}
