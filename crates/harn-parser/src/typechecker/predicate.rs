//! Static obligations of the registered predicate capability. Ordinary method
//! syntax preserves lexical capability resolution and existing editor tooling.

use super::{scope::TypeScope, TypeChecker};
use crate::{ast::*, builtin_signatures::TyExt, diagnostic_codes::Code};
use harn_lexer::Span;

/// A checked source site. Consumers hash the canonical type and question at
/// their artifact boundary; this record never contains runtime input values.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PredicateSite {
    pub id: String,
    pub question: String,
    pub input_type: TypeExpr,
    pub line: usize,
    pub column: usize,
    pub start: usize,
    pub end: usize,
    /// The declaration-time route, before catalog admission. An unknown route
    /// remains explicit so consumers cannot confuse no measurement with support.
    #[serde(default)]
    pub model_route: Option<PredicateModelRoute>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PredicateModelRoute {
    pub provider: String,
    pub model: String,
}

fn model_route(policy: &SNode, scope: &TypeScope) -> Option<PredicateModelRoute> {
    let crate::const_eval::ConstValue::Dict(fields) = scope.const_value(policy)? else {
        return None;
    };
    let string = |name: &str| {
        fields.iter().find_map(|(key, value)| {
            if key != name {
                return None;
            }
            match value {
                crate::const_eval::ConstValue::String(value) => Some(value.clone()),
                _ => None,
            }
        })
    };
    Some(PredicateModelRoute {
        provider: string("provider")?,
        model: string("model")?,
    })
}

/// A deterministic structural identity, independent of field/union ordering
/// and source spans. This is material for an artifact/cache digest, not a hash.
pub fn canonical_type(ty: &TypeExpr) -> String {
    fn normalize(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(fields) => {
                for (name, value) in fields {
                    normalize(value);
                    if matches!(name.as_str(), "Shape" | "Union" | "Intersection") {
                        if let serde_json::Value::Array(items) = value {
                            items.sort_by_key(serde_json::Value::to_string);
                        }
                    }
                }
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(normalize),
            _ => {}
        }
    }
    let mut value = serde_json::to_value(ty).expect("type expression serializes");
    normalize(&mut value);
    value.to_string()
}

fn serializable(ty: &TypeExpr) -> bool {
    match ty {
        TypeExpr::Named(name) => {
            matches!(name.as_str(), "string" | "bool" | "int" | "float" | "nil")
        }
        TypeExpr::LitString(_) | TypeExpr::LitInt(_) => true,
        TypeExpr::Shape(fields) => fields.iter().all(|field| serializable(&field.type_expr)),
        TypeExpr::List(item) => serializable(item),
        TypeExpr::Tuple(items) | TypeExpr::Union(items) => {
            !items.is_empty() && items.iter().all(serializable)
        }
        TypeExpr::DictType(key, value) => {
            matches!(key.as_ref(), TypeExpr::Named(name) if name == "string") && serializable(value)
        }
        _ => false,
    }
}

impl TypeChecker {
    fn predicate_method_name(method: &str) -> bool {
        crate::builtin_signatures::lookup_capability_method(
            harn_builtin_meta::CapabilityId::Llm,
            method,
        )
        .is_some_and(|signature| signature.name == harn_builtin_meta::predicate::EVALUATE.name)
    }

    fn is_predicate_method(&self, object: &SNode, method: &str, scope: &TypeScope) -> bool {
        if !Self::predicate_method_name(method) {
            return false;
        }
        let Some(ty) = self.infer_type(object, scope) else {
            return false;
        };
        let ty = self.resolve_alias(&ty, scope);
        let Some(TypeExpr::Named(name)) = super::union::without_nil(&ty) else {
            return false;
        };
        harn_builtin_meta::CapabilityId::from_type_name(&name)
            == Some(harn_builtin_meta::CapabilityId::Llm)
    }

    pub(super) fn check_predicate_node(&mut self, node: &SNode, scope: &TypeScope) {
        let projection = match &node.node {
            Node::PropertyAccess { object, property }
            | Node::OptionalPropertyAccess { object, property } => {
                Some((object, Some(property.as_str())))
            }
            Node::SubscriptAccess { object, index }
            | Node::OptionalSubscriptAccess { object, index } => {
                let field = match &index.node {
                    Node::StringLiteral(name) | Node::RawStringLiteral(name) => Some(name.as_str()),
                    _ => None,
                };
                Some((object, field))
            }
            _ => None,
        };
        if let Some((object, field)) = projection {
            if let Some(ty) = self.infer_type(object, scope) {
                self.check_predicate_field(&ty, field, node.span, scope);
            }
        }
        let named_receiver = match &node.node {
            Node::MethodCall { object, method, .. }
            | Node::OptionalMethodCall { object, method, .. } => Some((object, method)),
            Node::PropertyAccess { object, property }
            | Node::OptionalPropertyAccess { object, property } => Some((object, property)),
            _ => None,
        };
        if let Some((object, method)) = named_receiver {
            if Self::predicate_method_name(method)
                && self.infer_type(object, scope).is_none_or(|ty| {
                    matches!(self.resolve_alias(&ty, scope), TypeExpr::Named(name)
                        if matches!(name.as_str(), "any" | "unknown" | "dict" | "_"))
                })
            {
                self.error_at_with_help(
                    Code::PredicateSiteInvalid,
                    "predicate method receiver has no statically resolved type".into(),
                    node.span,
                    "retain HarnessLlm in the helper signature instead of erasing it to an unvalidated value".into(),
                );
            }
        }
        match &node.node {
            Node::IfElse { condition, .. }
            | Node::WhileLoop { condition, .. }
            | Node::GuardStmt { condition, .. }
            | Node::RequireStmt { condition, .. }
            | Node::Ternary { condition, .. } => self.check_predicate_boolean(condition, scope),
            Node::UnaryOp { op, operand } if op == "!" => {
                self.check_predicate_boolean(operand, scope);
            }
            Node::BinaryOp { op, left, right } if op == "&&" || op == "||" => {
                self.check_predicate_boolean(left, scope);
                self.check_predicate_boolean(right, scope);
            }
            Node::PropertyAccess { object, property }
            | Node::OptionalPropertyAccess { object, property }
                if self.is_predicate_method(object, property, scope) =>
            {
                self.error_at(Code::PredicateSiteInvalid,
                    "predicate evaluation cannot be captured as a function value; use a typed helper with a literal site".into(), node.span);
            }
            Node::OptionalMethodCall { object, method, .. }
                if self.is_predicate_method(object, method, scope) =>
            {
                self.error_at(Code::PredicateSiteInvalid,
                    "predicate evaluation requires an unconditional capability call; handle capability absence explicitly".into(), node.span);
            }
            _ => {}
        }
    }

    pub(super) fn check_predicate_call(&mut self, args: &[SNode], scope: &TypeScope, span: Span) {
        let [id, question, input, policy] = args else {
            return; // Ordinary signature checking owns arity.
        };
        let literal = |node: &SNode| match &node.node {
            Node::StringLiteral(text) | Node::RawStringLiteral(text) if !text.is_empty() => {
                Some(text.clone())
            }
            _ => None,
        };
        let (Some(id), Some(question)) = (literal(id), literal(question)) else {
            self.error_at(
                Code::PredicateSiteInvalid,
                "predicate id and question must be nonempty string literals".into(),
                span,
            );
            return;
        };
        let Some(input_type) = self.infer_type(input, scope) else {
            self.predicate_input_error(input.span);
            return;
        };
        let input_type = self.resolve_alias(&input_type, scope);
        if !serializable(&input_type) {
            self.predicate_input_error(input.span);
            return;
        }
        // Gradual typing ordinarily allows `any` at a typed call boundary.
        // Predicate admission must not accept an opaque policy that way.
        let policy_is_closed = self
            .infer_type(policy, scope)
            .is_some_and(|ty| serializable(&self.resolve_alias(&ty, scope)));
        if !policy_is_closed {
            self.error_at(
                Code::PredicateInputInvalid,
                "predicate policy must have a closed typed record".into(),
                policy.span,
            );
        }
        if self
            .predicate_sites
            .iter()
            .any(|site| site.id == id && (site.start != span.start || site.end != span.end))
        {
            self.error_at(
                Code::PredicateSiteInvalid,
                format!("predicate id `{id}` is declared by more than one source site"),
                span,
            );
            return;
        }
        if !self
            .predicate_sites
            .iter()
            .any(|site| site.start == span.start && site.end == span.end)
        {
            self.predicate_sites.push(PredicateSite {
                model_route: model_route(policy, scope),
                id,
                question,
                input_type,
                line: span.line,
                column: span.column,
                start: span.start,
                end: span.end,
            });
        }
    }

    fn predicate_input_error(&mut self, span: Span) {
        self.error_at(
            Code::PredicateInputInvalid,
            "predicate input must have a closed serializable type; functions, handles, open records and unvalidated values are not accepted".into(),
            span,
        );
    }

    pub(super) fn is_predicate_outcome(&self, ty: &TypeExpr, scope: &TypeScope) -> bool {
        let ty = self.resolve_alias(ty, scope);
        let Some(ty) = super::union::without_nil(&ty) else {
            return false;
        };
        let members = match &ty {
            TypeExpr::Union(members) => members.as_slice(),
            other => std::slice::from_ref(other),
        };
        if members.is_empty()
            || !members.iter().all(|member| {
                matches!(member, TypeExpr::Shape(fields) if fields.iter().any(|field| field.name == "receipt"))
            })
        {
            return false;
        }
        static VARIANTS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
        let variants = VARIANTS.get_or_init(|| {
            let TypeExpr::Union(variants) = harn_builtin_meta::predicate::OUTCOME.to_type_expr()
            else {
                unreachable!("predicate outcome is a closed union");
            };
            variants.iter().map(canonical_type).collect()
        });
        members
            .iter()
            .all(|member| variants.contains(&canonical_type(member)))
    }

    fn check_predicate_field(
        &mut self,
        ty: &TypeExpr,
        field: Option<&str>,
        span: Span,
        scope: &TypeScope,
    ) {
        if !self.is_predicate_outcome(ty, scope) {
            return;
        }
        let ty = self.resolve_alias(ty, scope);
        let Some(ty) = super::union::without_nil(&ty) else {
            return;
        };
        let members = match &ty {
            TypeExpr::Union(members) => members.as_slice(),
            other => std::slice::from_ref(other),
        };
        if field.is_some_and(|name| members.iter().all(|member| {
            matches!(member, TypeExpr::Shape(fields) if fields.iter().any(|field| field.name == name))
        })) { return; }
        self.error_at_with_help(
            Code::PredicateOutcomeUnnarrowed,
            "predicate variant field is not available on every remaining outcome".into(),
            span,
            "match outcome.kind before accessing a variant field; use a named field rather than a dynamic index".into(),
        );
    }

    pub(super) fn check_predicate_boolean(&mut self, node: &SNode, scope: &TypeScope) {
        if self
            .infer_type(node, scope)
            .is_some_and(|ty| self.is_predicate_outcome(&ty, scope))
        {
            self.error_at_with_help(
                Code::PredicateBooleanUse,
                "a predicate outcome is not a boolean".into(),
                node.span,
                "match outcome.kind, then branch on outcome.value.verdict only in the verdict arm"
                    .into(),
            );
        }
    }

    pub(super) fn record_predicate_binding(
        &mut self,
        pattern: &BindingPattern,
        inferred: Option<&TypeExpr>,
        span: Span,
        scope: &TypeScope,
    ) {
        if !inferred.is_some_and(|ty| self.is_predicate_outcome(ty, scope)) {
            return;
        }
        if let (BindingPattern::Dict(fields), Some(ty)) = (pattern, inferred) {
            for field in fields {
                if !field.is_rest {
                    self.check_predicate_field(ty, Some(&field.key), span, scope);
                }
            }
        }
        if let BindingPattern::Identifier(name) = pattern {
            if is_discard_name(name) {
                self.unused_predicate_error(span);
            } else {
                let binding = crate::lexical::BindingId {
                    name: name.clone(),
                    declaration_start: span.start,
                    declaration_end: span.end,
                };
                if !self
                    .predicate_bindings
                    .iter()
                    .any(|(existing, _)| *existing == binding)
                {
                    self.predicate_bindings.push((binding, span));
                }
            }
        }
    }

    pub(super) fn unused_predicate_error(&mut self, span: Span) {
        self.error_at_with_help(
            Code::PredicateOutcomeUnused,
            "predicate outcome is discarded without a disposition".into(),
            span,
            "match the outcome or pass it to a typed outcome policy".into(),
        );
    }

    pub(super) fn check_unused_predicate_bindings(&mut self, program: &[SNode]) {
        let patterns = crate::lexical::module_match_pattern_catalog_with_visible(
            program,
            &self.imported_type_decls,
        );
        let used = crate::lexical::resolved_identifier_bindings_with_source(
            &[],
            program,
            self.source.as_deref(),
            &patterns,
        );
        let unused: Vec<_> = self
            .predicate_bindings
            .iter()
            .filter(|(binding, _)| !used.values().any(|used| used == binding))
            .map(|(_, span)| *span)
            .collect();
        for span in unused {
            self.unused_predicate_error(span);
        }
    }
}
