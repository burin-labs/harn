use std::collections::HashMap;

use harn_parser::{Node, SNode, TypeExpr};

use super::Linter;

impl Linter<'_> {
    pub(super) fn push_scope(&mut self) {
        self.scopes.push(Default::default());
        self.typed_scopes.push(HashMap::new());
    }

    pub(super) fn pop_scope(&mut self) {
        self.scopes.pop();
        self.typed_scopes.pop();
    }

    pub(crate) fn install_binding_types(
        &mut self,
        bindings: impl IntoIterator<Item = harn_parser::BindingTypeInfo>,
    ) {
        self.binding_types.extend(
            bindings
                .into_iter()
                .map(|binding| ((binding.span.start, binding.span.end), binding.type_expr)),
        );
    }

    pub(super) fn expression_is_non_optional_bool(&self, expression: &SNode) -> bool {
        match &expression.node {
            Node::BoolLiteral(_) => true,
            Node::Identifier(name) => self
                .typed_scopes
                .iter()
                .rev()
                .find_map(|scope| scope.get(name))
                .is_some_and(
                    |type_expr| matches!(type_expr, TypeExpr::Named(name) if name == "bool"),
                ),
            _ => false,
        }
    }
}
