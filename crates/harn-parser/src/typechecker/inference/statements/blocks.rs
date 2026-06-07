use super::*;

impl TypeChecker {
    pub(in crate::typechecker) fn check_block(&mut self, stmts: &[SNode], scope: &mut TypeScope) {
        self.check_block_with_expected_tail(stmts, None, scope);
    }

    pub(in crate::typechecker) fn check_block_with_expected_tail(
        &mut self,
        stmts: &[SNode],
        expected_tail: Option<&TypeExpr>,
        scope: &mut TypeScope,
    ) {
        let mut definitely_exited = false;
        for (idx, stmt) in stmts.iter().enumerate() {
            if definitely_exited {
                self.warning_at(
                    Code::UnreachableCode,
                    "unreachable code".to_string(),
                    stmt.span,
                );
                break; // warn once per block
            }
            if idx + 1 == stmts.len() && !matches!(stmt.node, Node::ReturnStmt { .. }) {
                self.check_node_with_expected(stmt, expected_tail, scope);
            } else {
                self.check_node(stmt, scope);
            }
            if Self::stmt_definitely_exits(stmt) {
                definitely_exited = true;
            }
        }
    }

    pub(in crate::typechecker) fn check_node_with_expected(
        &mut self,
        snode: &SNode,
        expected: Option<&TypeExpr>,
        scope: &mut TypeScope,
    ) -> bool {
        let Some(expected) = expected else {
            self.check_node(snode, scope);
            return false;
        };
        if self.check_contextual_closure(snode, expected, scope) {
            return true;
        }
        if self.check_compound_node_with_expected(snode, expected, scope) {
            return false;
        }
        self.check_node(snode, scope);
        false
    }
}
