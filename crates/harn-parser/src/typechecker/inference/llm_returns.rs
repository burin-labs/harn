//! Return types the `llm` capability narrows beyond its declared signature.
//!
//! Two narrowings live here and nowhere else. A `call` or `completion` with an
//! `output` schema promises a typed `data` field, and a batched `evaluate`
//! types every answer from its own question. Both the ambient `llm_call` form
//! and the `harness.llm.*` capability method resolve through this one module
//! so they cannot disagree about either.

use crate::ast::*;
use crate::builtin_signatures;
use crate::builtin_signatures::TyExt;

use super::super::schema_inference::{
    output_schema_type_expr_from_node, output_validation_is_required,
};
use super::super::scope::{builtin_return_type, InferredType, TypeScope};
use super::super::TypeChecker;

impl TypeChecker {
    pub(in crate::typechecker) fn infer_llm_call_result_type(
        &self,
        name: &str,
        args: &[SNode],
        scope: &TypeScope,
    ) -> InferredType {
        let data = self.llm_call_schema_data_type(args, scope)?;
        Some(Self::narrow_schema_data_field(
            builtin_return_type(name)?,
            data,
        ))
    }

    /// Replace the envelope's `data` field with the type an `output` schema
    /// promises. Both the ambient `llm_call` form and the `harness.llm.*`
    /// capability method resolve through here so they cannot disagree.
    fn narrow_schema_data_field(
        mut result: TypeExpr,
        (data_type, data_required): (TypeExpr, bool),
    ) -> TypeExpr {
        let TypeExpr::Shape(fields) = &mut result else {
            return result;
        };
        if let Some(field) = fields.iter_mut().find(|field| field.name == "data") {
            field.type_expr = data_type;
            field.optional = !data_required;
        }
        result
    }

    fn llm_call_schema_data_type(
        &self,
        args: &[SNode],
        scope: &TypeScope,
    ) -> Option<(TypeExpr, bool)> {
        let opts = args.get(2)?;
        let Node::DictLiteral(entries) = &opts.node else {
            return None;
        };
        let mut data_type = None;
        let mut data_required = false;
        for entry in entries {
            let key = match &entry.key.node {
                Node::StringLiteral(key) | Node::Identifier(key) => key.as_str(),
                _ => continue,
            };
            if key == "output" {
                data_type = output_schema_type_expr_from_node(&entry.value, scope);
                data_required = output_validation_is_required(&entry.value);
            }
        }
        data_type.map(|ty| (ty, data_required))
    }

    pub(in crate::typechecker) fn harness_method_return_type(
        &self,
        receiver: &TypeExpr,
        method: &str,
        args: &[SNode],
        scope: &TypeScope,
    ) -> InferredType {
        let receiver = self.resolve_alias(receiver, scope);
        match receiver {
            TypeExpr::Named(name) => {
                let capability = harn_builtin_meta::CapabilityId::from_type_name(name.as_str())?;
                let sig = builtin_signatures::lookup_capability_method(capability, method)?;
                let declared = (!sig.returns.is_any()).then(|| sig.returns.to_type_expr())?;
                if capability != harn_builtin_meta::CapabilityId::Llm {
                    return Some(declared);
                }
                // A batched evaluation types every answer from its own
                // question, so a choice answer's label is the literal union of
                // that question's criteria keys. An unreadable question set
                // keeps the declared answer map; the site check reports why.
                if sig.name == harn_builtin_meta::predicate::EVALUATE.name {
                    let Some(answers) = self.evaluation_answer_record(args, scope) else {
                        return Some(declared);
                    };
                    return Some(Self::narrow_evaluation_answers(declared, answers));
                }
                if !matches!(method, "call" | "completion") {
                    return Some(declared);
                }
                let Some(data) = self.llm_call_schema_data_type(args, scope) else {
                    return Some(declared);
                };
                Some(Self::narrow_schema_data_field(declared, data))
            }
            _ => None,
        }
    }
}
