//! Free helpers for "does this statement / block definitely exit?" analysis.
//!
//! Used both by `harn-lint` (publicly) and the type checker's flow narrowing
//! logic (via the same-named methods on `TypeChecker`, which delegate here).

use crate::ast::*;

/// Check whether a single statement definitely exits (return/throw/break/continue
/// or an if/else / match where every reachable branch exits).
pub fn stmt_definitely_exits(stmt: &SNode) -> bool {
    match &stmt.node {
        Node::ReturnStmt { .. } | Node::ThrowStmt { .. } | Node::BreakStmt | Node::ContinueStmt => {
            true
        }
        Node::IfElse {
            then_body,
            else_body: Some(else_body),
            ..
        } => block_definitely_exits(then_body) && block_definitely_exits(else_body),
        // A `match` definitely exits when every arm exits AND at least
        // one arm is an unguarded wildcard (`_ -> { ... }`) — that
        // guarantees the match is exhaustive at the source level
        // without re-deriving the value's type here. The lint and flow
        // narrowing layers both rely on this to flag code after a
        // returning `match` as unreachable, and to let the type
        // checker treat the tail as `never`.
        Node::MatchExpr { arms, .. } => match_definitely_exits(arms),
        // `while true { ... }` with no `break` binding to it never proceeds
        // past the loop: control either stays inside forever or leaves the
        // function via `return`/`throw` from the body. Treat it as diverging
        // so a function whose tail is such a loop needs no unreachable
        // trailing `return`, and code after it is flagged unreachable. Any
        // `break` at this loop's level restores fall-through (breaks inside
        // nested loops bind the inner loop and do not count).
        Node::WhileLoop { condition, body } => {
            matches!(condition.node, Node::BoolLiteral(true)) && !body_breaks_enclosing_loop(body)
        }
        Node::Block(body)
        | Node::TryExpr { body }
        | Node::CostRoute { body, .. }
        | Node::MutexBlock { body, .. }
        | Node::DeadlineBlock { body, .. }
        | Node::Retry { body, .. } => block_definitely_exits(body),
        Node::TryCatch {
            body,
            catch_body,
            finally_body,
            ..
        } => {
            finally_body
                .as_ref()
                .is_some_and(|body| block_definitely_exits(body))
                || (block_definitely_exits(body) && block_definitely_exits(catch_body))
        }
        _ => false,
    }
}

fn match_definitely_exits(arms: &[MatchArm]) -> bool {
    if arms.is_empty() {
        return false;
    }
    let has_unguarded_wildcard = arms.iter().any(|arm| {
        arm.guard.is_none() && matches!(&arm.pattern.node, Node::Identifier(name) if name == "_")
    });
    has_unguarded_wildcard && arms.iter().all(|arm| block_definitely_exits(&arm.body))
}

/// Check whether a block definitely exits (contains a terminating statement).
pub fn block_definitely_exits(stmts: &[SNode]) -> bool {
    stmts.iter().any(stmt_definitely_exits)
}

/// Whether `body` contains a `break` that binds to the enclosing loop — i.e.
/// a `break` not nested inside a further `while`/`for` loop. Every other
/// construct (if/match/try/blocks/closures embedded in expressions, ...) is
/// scanned through via the shared [`crate::visit`] child collector, so the
/// scan is deliberately conservative: an unexpected `break` can only make the
/// caller treat the loop as breakable (demanding an explicit trailing
/// `return`), never the unsound reverse.
fn body_breaks_enclosing_loop(body: &[SNode]) -> bool {
    body.iter().any(stmt_breaks_enclosing_loop)
}

fn stmt_breaks_enclosing_loop(stmt: &SNode) -> bool {
    match &stmt.node {
        Node::BreakStmt => true,
        // Nested loops capture `break`s in their own bodies.
        Node::WhileLoop { .. } | Node::ForIn { .. } => false,
        _ => crate::visit::immediate_children(stmt)
            .into_iter()
            .any(stmt_breaks_enclosing_loop),
    }
}
