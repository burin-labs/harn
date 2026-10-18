//! Self-contained namespace call contracts shared by graph and package compilation.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::{NamespaceImportBinding, Node, SNode, TypeExpr, TypePredicate};

const MAX_INLINE_DEPTH: usize = 16;

/// A namespace member's signature with defining-module aliases inlined.
#[derive(Debug, Clone, PartialEq)]
pub struct NamespaceMemberSignature {
    pub param_names: Vec<String>,
    /// Everything up to the first defaulted parameter is required.
    pub required_params: usize,
    pub fn_type: TypeExpr,
    pub type_predicate: Option<TypePredicate>,
}

impl NamespaceImportBinding {
    /// Build the checker's namespace projection from resolved member contracts.
    pub fn from_member_signatures(
        module_path: String,
        members: BTreeSet<String>,
        signatures: BTreeMap<String, NamespaceMemberSignature>,
    ) -> Self {
        let mut binding = Self {
            module_path,
            members,
            member_types: BTreeMap::new(),
            member_param_names: BTreeMap::new(),
            member_required_params: BTreeMap::new(),
            member_type_predicates: BTreeMap::new(),
        };
        for (name, signature) in signatures {
            binding.member_types.insert(name.clone(), signature.fn_type);
            binding
                .member_param_names
                .insert(name.clone(), signature.param_names);
            binding
                .member_required_params
                .insert(name.clone(), signature.required_params);
            if let Some(predicate) = signature.type_predicate {
                binding.member_type_predicates.insert(name, predicate);
            }
        }
        binding
    }
}

/// Lower a callable using the defining module's alias resolver.
///
/// Generic and rest signatures stay gradual. Unresolved or recursive aliases
/// also stay gradual rather than inventing a structural contract.
pub fn lower_callable_signature(
    declaration: &SNode,
    resolve_alias: impl Fn(&str) -> Option<SNode>,
) -> Option<NamespaceMemberSignature> {
    let inner = match &declaration.node {
        Node::AttributedDecl { inner, .. } => inner.as_ref(),
        _ => declaration,
    };
    let (params, return_type, type_predicate) = match &inner.node {
        Node::FnDecl {
            params,
            return_type,
            type_predicate,
            type_params,
            ..
        } if type_params.is_empty() => (params, return_type, type_predicate.as_ref()),
        Node::Pipeline {
            params,
            return_type,
            ..
        } => (params, return_type, None),
        _ => return None,
    };
    if params.iter().any(|param| param.rest) {
        return None;
    }
    let inline = |ty: &TypeExpr| inline_named_types(ty, &resolve_alias, &mut HashSet::new(), 0);
    let lowered = params
        .iter()
        .map(|param| match &param.type_expr {
            Some(ty) if param.default_value.is_none() => inline(ty),
            _ => TypeExpr::Named("any".into()),
        })
        .collect();
    let ret = return_type
        .as_ref()
        .map(inline)
        .unwrap_or(TypeExpr::Named("any".into()));
    let type_predicate = type_predicate.and_then(|predicate| {
        let type_expr = inline(&predicate.type_expr);
        (!contains_gradual_type(&type_expr)).then(|| TypePredicate {
            parameter: predicate.parameter.clone(),
            type_expr,
            one_sided: predicate.one_sided,
            span: predicate.span,
        })
    });
    Some(NamespaceMemberSignature {
        param_names: params.iter().map(|param| param.name.clone()).collect(),
        required_params: params
            .iter()
            .position(|param| param.default_value.is_some())
            .unwrap_or(params.len()),
        fn_type: TypeExpr::FnType {
            params: lowered,
            return_type: Box::new(ret),
        },
        type_predicate,
    })
}

fn is_builtin_type_name(name: &str) -> bool {
    matches!(
        name,
        "int"
            | "float"
            | "string"
            | "bool"
            | "nil"
            | "list"
            | "dict"
            | "set"
            | "closure"
            | "bytes"
            | "any"
            | "unknown"
            | "never"
            | "number"
            | "Harness"
            | "_"
    )
}

fn contains_gradual_type(ty: &TypeExpr) -> bool {
    match ty {
        TypeExpr::Named(name) => matches!(name.as_str(), "any" | "unknown" | "_"),
        TypeExpr::Union(items) | TypeExpr::Intersection(items) | TypeExpr::Tuple(items) => {
            items.iter().any(contains_gradual_type)
        }
        TypeExpr::Shape(fields) => fields
            .iter()
            .any(|field| contains_gradual_type(&field.type_expr)),
        TypeExpr::OpenShape { fields, rests } => {
            fields
                .iter()
                .any(|field| contains_gradual_type(&field.type_expr))
                || rests.iter().any(contains_gradual_type)
        }
        TypeExpr::List(inner)
        | TypeExpr::Iter(inner)
        | TypeExpr::Generator(inner)
        | TypeExpr::Stream(inner)
        | TypeExpr::Owned(inner) => contains_gradual_type(inner),
        TypeExpr::DictType(key, value) => {
            contains_gradual_type(key) || contains_gradual_type(value)
        }
        TypeExpr::Applied { args, .. } => args.iter().any(contains_gradual_type),
        TypeExpr::FnType {
            params,
            return_type,
        } => params.iter().any(contains_gradual_type) || contains_gradual_type(return_type),
        TypeExpr::Never | TypeExpr::LitString(_) | TypeExpr::LitInt(_) => false,
    }
}

fn inline_named_types(
    ty: &TypeExpr,
    resolve_alias: &impl Fn(&str) -> Option<SNode>,
    visited: &mut HashSet<String>,
    depth: usize,
) -> TypeExpr {
    let recurse = |inner: &TypeExpr, visited: &mut HashSet<String>| {
        inline_named_types(inner, resolve_alias, visited, depth + 1)
    };
    match ty {
        TypeExpr::Named(name) => {
            if is_builtin_type_name(name) {
                return ty.clone();
            }
            if depth >= MAX_INLINE_DEPTH || !visited.insert(name.clone()) {
                return TypeExpr::Named("any".into());
            }
            let resolved = resolve_alias(name);
            let body = match resolved.as_ref().map(|decl| &decl.node) {
                Some(Node::TypeDecl {
                    type_params,
                    type_expr,
                    ..
                }) if type_params.is_empty() => {
                    inline_named_types(type_expr, resolve_alias, visited, depth + 1)
                }
                _ => TypeExpr::Named("any".into()),
            };
            visited.remove(name);
            body
        }
        TypeExpr::Union(items) => {
            TypeExpr::Union(items.iter().map(|item| recurse(item, visited)).collect())
        }
        TypeExpr::Intersection(items) => {
            TypeExpr::Intersection(items.iter().map(|item| recurse(item, visited)).collect())
        }
        TypeExpr::Shape(fields) => TypeExpr::Shape(
            fields
                .iter()
                .map(|field| {
                    let mut next = field.clone();
                    next.type_expr = recurse(&field.type_expr, visited);
                    next
                })
                .collect(),
        ),
        TypeExpr::List(inner) => TypeExpr::List(Box::new(recurse(inner, visited))),
        TypeExpr::Iter(inner) => TypeExpr::Iter(Box::new(recurse(inner, visited))),
        TypeExpr::Owned(inner) => TypeExpr::Owned(Box::new(recurse(inner, visited))),
        TypeExpr::Tuple(items) => {
            TypeExpr::Tuple(items.iter().map(|item| recurse(item, visited)).collect())
        }
        TypeExpr::DictType(key, value) => TypeExpr::DictType(
            Box::new(recurse(key, visited)),
            Box::new(recurse(value, visited)),
        ),
        TypeExpr::OpenShape { .. }
        | TypeExpr::Generator(_)
        | TypeExpr::Stream(_)
        | TypeExpr::Applied { .. }
        | TypeExpr::FnType { .. } => TypeExpr::Named("any".into()),
        TypeExpr::Never | TypeExpr::LitString(_) | TypeExpr::LitInt(_) => ty.clone(),
    }
}
