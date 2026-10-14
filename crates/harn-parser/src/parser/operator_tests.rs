//! Operator precedence, range spelling, and contextual keyword parsing.

use harn_lexer::Lexer;

use super::{Parser, ParserError};
use crate::ast::{BinaryOperator, Node, SNode};
use crate::operators::OperatorGrammar;

fn parse_with(source: &str, grammar: OperatorGrammar) -> Result<Vec<SNode>, ParserError> {
    let tokens = Lexer::new(source).tokenize().unwrap();
    Parser::new(tokens).with_operator_grammar(grammar).parse()
}

fn parse(source: &str) -> Result<Vec<SNode>, ParserError> {
    parse_with(source, OperatorGrammar::CURRENT)
}

fn expression(source: &str) -> SNode {
    let tokens = Lexer::new(source).tokenize().unwrap();
    Parser::new(tokens).parse_single_expression().unwrap()
}

/// Render the grouping of an expression with explicit parentheses.
fn grouping(node: &SNode) -> String {
    match &node.node {
        Node::BinaryOp { op, left, right } => {
            format!("({} {} {})", grouping(left), op.symbol(), grouping(right))
        }
        Node::RangeExpr {
            start,
            end,
            inclusive,
        } => {
            let operator = if *inclusive { "..=" } else { ".." };
            format!("({}{operator}{})", grouping(start), grouping(end))
        }
        Node::Identifier(name) => name.clone(),
        Node::IntLiteral(value) => value.to_string(),
        other => format!("{other:?}"),
    }
}

#[test]
fn nil_coalescing_binds_looser_than_arithmetic_and_tighter_than_comparison() {
    assert_eq!(grouping(&expression("b ?? 0 + 1")), "(b ?? (0 + 1))");
    assert_eq!(grouping(&expression("a ?? 0 == 1")), "((a ?? 0) == 1)");
    assert_eq!(grouping(&expression("a ?? b * c")), "(a ?? (b * c))");
    assert_eq!(grouping(&expression("a ?? b ?? c")), "((a ?? b) ?? c)");
}

#[test]
fn ranges_bind_between_nil_coalescing_and_arithmetic() {
    assert_eq!(grouping(&expression("0..n - 1")), "(0..(n - 1))");
    assert_eq!(grouping(&expression("1..=n")), "(1..=n)");
    assert_eq!(grouping(&expression("x in 0..n")), "(x in (0..n))");
    assert_eq!(grouping(&expression("a ?? 0..n")), "(a ?? (0..n))");
}

#[test]
fn ranges_do_not_chain() {
    let tokens = Lexer::new("0..1..2").tokenize().unwrap();
    assert!(Parser::new(tokens).parse_single_expression().is_err());
}

#[test]
fn removed_range_spelling_reports_a_targeted_error() {
    let error = parse("for i in 1 to 3 { }").unwrap_err();
    let ParserError::RemovedRangeSyntax { span } = error else {
        panic!("expected RemovedRangeSyntax, got {error:?}");
    };
    assert_eq!((span.line, span.column), (1, 12));
}

#[test]
fn legacy_grammar_reads_the_old_operator_rules() {
    let program = parse_with("let r = b ?? 0 + 1 to 9 exclusive", OperatorGrammar::LEGACY)
        .expect("legacy source parses");
    let Node::LetBinding { value, .. } = &program[0].node else {
        panic!("expected let binding");
    };
    assert_eq!(grouping(value), "(((b ?? 0) + 1)..9)");
}

#[test]
fn de_reserved_words_are_identifiers_outside_their_constructs() {
    let program = parse(
        "let to = 1\nlet select = 2\nlet tool = {name: \"t\"}\nlet require = to + select\n\
         let from = tool.name\nlet exclusive = true\nlet emit = [require]\nlet mutex = emit\n\
         let deadline = 3\nlet skill = deadline\nlet eval_pack = skill\n",
    )
    .expect("contextual keywords parse as identifiers");
    assert_eq!(program.len(), 11);
}

#[test]
fn contextual_constructs_still_parse() {
    let program = parse(
        "select {\n  v from ch { log(v) }\n  default { }\n}\nrequire ok, \"must be ok\"\n\
         mutex(\"k\") { }\ndeadline 5s { }\n",
    )
    .expect("contextual constructs parse");
    assert!(matches!(program[0].node, Node::SelectExpr { .. }));
    assert!(matches!(program[1].node, Node::RequireStmt { .. }));
    assert!(matches!(program[2].node, Node::MutexBlock { .. }));
    assert!(matches!(program[3].node, Node::DeadlineBlock { .. }));
}

#[test]
fn not_in_is_a_single_comparison_operator() {
    let node = expression("a not in b");
    assert!(matches!(
        &node.node,
        Node::BinaryOp {
            op: BinaryOperator::NotIn,
            ..
        }
    ));
}
