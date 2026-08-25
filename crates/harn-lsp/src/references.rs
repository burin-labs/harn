use harn_lexer::{Lexer, Span, TokenKind};

/// Refine raw AST reference spans down to the exact identifier tokens.
///
/// Re-lexes `source` and keeps each `name` identifier token that falls
/// inside one of `ref_spans`, deduplicated by offset. This is what makes
/// find-references and rename point at the identifier itself instead of
/// highlighting an entire `fn`/`pipeline`/`let` declaration.
pub(crate) fn identifier_token_spans_within(
    source: &str,
    name: &str,
    ref_spans: &[Span],
) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut seen_offsets = std::collections::HashSet::new();
    let mut lexer = Lexer::new(source);
    let Ok(tokens) = lexer.tokenize() else {
        return spans;
    };
    for token in &tokens {
        if let TokenKind::Identifier(ref token_name) = token.kind {
            if token_name == name
                && ref_spans
                    .iter()
                    .any(|rs| token.span.start >= rs.start && token.span.end <= rs.end)
                && seen_offsets.insert(token.span.start)
            {
                spans.push(token.span);
            }
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn whole(source: &str) -> Span {
        Span {
            start: 0,
            end: source.len(),
            line: 1,
            column: 1,
            end_line: 1,
        }
    }

    #[test]
    #[expect(clippy::string_slice, reason = "test input is ASCII")]
    fn references_to_fn_param_refine_to_identifier_tokens() {
        let source = "fn process(data) {\n  return data\n}\n";
        // A whole-file span stands in for a raw AST hit: refinement must
        // shrink it to the 4-byte identifier tokens themselves.
        let refined = identifier_token_spans_within(source, "data", &[whole(source)]);
        assert_eq!(refined.len(), 2, "param + body use; got {refined:?}");
        for span in &refined {
            assert_eq!(span.end - span.start, "data".len(), "span {span:?}");
            assert_eq!(&source[span.start..span.end], "data");
        }
    }

    #[test]
    #[expect(clippy::string_slice, reason = "test input is ASCII")]
    fn references_to_let_binding_refine_to_identifier_tokens() {
        let source = "pipeline t(task) {\n  const total = 1\n  log(total)\n}\n";
        let refined = identifier_token_spans_within(source, "total", &[whole(source)]);
        assert_eq!(refined.len(), 2, "binding + use; got {refined:?}");
        for span in &refined {
            assert_eq!(&source[span.start..span.end], "total");
        }
    }
}
