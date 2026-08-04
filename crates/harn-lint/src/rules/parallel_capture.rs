//! `mutable-capture-across-parallel` rule (`HARN-LNT-064`).
//!
//! Harn closures capture their environment by *reference*: a mutable local
//! reassigned inside a nested closure writes through a shared cell, so the
//! outer binding sees the update. `parallel` / `spawn` bodies are lowered into
//! exactly such closures, and every concurrent branch runs a *clone* of that
//! closure that shares the captured cells by `Arc`. Reassigning a captured
//! variable from inside a branch therefore mutates one cell from many branches
//! at once.
//!
//! That is memory-safe — the cell is an `Arc<Mutex<_>>` — but it is almost
//! never what the author means. On the cooperative scheduler the branches
//! interleave at every `await` point (an `llm_call`, a host call), so a
//! read-modify-write like `total = total + x` loses updates; under a future
//! multi-threaded runtime it is a genuine data race in the logical sense. The
//! idiomatic fix is to have each branch *return* its contribution and combine
//! the results after the fan-out (`const parts = parallel each xs { ... }`),
//! which is deterministic and lock-free.
//!
//! The rule fires on an assignment, inside a `parallel`/`spawn` body, whose
//! target's root identifier is *not* bound within that body (so it is captured
//! from an enclosing scope). Reassigning a body-local `let`, or writing the
//! branch's own accumulator, is fine and never flagged. To stay high-signal it
//! does not descend into closures nested *within* the body — those open a
//! fresh capture scope of their own — and it treats a name introduced anywhere
//! in the body scope as body-local, so it never cries wolf on a shadowed name.

use std::collections::HashSet;

use harn_lexer::Span;
use harn_parser::{BindingPattern, DiagnosticCode as Code, Node, SNode};

use crate::diagnostic::{LintDiagnostic, LintSeverity};

const RULE_NAME: &str = "mutable-capture-across-parallel";

pub(crate) fn check_mutable_capture_across_parallel(
    program: &[SNode],
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    for node in program {
        visit(node, diagnostics);
    }
}

/// A node that opens a fresh capture/binding scope. Analysis of one
/// `parallel`/`spawn` body stops at these: a closure nested inside the body
/// captures relative to the body (not the enclosing frame), and a nested
/// `parallel`/`spawn` is analyzed on its own when the walk reaches it.
fn opens_scope(node: &Node) -> bool {
    matches!(
        node,
        Node::Closure { .. }
            | Node::FnDecl { .. }
            | Node::ToolDecl { .. }
            | Node::Parallel { .. }
            | Node::SpawnExpr { .. }
    )
}

/// Whole-program walk. At each `parallel`/`spawn` node analyze its body, then
/// keep descending so nested concurrency (and concurrency inside functions) is
/// analyzed with its own body scope.
fn visit(node: &SNode, diagnostics: &mut Vec<LintDiagnostic>) {
    match &node.node {
        Node::Parallel { variable, body, .. } => {
            let mut bound = HashSet::new();
            // The loop variable (`parallel each xs { x -> ... }`) is a branch
            // parameter, immutable and branch-local.
            if let Some(var) = variable {
                bound.insert(var.clone());
            }
            analyze_body(body, bound, diagnostics);
        }
        Node::SpawnExpr { body } => {
            analyze_body(body, HashSet::new(), diagnostics);
        }
        _ => {}
    }
    for child in harn_parser::visit::immediate_children(node) {
        visit(child, diagnostics);
    }
}

fn analyze_body(body: &[SNode], mut bound: HashSet<String>, diagnostics: &mut Vec<LintDiagnostic>) {
    for stmt in body {
        collect_bound(stmt, &mut bound);
    }
    for stmt in body {
        flag_captured_assignments(stmt, &bound, diagnostics);
    }
}

/// Collect every name introduced within the current body scope (`let`/`const`
/// bindings, `for`-in patterns), descending through control flow but stopping
/// at nested scope openers.
fn collect_bound(node: &SNode, bound: &mut HashSet<String>) {
    match &node.node {
        Node::LetBinding { pattern, .. } | Node::ConstBinding { pattern, .. } => {
            pattern_idents(pattern, bound);
        }
        Node::ForIn { pattern, .. } => {
            pattern_idents(pattern, bound);
        }
        other if opens_scope(other) => return,
        _ => {}
    }
    for child in harn_parser::visit::immediate_children(node) {
        collect_bound(child, bound);
    }
}

/// Flag assignments whose target root identifier is captured from an enclosing
/// scope. Descends through control flow but stops at nested scope openers.
fn flag_captured_assignments(
    node: &SNode,
    bound: &HashSet<String>,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    if opens_scope(&node.node) {
        return;
    }
    if let Node::Assignment { target, .. } = &node.node {
        if let Some((name, span)) = target_root(target)
            .filter(|(name, _)| !bound.contains(name) && !harn_parser::is_discard_name(name))
        {
            diagnostics.push(diagnostic(&name, span));
        }
    }
    for child in harn_parser::visit::immediate_children(node) {
        flag_captured_assignments(child, bound, diagnostics);
    }
}

/// The root identifier an assignment target ultimately writes to: `x`,
/// `x[i]`, `x.f`, `x.f[i].g` all root at `x`. Returns the identifier's span so
/// the diagnostic anchors at the write site.
fn target_root(node: &SNode) -> Option<(String, Span)> {
    match &node.node {
        Node::Identifier(name) => Some((name.clone(), node.span)),
        Node::PropertyAccess { object, .. }
        | Node::OptionalPropertyAccess { object, .. }
        | Node::SubscriptAccess { object, .. }
        | Node::OptionalSubscriptAccess { object, .. }
        | Node::SliceAccess { object, .. } => target_root(object),
        _ => None,
    }
}

fn pattern_idents(pattern: &BindingPattern, bound: &mut HashSet<String>) {
    match pattern {
        BindingPattern::Identifier(name) => {
            bound.insert(name.clone());
        }
        BindingPattern::Pair(first, second) => {
            bound.insert(first.clone());
            bound.insert(second.clone());
        }
        BindingPattern::Dict(fields) => {
            for field in fields {
                bound.insert(field.alias.clone().unwrap_or_else(|| field.key.clone()));
            }
        }
        BindingPattern::List(elements) => {
            for element in elements {
                bound.insert(element.name.clone());
            }
        }
    }
}

fn diagnostic(name: &str, span: Span) -> LintDiagnostic {
    LintDiagnostic {
        code: Code::LintMutableCaptureAcrossParallel,
        rule: RULE_NAME.into(),
        message: format!(
            "`{name}` is captured from an enclosing scope and reassigned inside a `parallel`/`spawn` body; every concurrent branch shares one cell, so the writes race and can be lost"
        ),
        span,
        severity: LintSeverity::Warning,
        suggestion: Some(format!(
            "have each branch return its contribution and combine the results after the fan-out (e.g. `const parts = parallel each ... {{ ... }}`) instead of mutating the shared `{name}`"
        )),
        fix: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_lexer::Lexer;
    use harn_parser::Parser;

    fn flagged_names(source: &str) -> Vec<String> {
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let program = Parser::new(tokens).parse().expect("parse");
        let mut diags = Vec::new();
        check_mutable_capture_across_parallel(&program, &mut diags);
        for diag in &diags {
            assert_eq!(diag.code, Code::LintMutableCaptureAcrossParallel);
            assert_eq!(diag.severity, LintSeverity::Warning);
        }
        // The message leads with the offending name in backticks; recover it so
        // tests assert on *which* variable was flagged, not just the count.
        diags
            .iter()
            .map(|d| {
                d.message
                    .split('`')
                    .nth(1)
                    .expect("message names the variable")
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn warns_on_scalar_reassign_in_parallel_each() {
        let names = flagged_names(
            r"
pipeline default() {
  let total = 0
  parallel each [1, 2, 3] { x ->
    total = total + x
  }
  log(total)
}
",
        );
        assert_eq!(names, vec!["total"]);
    }

    #[test]
    fn warns_on_container_and_field_writes() {
        // A subscript or property write roots at the captured container.
        let names = flagged_names(
            r"
pipeline default() {
  let slots = [0, 0]
  let acc = {n: 0}
  parallel 2 { i ->
    slots[i] = i
    acc.n = acc.n + 1
  }
  log(len(slots))
}
",
        );
        assert_eq!(names, vec!["slots", "acc"]);
    }

    #[test]
    fn warns_on_spawn_body() {
        let names = flagged_names(
            r"
pipeline default() {
  let flag = 0
  scope {
    spawn {
      flag = 7
    }
  }
  log(flag)
}
",
        );
        assert_eq!(names, vec!["flag"]);
    }

    #[test]
    fn no_warning_on_body_local_accumulator() {
        // `local` is declared inside the branch, so mutating it is branch-local
        // and safe — it must not be flagged.
        let names = flagged_names(
            r"
pipeline default() {
  const parts = parallel each [1, 2, 3] { x ->
    let local = 0
    local = local + x
    return local
  }
  log(len(parts))
}
",
        );
        assert!(names.is_empty(), "unexpected warnings: {names:?}");
    }

    #[test]
    fn no_warning_on_readonly_capture() {
        // Reading a captured variable inside a branch is fine; only a write
        // races.
        let names = flagged_names(
            r"
pipeline default() {
  let base = 10
  const parts = parallel each [1, 2] { x ->
    return x + base
  }
  log(len(parts))
}
",
        );
        assert!(names.is_empty(), "unexpected warnings: {names:?}");
    }

    #[test]
    fn no_warning_outside_concurrency() {
        // The same capture-and-mutate pattern in a plain closure is legal
        // reference capture, not a race — the rule is scoped to concurrency.
        let names = flagged_names(
            r"
pipeline default() {
  let n = 0
  const inc = { -> n = n + 1 }
  inc()
  log(n)
}
",
        );
        assert!(names.is_empty(), "unexpected warnings: {names:?}");
    }
}
