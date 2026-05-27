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
        Node::Block(body)
        | Node::TryExpr { body }
        | Node::CostRoute { body, .. }
        | Node::MutexBlock { body }
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
