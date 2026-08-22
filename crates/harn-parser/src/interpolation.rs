//! Shared parsing for `${...}` expression holes.
//!
//! The lexer stores an interpolation hole as source text plus the line and
//! column where it starts. Everything that needs the expression back — the
//! typechecker, the linter, and `harn fix` — parses it through here, so every
//! consumer sees spans in the containing file's coordinates rather than
//! offsets relative to the hole (harn#5850).

use harn_lexer::Lexer;

use crate::{Parser, SNode};

/// Parse one `${...}` hole into an expression whose spans address `source`.
///
/// `segment`, `line`, and `column` come from a
/// [`harn_lexer::StringSegment::Expression`]; `source` is the whole file that
/// segment was lexed from. Pass `None` only when the containing file is not
/// available — the expression still parses, but its spans stay relative to the
/// hole and must not be used to edit the file.
pub fn parse_expression(
    source: Option<&str>,
    segment: &str,
    line: usize,
    column: usize,
) -> Option<SNode> {
    Parser::new(lexer(source, segment, line, column).tokenize().ok()?)
        .parse_single_expression()
        .ok()
}

/// Build the lexer `parse_expression` uses, for callers that need the tokens.
pub fn lexer<'seg>(
    source: Option<&str>,
    segment: &'seg str,
    line: usize,
    column: usize,
) -> Lexer<'seg> {
    let offset = source
        .and_then(|source| harn_lexer::byte_offset_for_position(source, line, column))
        .unwrap_or_default();
    Lexer::with_position_and_offset(segment, line, column, offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Node;

    #[test]
    #[expect(
        clippy::string_slice,
        reason = "expression spans come from the lexer and lie on char boundaries"
    )]
    fn parses_a_hole_into_spans_that_address_the_containing_source() {
        let source = "const label = \"é ${platform()}\"\n";

        let expression = parse_expression(Some(source), "platform()", 1, 20).expect("expression");

        assert!(matches!(expression.node, Node::FunctionCall { .. }));
        assert_eq!(
            &source[expression.span.start..expression.span.end],
            "platform()"
        );
    }
}
