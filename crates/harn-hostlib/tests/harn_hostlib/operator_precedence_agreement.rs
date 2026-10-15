//! The tree-sitter grammar groups every operator pair the way the parser does.
//!
//! `harn_parser::operators` owns precedence. The tree-sitter grammar reads the
//! generated `PREC` constants, but a rule could still hard-code a number or
//! pick the wrong group. This test parses `a OP1 b OP2 c` for every pair of
//! binary operators with both parsers and compares which side grouped first.

use harn_hostlib::ast::Language;
use harn_parser::operators::{BinaryOperator, RANGE_EXCLUSIVE, RANGE_INCLUSIVE};
use harn_parser::{Node, Parser as HarnParser};
use tree_sitter::{Node as TsNode, Parser};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grouping {
    Left,
    Right,
}

/// Every infix spelling the parser accepts, including range formation.
fn infix_spellings() -> Vec<&'static str> {
    let mut spellings: Vec<&'static str> =
        BinaryOperator::ALL.iter().map(|op| op.symbol()).collect();
    spellings.extend([RANGE_EXCLUSIVE, RANGE_INCLUSIVE]);
    spellings
}

/// How the Rust parser grouped `expression`, or `None` when it rejects it.
fn parser_grouping(expression: &str) -> Option<Grouping> {
    let tokens = harn_lexer::Lexer::new(expression).tokenize().ok()?;
    let node = HarnParser::new(tokens).parse_single_expression().ok()?;
    let (left, right) = match &node.node {
        Node::BinaryOp { left, right, .. } => (left, right),
        Node::RangeExpr { start, end, .. } => (start, end),
        _ => return None,
    };
    let is_compound = |node: &Node| matches!(node, Node::BinaryOp { .. } | Node::RangeExpr { .. });
    match (is_compound(&left.node), is_compound(&right.node)) {
        (true, false) => Some(Grouping::Left),
        (false, true) => Some(Grouping::Right),
        _ => None,
    }
}

/// How tree-sitter grouped `a OP1 b OP2 c`: which operand pair forms a node of
/// its own. Judged by byte ranges so the test does not depend on node names.
fn tree_sitter_grouping(parser: &mut Parser, expression: &str) -> Option<Grouping> {
    let source = format!("let probe = {expression}\n");
    let tree = parser.parse(&source, None)?;
    if tree.root_node().has_error() {
        return None;
    }
    let offset = source.find(expression)?;
    let a = offset;
    let b = offset + expression.find(" b ")? + 1;
    let c = offset + expression.len() - 1;
    let top = find_spanning(tree.root_node(), (a, c + 1))?;
    let mut cursor = top.walk();
    let operands: Vec<(usize, usize)> = top
        .named_children(&mut cursor)
        .map(|child| (child.start_byte(), child.end_byte()))
        .collect();
    if operands.contains(&(a, b + 1)) {
        Some(Grouping::Left)
    } else if operands.contains(&(b, c + 1)) {
        Some(Grouping::Right)
    } else {
        None
    }
}

/// The outermost named node spanning exactly `range`.
fn find_spanning(node: TsNode<'_>, range: (usize, usize)) -> Option<TsNode<'_>> {
    if (node.start_byte(), node.end_byte()) == range && node.named_child_count() >= 2 {
        return Some(node);
    }
    let mut cursor = node.walk();
    let children: Vec<TsNode> = node.named_children(&mut cursor).collect();
    children
        .into_iter()
        .filter(|child| child.start_byte() <= range.0 && range.1 <= child.end_byte())
        .find_map(|child| find_spanning(child, range))
}

#[test]
fn tree_sitter_groups_every_operator_pair_like_the_parser() {
    let language = Language::Harn
        .ts_language()
        .expect("the Harn grammar must be compiled into harn-hostlib");
    let mut parser = Parser::new();
    parser.set_language(&language).expect("load Harn grammar");

    let spellings = infix_spellings();
    let mut compared = 0usize;
    let mut disagreements = Vec::new();
    for first in &spellings {
        for second in &spellings {
            // Spaced operands keep `..` from touching identifiers ambiguously.
            let expression = format!("a {first} b {second} c");
            let Some(expected) = parser_grouping(&expression) else {
                // The parser rejects it (for example `a..b..c`).
                continue;
            };
            compared += 1;
            let actual = tree_sitter_grouping(&mut parser, &expression);
            if actual != Some(expected) {
                disagreements.push(format!(
                    "{expression}: parser {expected:?}, tree-sitter {actual:?}"
                ));
            }
        }
    }
    // A measured zero must not read as agreement.
    assert!(
        compared > spellings.len() * spellings.len() / 2,
        "only {compared} operator pairs were comparable"
    );
    assert!(
        disagreements.is_empty(),
        "tree-sitter disagrees with harn-parser on {} of {compared} operator pairs:\n{}",
        disagreements.len(),
        disagreements.join("\n")
    );
}
