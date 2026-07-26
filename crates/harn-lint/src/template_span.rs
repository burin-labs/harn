//! Mapping a prompt-template position onto a source span.
//!
//! The template engine reports a diagnostic's position as the 1-based
//! line/column of the directive it is about. Everything downstream — the
//! CLI's underline renderer, the language server's ranges — needs byte
//! offsets, so the conversion lives here once for every template
//! diagnostic rather than once per rule.

use harn_lexer::Span;

/// Span the `{{ .. }}` directive that starts at 1-based `line`/`col`.
///
/// Falls back to the remainder of that line when no directive starts
/// there: a template can fail to parse precisely because the directive
/// at the reported position is malformed, and an approximate underline
/// beats none.
pub(crate) fn directive_span(source: &str, line: usize, col: usize) -> Span {
    let line = line.max(1);
    let col = col.max(1);
    let Some((line_start, line_text)) = nth_line(source, line) else {
        return Span::dummy();
    };
    let start = line_start + byte_offset_for_column(line_text, col);
    let line_end = line_start + line_text.len();
    let rest = &source[start..];
    let end = if rest.starts_with("{{") {
        rest.find("}}")
            .map(|idx| start + idx + 2)
            .unwrap_or(line_end)
    } else {
        line_end
    };
    let end = end.max(start);
    Span {
        start,
        end,
        line,
        column: col,
        end_line: line + source[start..end].matches('\n').count(),
    }
}

/// Byte offset and text (newline excluded) of the 1-based `line`.
fn nth_line(source: &str, line: usize) -> Option<(usize, &str)> {
    let mut offset = 0usize;
    for (index, text) in source.split_inclusive('\n').enumerate() {
        if index + 1 == line {
            return Some((offset, text.trim_end_matches(['\n', '\r'])));
        }
        offset += text.len();
    }
    None
}

/// Byte offset within `line_text` of the 1-based character `col`,
/// clamped to the end of the line.
fn byte_offset_for_column(line_text: &str, col: usize) -> usize {
    line_text
        .char_indices()
        .nth(col - 1)
        .map(|(offset, _)| offset)
        .unwrap_or(line_text.len())
}

#[cfg(test)]
mod tests {
    use super::directive_span;

    #[test]
    fn spans_the_directive_that_starts_at_the_position() {
        let source = "intro\n{{ if llm.provider == \"anthropic\" }}\nbody\n";
        let span = directive_span(source, 2, 1);
        assert_eq!(
            &source[span.start..span.end],
            "{{ if llm.provider == \"anthropic\" }}"
        );
        assert_eq!((span.line, span.column, span.end_line), (2, 1, 2));
    }

    #[test]
    fn separates_two_directives_sharing_a_line() {
        let source = "{{ if a }}x{{ elif b }}y{{ end }}\n";
        let elif_col = source[..source.find("{{ elif").unwrap()].chars().count() + 1;
        let span = directive_span(source, 1, elif_col);
        assert_eq!(&source[span.start..span.end], "{{ elif b }}");
    }

    #[test]
    fn falls_back_to_the_rest_of_the_line_without_a_directive() {
        let source = "plain text line\nnext\n";
        let span = directive_span(source, 1, 7);
        assert_eq!(&source[span.start..span.end], "text line");
    }

    #[test]
    fn counts_columns_in_characters_not_bytes() {
        let source = "héllo {{ x }}\n";
        let span = directive_span(source, 1, 7);
        assert_eq!(&source[span.start..span.end], "{{ x }}");
    }

    #[test]
    fn spans_a_directive_that_wraps_onto_a_later_line() {
        let source = "{{ if a\n  and b }}\nbody\n";
        let span = directive_span(source, 1, 1);
        assert_eq!(&source[span.start..span.end], "{{ if a\n  and b }}");
        assert_eq!(span.end_line, 2);
    }

    #[test]
    fn out_of_range_line_yields_a_dummy_span() {
        assert_eq!(
            directive_span("one line\n", 9, 1),
            harn_lexer::Span::dummy()
        );
    }
}
