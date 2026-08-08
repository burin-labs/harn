//! Block-tail type inference.

use crate::ast::{node_produces_value, SNode, TypeExpr};

use super::super::scope::{BlockTypeInference, TypeScope};
use super::super::TypeChecker;

impl TypeChecker {
    /// Infer a block tail without conflating gradual typing with `nil`.
    pub(in crate::typechecker) fn infer_block_type(
        &self,
        stmts: &[SNode],
        scope: &TypeScope,
    ) -> BlockTypeInference {
        if Self::block_definitely_exits(stmts) {
            return BlockTypeInference::Known(TypeExpr::Never);
        }
        let Some(last) = stmts.last() else {
            return BlockTypeInference::NoValue;
        };
        match self.infer_type(last, scope) {
            Some(ty) => BlockTypeInference::Known(ty),
            None if node_produces_value(&last.node) => BlockTypeInference::Unknown,
            None => BlockTypeInference::NoValue,
        }
    }
}
