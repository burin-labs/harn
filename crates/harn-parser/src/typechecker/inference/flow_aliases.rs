//! Immutable aliases used by flow-sensitive condition analysis.

use std::collections::HashSet;

use crate::ast::{Node, SNode};

use super::super::scope::TypeScope;
use super::super::TypeChecker;

impl TypeChecker {
    /// Resolve an immutable expression alias at a condition leaf. The visited
    /// set is defensive; source-order const binding already prevents a
    /// user-written cycle.
    pub(in crate::typechecker) fn resolve_flow_alias_node(
        &self,
        node: &SNode,
        scope: &TypeScope,
    ) -> (SNode, bool) {
        let mut resolved = node.clone();
        let mut used_alias = false;
        let mut visited = HashSet::new();
        while let Node::Identifier(name) = &resolved.node {
            let name = name.clone();
            if !visited.insert(name.clone()) {
                break;
            }
            let Some(expression) = scope.get_flow_alias(&name) else {
                break;
            };
            resolved = expression.clone();
            used_alias = true;
        }
        (resolved, used_alias)
    }

    /// Resolve only aliases that preserve a `type_of` discriminant. A normal
    /// const still owns its value: `const result = lookup(); result != nil`
    /// must narrow `result`, not replace it with the call expression. In
    /// contrast, `const kind = type_of(value); kind == "string"` carries a
    /// stable fact about `value` and may expose that `type_of` call here.
    pub(in crate::typechecker) fn resolve_typeof_alias_node(
        &self,
        node: &SNode,
        scope: &TypeScope,
    ) -> (SNode, bool) {
        let original = node.clone();
        let mut resolved = node.clone();
        let mut visited = HashSet::new();
        while let Node::Identifier(name) = &resolved.node {
            let name = name.clone();
            if !visited.insert(name.clone()) {
                return (original, false);
            }
            let Some(expression) = scope.get_flow_alias(&name) else {
                return (original, false);
            };
            resolved = expression.clone();
        }
        if matches!(
            &resolved.node,
            Node::FunctionCall { name, .. } if name == "type_of"
        ) {
            (resolved, true)
        } else {
            (original, false)
        }
    }
}
