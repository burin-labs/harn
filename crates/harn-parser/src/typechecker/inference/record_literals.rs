//! Closed record inference, retaining literal error tags when requested.

use crate::ast::{DictEntry, Node, ShapeField, TypeExpr};

use super::super::binary_ops::merge_shape_fields;
use super::super::scope::{InferredType, TypeScope};
use super::super::TypeChecker;

impl TypeChecker {
    pub(super) fn infer_record_type(
        &self,
        entries: &[DictEntry],
        scope: &TypeScope,
        literal_fields: bool,
    ) -> InferredType {
        // Fold entries with the same right-biased merge used by the VM.
        let mut fields = Vec::new();
        for entry in entries {
            if let Node::Spread(inner) = &entry.value.node {
                let inferred = if let Node::DictLiteral(entries) = &inner.node {
                    self.infer_record_type(entries, scope, literal_fields)
                } else {
                    self.infer_type(inner, scope)
                };
                match inferred {
                    Some(TypeExpr::Shape(spread_fields)) => {
                        fields = merge_shape_fields(&fields, &spread_fields);
                    }
                    _ => return Some(TypeExpr::Named("dict".into())),
                }
                continue;
            }
            let key = match &entry.key.node {
                Node::StringLiteral(key) | Node::Identifier(key) => key.clone(),
                _ => return Some(TypeExpr::Named("dict".into())),
            };
            let inferred = match &entry.value.node {
                Node::StringLiteral(value) | Node::RawStringLiteral(value) if literal_fields => {
                    Some(TypeExpr::LitString(value.clone()))
                }
                Node::IntLiteral(value) if literal_fields => Some(TypeExpr::LitInt(*value)),
                Node::DictLiteral(entries) if literal_fields => {
                    self.infer_record_type(entries, scope, true)
                }
                _ => self.infer_type(&entry.value, scope),
            };
            let value_type = inferred.unwrap_or_else(Self::wildcard_type);
            fields = merge_shape_fields(&fields, &[ShapeField::synthetic(key, value_type, false)]);
        }
        Some(TypeExpr::Shape(fields))
    }
}
