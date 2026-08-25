//! Exported call signatures for `import * as alias from "..."` members.
//!
//! A namespace member used to reach the type checker as `any`, so
//! `alias.member(...)` was the one call form nothing checked: not its argument
//! types, not its required arity. The same call written as a named import was
//! checked normally, and the gap was identical for a local module and a
//! package — the import *form* was the variable, not the boundary (#6172).
//!
//! Signatures are lowered to a self-contained [`TypeExpr::FnType`] here rather
//! than handed over as declarations, because a namespace import deliberately
//! does not flatten the target's type names into the consumer. Every named
//! type in a parameter position is resolved against the *defining* module and
//! inlined structurally, so the consumer never has to have `Request` in scope
//! and a consumer type of the same name cannot collide with it.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use harn_parser::{Node, SNode, TypeExpr, TypePredicate, TypedParam};

use crate::{normalize_path, ModuleGraph};

/// Depth cap for inlining a named type into a parameter position.
///
/// A mutually recursive alias pair (`A = {next: B}`, `B = {next: A}`) has no
/// finite structural expansion. The visited set already breaks a direct cycle;
/// this bounds the pathological indirect case so lowering always terminates.
const MAX_INLINE_DEPTH: usize = 16;

/// One namespace member's lowered call signature.
///
/// `param_names` travels beside the `FnType` because `TypeExpr::FnType` is
/// positional only. Without it a mismatch reports `argument 2 \`arg2\``, while
/// the same call through a named import reports `argument 2 \`request\`` — the
/// name is what tells an author which parameter the signature change moved.
#[derive(Debug, Clone, PartialEq)]
pub struct NamespaceMemberSignature {
    pub param_names: Vec<String>,
    /// Arguments that must be supplied. Mirrors the checker's own rule for a
    /// declared `fn`: everything up to the first parameter with a default.
    /// Deriving it from the parameter count instead would reject every
    /// legitimate call that omits a defaulted tail.
    pub required_params: usize,
    pub fn_type: TypeExpr,
    /// Caller-side narrowing contract with module-local types inlined.
    pub type_predicate: Option<TypePredicate>,
}

/// Names the checker resolves on its own. Inlining must not rewrite these.
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

impl ModuleGraph {
    /// Exported call signatures for the members of one namespace import.
    ///
    /// Only `fn` and `pipeline` members get a signature; a `tool` has no
    /// statically checkable parameter list, and a non-callable export is not a
    /// call target. A member with no entry keeps its previous `any` treatment,
    /// which is what keeps this change incapable of rejecting a program the
    /// checker used to accept for reasons it cannot actually see.
    pub(crate) fn namespace_member_signatures(
        &self,
        module_path: &Path,
        member_names: &[String],
    ) -> BTreeMap<String, NamespaceMemberSignature> {
        let mut out = BTreeMap::new();
        for name in member_names {
            let mut visited = HashSet::new();
            let Some(decl) = self.find_exported_callable_decl(module_path, name, &mut visited)
            else {
                continue;
            };
            // Resolve named types against the module that DEFINES the member,
            // not the re-exporting one: a signature forwarded through a barrel
            // module names types the barrel never declared.
            let origin = self
                .export_definition_of(module_path, name)
                .map_or_else(|| normalize_path(module_path), |site| site.file);
            if let Some(signature) = self.lower_callable_signature(&origin, &decl) {
                out.insert(name.clone(), signature);
            }
        }
        out
    }

    fn lower_callable_signature(
        &self,
        origin: &Path,
        decl: &SNode,
    ) -> Option<NamespaceMemberSignature> {
        let inner = match &decl.node {
            Node::AttributedDecl { inner, .. } => inner.as_ref(),
            _ => decl,
        };
        let (params, return_type, type_predicate) = match &inner.node {
            Node::FnDecl {
                params,
                return_type,
                type_predicate,
                type_params,
                ..
            } => {
                // A generic signature would need the checker's inference to
                // bind its type parameters; lowering it to a fixed `FnType`
                // would report a mismatch against an unbound name. Leave
                // generics on the old gradual path rather than guess.
                if !type_params.is_empty() {
                    return None;
                }
                (params, return_type, type_predicate.as_ref())
            }
            Node::Pipeline {
                params,
                return_type,
                ..
            } => (params, return_type, None),
            _ => return None,
        };
        // A rest parameter accepts any tail, so a fixed positional `FnType`
        // would misdescribe it.
        if params.iter().any(|param| param.rest) {
            return None;
        }
        let lowered: Vec<TypeExpr> = params
            .iter()
            .map(|param| self.lower_param_type(origin, param))
            .collect();
        let ret = return_type
            .as_ref()
            .map(|ty| self.inline_named_types(origin, ty, &mut HashSet::new(), 0))
            .unwrap_or(TypeExpr::Named("any".into()));
        let type_predicate = type_predicate.and_then(|predicate| {
            let type_expr =
                self.inline_named_types(origin, &predicate.type_expr, &mut HashSet::new(), 0);
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

    /// A parameter with a default is optional at the call site. `FnType` has no
    /// optionality, and `required_params` is derived from its length, so a
    /// defaulted parameter must not tighten the required count.
    fn lower_param_type(&self, origin: &Path, param: &TypedParam) -> TypeExpr {
        let Some(declared) = &param.type_expr else {
            return TypeExpr::Named("any".into());
        };
        if param.default_value.is_some() {
            return TypeExpr::Named("any".into());
        }
        self.inline_named_types(origin, declared, &mut HashSet::new(), 0)
    }

    /// Replace every module-local named type with its structural body.
    ///
    /// A name that cannot be resolved to a plain type alias in `origin` —
    /// a struct, enum, interface, generic parameter, or a name from a module
    /// this walk cannot see — becomes `any`. That is deliberate: an
    /// unresolvable `Named` would be compared structurally against the
    /// argument and could reject a correct program, and a false positive in
    /// `harn check` is worse than the gap this closes.
    fn inline_named_types(
        &self,
        origin: &Path,
        ty: &TypeExpr,
        visited: &mut HashSet<String>,
        depth: usize,
    ) -> TypeExpr {
        let recurse = |graph: &Self, inner: &TypeExpr, visited: &mut HashSet<String>| {
            graph.inline_named_types(origin, inner, visited, depth + 1)
        };
        match ty {
            TypeExpr::Named(name) => {
                if is_builtin_type_name(name) {
                    return ty.clone();
                }
                if depth >= MAX_INLINE_DEPTH || !visited.insert(name.clone()) {
                    return TypeExpr::Named("any".into());
                }
                let resolved = self
                    .find_exported_type_decl(origin, name, &mut HashSet::new())
                    .or_else(|| self.local_type_decl(origin, name));
                let body = match resolved.as_ref().map(|decl| &decl.node) {
                    Some(Node::TypeDecl {
                        type_params,
                        type_expr,
                        ..
                    }) if type_params.is_empty() => {
                        self.inline_named_types(origin, type_expr, visited, depth + 1)
                    }
                    _ => TypeExpr::Named("any".into()),
                };
                visited.remove(name);
                body
            }
            TypeExpr::Union(items) => TypeExpr::Union(
                items
                    .iter()
                    .map(|item| recurse(self, item, visited))
                    .collect(),
            ),
            TypeExpr::Intersection(items) => TypeExpr::Intersection(
                items
                    .iter()
                    .map(|item| recurse(self, item, visited))
                    .collect(),
            ),
            TypeExpr::Shape(fields) => TypeExpr::Shape(
                fields
                    .iter()
                    .map(|field| {
                        let mut next = field.clone();
                        next.type_expr = recurse(self, &field.type_expr, visited);
                        next
                    })
                    .collect(),
            ),
            TypeExpr::List(inner) => TypeExpr::List(Box::new(recurse(self, inner, visited))),
            TypeExpr::Iter(inner) => TypeExpr::Iter(Box::new(recurse(self, inner, visited))),
            TypeExpr::Owned(inner) => TypeExpr::Owned(Box::new(recurse(self, inner, visited))),
            TypeExpr::Tuple(items) => TypeExpr::Tuple(
                items
                    .iter()
                    .map(|item| recurse(self, item, visited))
                    .collect(),
            ),
            TypeExpr::DictType(key, value) => TypeExpr::DictType(
                Box::new(recurse(self, key, visited)),
                Box::new(recurse(self, value, visited)),
            ),
            // An open shape's row tail, a generator/stream payload, an applied
            // generic, and a function-typed parameter all carry inference
            // obligations that a structural inline cannot preserve. Leave the
            // whole parameter gradual rather than lower it wrongly.
            TypeExpr::OpenShape { .. }
            | TypeExpr::Generator(_)
            | TypeExpr::Stream(_)
            | TypeExpr::Applied { .. }
            | TypeExpr::FnType { .. } => TypeExpr::Named("any".into()),
            TypeExpr::Never | TypeExpr::LitString(_) | TypeExpr::LitInt(_) => ty.clone(),
        }
    }

    fn local_type_decl(&self, module_path: &Path, name: &str) -> Option<SNode> {
        let module = self
            .modules
            .get(module_path)
            .or_else(|| self.modules.get(&normalize_path(module_path)))?;
        module
            .type_declarations
            .iter()
            .find(|decl| crate::type_decl_name(decl) == Some(name))
            .cloned()
    }
}
