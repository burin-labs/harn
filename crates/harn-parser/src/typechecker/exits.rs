//! Free helpers for "does this statement / block definitely exit?" analysis.
//!
//! Used both by `harn-lint` (publicly) and the type checker's flow narrowing
//! logic (via the same-named methods on `TypeChecker`, which delegate here).

use crate::ast::*;

/// Check whether a single statement definitely exits (return/throw/break/continue
/// or an if/else where both branches exit).
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
        _ => false,
    }
}

/// Check whether a block definitely exits (contains a terminating statement).
pub fn block_definitely_exits(stmts: &[SNode]) -> bool {
    stmts.iter().any(stmt_definitely_exits)
}
