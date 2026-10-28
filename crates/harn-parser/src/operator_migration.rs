//! Rewrite source written for the pre-0.11 operator grammar.
//!
//! The old grammar spelled ranges `a to b` (inclusive) and `a to b exclusive`,
//! placed range formation just above `|>`, and bound `??` tighter than `+`.
//! [`migrate_legacy_operators`] parses a file under
//! [`OperatorGrammar::LEGACY`], rewrites every range to `a..=b` or `a..b`, and
//! inserts parentheses wherever the current grammar would group an operand
//! differently. It then parses the result under the current grammar and
//! refuses the rewrite unless both trees match, so a migrated file always
//! keeps its meaning. Interpolation holes are migrated in place.

use harn_lexer::{FixEdit, Lexer, Span, StringSegment, Token, TokenKind};

use crate::ast_json::structural_projection;
use crate::interpolation;
use crate::operators::{
    is_parenthesized, operand_needs_parens, OperandSide, OperatorGrammar, Precedence,
};
use crate::operators::{RANGE_EXCLUSIVE, RANGE_INCLUSIVE};
use crate::{visit, Node, Parser, SNode, REMOVED_RANGE_KEYWORD};

/// Why a file could not be migrated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperatorMigrationError {
    /// The source does not parse under the legacy grammar.
    LegacyParse(String),
    /// The rewritten source does not parse under the current grammar.
    CurrentParse(String),
    /// The rewritten source parses to a different tree. `line` is where the
    /// first differing top-level item starts in the original source.
    MeaningChanged { line: usize },
}

impl std::fmt::Display for OperatorMigrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LegacyParse(error) => {
                write!(
                    f,
                    "does not parse under the legacy operator grammar: {error}"
                )
            }
            Self::CurrentParse(error) => {
                write!(
                    f,
                    "rewrite does not parse under the current grammar: {error}"
                )
            }
            Self::MeaningChanged { line } => write!(
                f,
                "rewrite would change the meaning of the item starting on line {line}"
            ),
        }
    }
}

/// Migrate `source`. Returns `Ok(None)` when the file needs no change.
pub fn migrate_legacy_operators(source: &str) -> Result<Option<String>, OperatorMigrationError> {
    let legacy = parse_program(source, OperatorGrammar::LEGACY)
        .map_err(OperatorMigrationError::LegacyParse)?;
    let mut edits = Vec::new();
    collect_program_edits(source, &legacy.tokens, &legacy.program, &mut edits);
    if edits.is_empty() {
        return Ok(None);
    }
    let migrated = FixEdit::apply_all(source, &edits);
    let current = parse_program(&migrated, OperatorGrammar::CURRENT)
        .map_err(OperatorMigrationError::CurrentParse)?;
    if let Some(line) = first_divergent_item(&legacy.program, &current.program) {
        return Err(OperatorMigrationError::MeaningChanged { line });
    }
    Ok(Some(migrated))
}

struct Parsed {
    tokens: Vec<Token>,
    program: Vec<SNode>,
}

fn parse_program(source: &str, grammar: OperatorGrammar) -> Result<Parsed, String> {
    let tokens = Lexer::new(source)
        .tokenize()
        .map_err(|error| error.to_string())?;
    let program = Parser::new(tokens.clone())
        .with_operator_grammar(grammar)
        .parse()
        .map_err(|error| error.to_string())?;
    Ok(Parsed { tokens, program })
}

fn collect_program_edits(
    source: &str,
    tokens: &[Token],
    program: &[SNode],
    edits: &mut Vec<FixEdit>,
) {
    visit::walk_program(program, &mut |node| {
        collect_node_edits(source, tokens, node, edits);
        if let Node::InterpolatedString(segments) = &node.node {
            for segment in segments {
                if let StringSegment::Expression(text, line, column) = segment {
                    collect_hole_edits(source, text, *line, *column, edits);
                }
            }
        }
    });
}

fn collect_hole_edits(
    source: &str,
    text: &str,
    line: usize,
    column: usize,
    edits: &mut Vec<FixEdit>,
) {
    let Ok(tokens) = interpolation::lexer(Some(source), text, line, column).tokenize() else {
        return;
    };
    let Some(expression) = interpolation::parse_expression_with_grammar(
        OperatorGrammar::LEGACY,
        Some(source),
        text,
        line,
        column,
    ) else {
        return;
    };
    collect_program_edits(source, &tokens, std::slice::from_ref(&expression), edits);
}

fn collect_node_edits(source: &str, tokens: &[Token], node: &SNode, edits: &mut Vec<FixEdit>) {
    match &node.node {
        Node::BinaryOp { op, left, right } => {
            let group = op.precedence();
            parenthesize(source, group, left, OperandSide::Left, edits);
            parenthesize(source, group, right, OperandSide::Right, edits);
        }
        Node::UnaryOp { operand, .. } => {
            parenthesize(
                source,
                Precedence::Prefix,
                operand,
                OperandSide::Prefix,
                edits,
            );
        }
        Node::Ternary {
            condition,
            true_expr,
            false_expr,
        } => {
            let group = Precedence::Ternary;
            parenthesize(
                source,
                group,
                condition,
                OperandSide::TernaryCondition,
                edits,
            );
            parenthesize(source, group, true_expr, OperandSide::TernaryBranch, edits);
            parenthesize(source, group, false_expr, OperandSide::TernaryBranch, edits);
        }
        Node::RangeExpr {
            start,
            end,
            inclusive,
        } => {
            parenthesize(source, Precedence::Range, start, OperandSide::Left, edits);
            parenthesize(source, Precedence::Range, end, OperandSide::Right, edits);
            respell_range(tokens, node.span, start.span, end.span, *inclusive, edits);
        }
        _ => {}
    }
}

/// Wrap `operand` in parentheses when the current grammar would not keep it
/// as this operand and the source does not already group it.
fn parenthesize(
    source: &str,
    parent: Precedence,
    operand: &SNode,
    side: OperandSide,
    edits: &mut Vec<FixEdit>,
) {
    if !operand_needs_parens(parent, Precedence::of_node(&operand.node), side)
        || source
            .get(operand.span.start..operand.span.end)
            .is_some_and(is_parenthesized)
    {
        return;
    }
    edits.push(insertion(operand.span.start, "("));
    edits.push(insertion(operand.span.end, ")"));
}

/// Replace `to` (with the whitespace around it) by `..=` or `..`, and drop an
/// `exclusive` suffix.
fn respell_range(
    tokens: &[Token],
    range: Span,
    start: Span,
    end: Span,
    inclusive: bool,
    edits: &mut Vec<FixEdit>,
) {
    let Some(to_index) = tokens.iter().position(|token| {
        token.span.start >= start.end
            && token.span.end <= end.start
            && matches!(&token.kind, TokenKind::Identifier(word) if word == REMOVED_RANGE_KEYWORD)
    }) else {
        return;
    };
    let before = to_index
        .checked_sub(1)
        .map_or(tokens[to_index].span.start, |index| tokens[index].span.end);
    let after = tokens
        .get(to_index + 1)
        .map_or(tokens[to_index].span.end, |token| token.span.start);
    let operator = if inclusive {
        RANGE_INCLUSIVE
    } else {
        RANGE_EXCLUSIVE
    };
    edits.push(replacement(before, after, operator));
    if inclusive {
        return;
    }
    if let Some(exclusive_index) = tokens.iter().position(|token| {
        token.span.start >= end.end
            && token.span.end <= range.end
            && matches!(&token.kind, TokenKind::Identifier(word) if word == "exclusive")
    }) {
        let removal_start = tokens[exclusive_index - 1].span.end;
        edits.push(replacement(
            removal_start,
            tokens[exclusive_index].span.end,
            "",
        ));
    }
}

fn insertion(offset: usize, text: &str) -> FixEdit {
    replacement(offset, offset, text)
}

fn replacement(start: usize, end: usize, text: &str) -> FixEdit {
    FixEdit {
        span: Span::with_offsets(start, end, 0, 0),
        replacement: text.to_string(),
    }
}

/// The source line of the first top-level item whose meaning differs.
fn first_divergent_item(legacy: &[SNode], current: &[SNode]) -> Option<usize> {
    let items = legacy.len().max(current.len());
    (0..items).find_map(|index| {
        let before = legacy.get(index);
        let after = current.get(index);
        let same = match (before, after) {
            (Some(before), Some(after)) => {
                structural_projection(std::slice::from_ref(before), OperatorGrammar::LEGACY)
                    == structural_projection(std::slice::from_ref(after), OperatorGrammar::CURRENT)
            }
            _ => false,
        };
        (!same).then(|| before.or(after).map_or(0, |item| item.span.line))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn migrate(source: &str) -> String {
        migrate_legacy_operators(source)
            .expect("migrates")
            .unwrap_or_else(|| source.to_string())
    }

    #[test]
    fn respells_ranges() {
        assert_eq!(migrate("for i in 1 to 3 { }\n"), "for i in 1..=3 { }\n");
        assert_eq!(
            migrate("for i in 0 to n exclusive { }\n"),
            "for i in 0..n { }\n"
        );
        assert_eq!(
            migrate("let r = 0 to len(xs) - 1\n"),
            "let r = 0..=len(xs) - 1\n"
        );
    }

    #[test]
    fn keeps_the_legacy_grouping_of_nil_coalescing() {
        assert_eq!(migrate("let x = b ?? 0 + 1\n"), "let x = (b ?? 0) + 1\n");
        assert_eq!(migrate("let x = a + b ?? 1\n"), "let x = a + (b ?? 1)\n");
        assert_eq!(migrate("let x = (a ?? 0) + 1\n"), "let x = (a ?? 0) + 1\n");
        assert_eq!(migrate("let x = a ?? b == c\n"), "let x = a ?? b == c\n");
    }

    #[test]
    fn parenthesizes_bounds_that_now_bind_looser_than_a_range() {
        assert_eq!(migrate("let r = 0 to n ?? 5\n"), "let r = 0..=(n ?? 5)\n");
        assert_eq!(
            migrate("let r = c ? 1 : 2 to 5\n"),
            "let r = (c ? 1 : 2)..=5\n"
        );
    }

    #[test]
    fn ignores_positions_held_outside_span_fields() {
        // `IfElse` stores `then_span`, which shifts when a range respells.
        assert_eq!(
            migrate(
                "fn f() {\n  for i in 1 to 6 {\n    if i == 3 {\n      continue\n    }\n  }\n}\n"
            ),
            "fn f() {\n  for i in 1..=6 {\n    if i == 3 {\n      continue\n    }\n  }\n}\n"
        );
    }

    #[test]
    fn migrates_interpolation_holes_in_place() {
        assert_eq!(
            migrate("let s = \"${b ?? 0 + 1} and ${1 to 2}\"\n"),
            "let s = \"${(b ?? 0) + 1} and ${1..=2}\"\n"
        );
    }

    #[test]
    fn is_idempotent_on_migrated_source() {
        let once = migrate("let x = b ?? 0 + 1\nfor i in 0 to n exclusive { }\n");
        assert_eq!(migrate_legacy_operators(&once), Ok(None));
    }
}
