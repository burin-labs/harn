use harn_lexer::{FixEdit, Lexer, TokenKind};

use super::*;

impl TypeChecker {
    pub(in crate::typechecker) fn check_prefer_pick(&mut self, node: &SNode, scope: &TypeScope) {
        let Node::DictLiteral(entries) = &node.node else {
            return;
        };
        if entries.len() < 2
            || scope.get_var_before_fn("pick").is_some()
            || scope.get_fn("pick").is_some()
            || self.name_is_imported("pick")
        {
            return;
        }
        let mut source_path = None;
        let mut source_type = None;
        let mut keys = Vec::with_capacity(entries.len());
        for entry in entries {
            let key = match &entry.key.node {
                Node::Identifier(key) | Node::StringLiteral(key) | Node::RawStringLiteral(key) => {
                    key
                }
                _ => return,
            };
            let Node::PropertyAccess { object, property } = &entry.value.node else {
                return;
            };
            if property != key {
                return;
            }
            let Some(path) = self.pick_source_path(object, scope) else {
                return;
            };
            if let Some(first) = &source_path {
                if first != &path {
                    return;
                }
            } else {
                source_type = self.infer_type(object, scope);
                source_path = Some(path);
            }
            keys.push(key.clone());
        }
        let (Some(source_path), Some(source_type)) = (source_path, source_type) else {
            return;
        };
        if !self.picked_fields_are_present(&source_type, &keys, scope) {
            return;
        }
        let Some(literal) = self
            .source
            .as_deref()
            .and_then(|source| source.get(node.span.start..node.span.end))
        else {
            return;
        };
        // Replacing the whole literal must not erase comments attached to fields.
        let Ok(tokens) = Lexer::new(literal).tokenize_with_comments() else {
            return;
        };
        if tokens.iter().any(|token| {
            matches!(
                token.kind,
                TokenKind::LineComment { .. } | TokenKind::BlockComment { .. }
            )
        }) {
            return;
        }
        let fields = keys
            .iter()
            .map(|key| serde_json::to_string(key).expect("field names serialize as strings"))
            .collect::<Vec<_>>()
            .join(", ");
        let replacement = format!("pick({source_path}, [{fields}])");
        self.lint_warning_at_with_fix(
            Code::LintPreferPick,
            "prefer-pick",
            format!(
                "record literal copies {} fields from `{source_path}` one by one",
                keys.len()
            ),
            node.span,
            format!("replace it with `{replacement}`"),
            vec![FixEdit {
                span: node.span,
                replacement,
            }],
        );
    }

    /// Repeated field reads can collapse to one evaluation only when the path
    /// has no calls and every intermediate field is present.
    fn pick_source_path(&self, node: &SNode, scope: &TypeScope) -> Option<String> {
        match &node.node {
            Node::Identifier(name) => Some(name.clone()),
            Node::PropertyAccess { object, property } => {
                let path = self.pick_source_path(object, scope)?;
                let receiver = self.infer_type(object, scope)?;
                self.picked_fields_are_present(&receiver, std::slice::from_ref(property), scope)
                    .then(|| format!("{path}.{property}"))
            }
            _ => None,
        }
    }

    fn picked_fields_are_present(&self, ty: &TypeExpr, keys: &[String], scope: &TypeScope) -> bool {
        let resolved = self.resolve_alias(ty, scope);
        if let TypeExpr::Union(members) = &resolved {
            return !members.is_empty()
                && members
                    .iter()
                    .all(|member| self.picked_fields_are_present(member, keys, scope));
        }
        let Ok(record) = self.projection_fields(&resolved, scope) else {
            return false;
        };
        keys.iter().all(|key| {
            record
                .fields
                .iter()
                .any(|field| field.name == *key && !field.optional)
        })
    }
}
