//! Shared AST traversal helpers for source-aware lint rules. The
//! per-variant recursion lives in `harn_parser::visit` so the linter,
//! formatter, and any future tooling stay in lockstep when new
//! `Node` variants land.

use std::collections::HashSet;

use harn_parser::{visit, Node, SNode};

pub(crate) fn collect_expression_spans(
    program: &[SNode],
    mut include: impl FnMut(&Node) -> bool,
) -> HashSet<(usize, usize)> {
    let mut spans = HashSet::new();
    visit::walk_program(program, &mut |node| {
        if include(&node.node) {
            spans.insert((node.span.start, node.span.end));
        }
    });
    spans
}
