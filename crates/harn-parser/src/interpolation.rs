//! Shared parsing for `${...}` expression holes.
//!
//! The lexer stores an interpolation hole as source text plus the line and
//! column where it starts. Everything that needs the expression back — the
//! typechecker, the linter, and `harn fix` — parses it through here, so every
//! consumer sees spans in the containing file's coordinates rather than
//! offsets relative to the hole (harn#5850).

use harn_lexer::Lexer;

use crate::operators::OperatorGrammar;
use crate::{Parser, SNode};

/// Reuse line boundaries when resolving several positions in one source file.
pub(crate) struct SourceCoordinates<'a> {
    source: &'a str,
    line_starts: Vec<usize>,
}

impl<'a> SourceCoordinates<'a> {
    pub(crate) fn new(source: &'a str) -> Self {
        let mut line_starts = vec![0];
        line_starts.extend(source.match_indices('\n').map(|(offset, _)| offset + 1));
        Self {
            source,
            line_starts,
        }
    }

    fn offset(&self, line: usize, column: usize) -> Option<usize> {
        let index = line.checked_sub(1)?;
        let start = *self.line_starts.get(index)?;
        let end = self
            .line_starts
            .get(index + 1)
            .map_or(self.source.len(), |next| next - 1);
        let text = self.source.get(start..end)?;
        harn_lexer::byte_offset_for_position(text, 1, column).map(|offset| start + offset)
    }
}

/// Parse a hole using line boundaries shared with the containing file's walk.
pub(crate) fn parse_expression_indexed(
    coordinates: &SourceCoordinates<'_>,
    segment: &str,
    line: usize,
    column: usize,
) -> Option<SNode> {
    let offset = coordinates.offset(line, column).unwrap_or_default();
    parse_with_lexer(
        Lexer::with_position_and_offset(segment, line, column, offset),
        OperatorGrammar::CURRENT,
    )
}

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
    parse_expression_with_grammar(OperatorGrammar::CURRENT, source, segment, line, column)
}

/// [`parse_expression`] under an explicit operator grammar. Only the syntax
/// migration reads holes with [`OperatorGrammar::LEGACY`].
pub fn parse_expression_with_grammar(
    grammar: OperatorGrammar,
    source: Option<&str>,
    segment: &str,
    line: usize,
    column: usize,
) -> Option<SNode> {
    parse_with_lexer(lexer(source, segment, line, column), grammar)
}

fn parse_with_lexer(mut lexer: Lexer<'_>, grammar: OperatorGrammar) -> Option<SNode> {
    Parser::new(lexer.tokenize().ok()?)
        .with_operator_grammar(grammar)
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
    fn indexed_positions_match_lexer_coordinates() {
        for source in ["", "x", "\n", "éα\r\nβ x\n", "hello\nworld\n\nlast"] {
            let coordinates = SourceCoordinates::new(source);
            for line in 0..=source.lines().count() + 2 {
                for column in 0..=source.len() + 2 {
                    assert_eq!(
                        coordinates.offset(line, column),
                        harn_lexer::byte_offset_for_position(source, line, column),
                        "source={source:?} line={line} column={column}",
                    );
                }
            }
        }
    }

    #[test]
    fn source_aware_bindings_cover_nested_interpolations() {
        let source = "fn helper() {\r\n  let value = 1\n  \"pré ${\"inner ${value}\"}\"\n}\n";
        let tokens = Lexer::new(source).tokenize().unwrap();
        let program = Parser::new(tokens).parse().unwrap();
        let bindings = crate::lexical::resolved_identifier_bindings_with_source(
            &[],
            &program,
            Some(source),
            &crate::lexical::MatchPatternCatalog::default(),
        );
        let use_start = source.rfind("value").unwrap();
        let binding = bindings
            .get(&(use_start, use_start + "value".len()))
            .expect("nested interpolation resolves the local binding");
        assert_eq!(binding.name, "value");
    }

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
