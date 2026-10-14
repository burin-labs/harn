//! Builtin value-method typing, derived from the method registry
//! ([`crate::builtin_methods`]). A method's result type is its registry
//! signature with the receiver's element/key/value types and the types its
//! callback arguments return substituted in.

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::{SNode, TypeExpr};
use crate::builtin_methods::{self as methods, MethodDescriptor, MethodReceiver};
use crate::builtin_signatures::TyExt;

use super::super::scope::{InferredType, TypeScope};
use super::super::TypeChecker;

impl TypeChecker {
    /// The builtin receiver kind of a resolved type, with the generic
    /// parameters the receiver itself binds (`T` for a list's element, `K`
    /// and `V` for a dict's key and value).
    pub(in crate::typechecker) fn builtin_method_receiver(
        ty: &TypeExpr,
    ) -> Option<(MethodReceiver, BTreeMap<String, TypeExpr>)> {
        let element = |t: &TypeExpr| BTreeMap::from([("T".to_string(), t.clone())]);
        let unbound = BTreeMap::new;
        Some(match ty {
            TypeExpr::List(t) => (MethodReceiver::List, element(t)),
            TypeExpr::Tuple(items) => (
                MethodReceiver::List,
                element(&Self::tuple_element_type(items)),
            ),
            TypeExpr::Iter(t) => (MethodReceiver::Iter, element(t)),
            TypeExpr::Generator(t) | TypeExpr::Stream(t) => (MethodReceiver::Generator, element(t)),
            TypeExpr::DictType(k, v) => (
                MethodReceiver::Dict,
                BTreeMap::from([
                    ("K".to_string(), (**k).clone()),
                    ("V".to_string(), (**v).clone()),
                ]),
            ),
            TypeExpr::Applied { name, args } if name == "set" && args.len() == 1 => {
                (MethodReceiver::Set, element(&args[0]))
            }
            TypeExpr::LitString(_) => (MethodReceiver::String, unbound()),
            TypeExpr::Named(name) => match name.as_str() {
                "string" => (MethodReceiver::String, unbound()),
                "list" => (MethodReceiver::List, unbound()),
                "set" => (MethodReceiver::Set, unbound()),
                "dict" => (MethodReceiver::Dict, unbound()),
                "range" => (MethodReceiver::Range, unbound()),
                "iter" => (MethodReceiver::Iter, unbound()),
                _ => return None,
            },
            _ => return None,
        })
    }

    /// The builtin receiver type a method call dispatches on: `T` for an
    /// optional-chained `T | nil`, and a plain `dict` for a record literal
    /// shape unless the shape carries a field named like the method (a
    /// callable member the call invokes instead).
    pub(in crate::typechecker) fn builtin_method_receiver_type(
        ty: &TypeExpr,
        method: &str,
    ) -> Option<TypeExpr> {
        let receiver = match ty {
            TypeExpr::Union(members) => {
                let non_nil: Vec<&TypeExpr> = members
                    .iter()
                    .filter(|member| !matches!(member, TypeExpr::Named(n) if n == "nil"))
                    .collect();
                if let [only] = non_nil.as_slice() {
                    return Self::builtin_method_receiver_type(only, method);
                }
                // `list<never> | list<string>` (an `if` choosing `[]` or a
                // literal) is still a list: use the kind every member shares.
                let kinds: Vec<Option<MethodReceiver>> = non_nil
                    .iter()
                    .map(|member| {
                        Self::builtin_method_receiver_type(member, method)
                            .and_then(|ty| Self::builtin_method_receiver(&ty).map(|(kind, _)| kind))
                    })
                    .collect();
                let first = (*kinds.first()?)?;
                if kinds.iter().all(|kind| *kind == Some(first)) {
                    return Some(TypeExpr::Named(first.type_name().into()));
                }
                return None;
            }
            TypeExpr::Shape(fields) | TypeExpr::OpenShape { fields, .. } => {
                if fields.iter().any(|field| field.name == method) {
                    return None;
                }
                TypeExpr::Named("dict".into())
            }
            other => other.clone(),
        };
        Self::builtin_method_receiver(&receiver).map(|_| receiver)
    }

    /// The registry row a call resolves to. Ranges lift any method they do
    /// not define onto their iterator.
    pub(in crate::typechecker) fn builtin_method_row(
        receiver: MethodReceiver,
        method: &str,
        bindings: &mut BTreeMap<String, TypeExpr>,
    ) -> Option<&'static MethodDescriptor> {
        methods::method(receiver, method).or_else(|| {
            if receiver != MethodReceiver::Range {
                return None;
            }
            bindings.insert("T".to_string(), TypeExpr::Named("int".into()));
            methods::method(MethodReceiver::Iter, method)
        })
    }

    /// Result type of `receiver.method(args)` on a builtin receiver, or `None`
    /// when the receiver is not a builtin value or the method is unknown.
    pub(in crate::typechecker) fn builtin_method_return_type(
        &self,
        receiver: &TypeExpr,
        method: &str,
        args: &[SNode],
        scope: &TypeScope,
    ) -> InferredType {
        let (kind, mut bindings) = Self::builtin_method_receiver(receiver)?;
        let row = Self::builtin_method_row(kind, method, &mut bindings)?;
        self.bind_method_arguments(row, args, &mut bindings, scope);
        Self::method_result_type(row, &bindings)
    }

    /// Result type of `value.method(args)` when the receiver's type is
    /// unknown: the type every builtin receiver's `method` agrees on, so
    /// `x.lower()` is a `string` and `x.len()` an `int` even through an
    /// untyped binding.
    pub(in crate::typechecker) fn gradual_builtin_method_return_type(method: &str) -> InferredType {
        let mut agreed: Option<TypeExpr> = None;
        for row in methods::BUILTIN_METHODS
            .iter()
            .filter(|row| row.name() == method)
        {
            // A generic result depends on the receiver; without one the call
            // could equally be a host method (`client.first()`).
            if methods::returns_generic(row) {
                return None;
            }
            let ty = Self::method_result_type(row, &BTreeMap::new())?;
            match &agreed {
                Some(existing) if *existing != ty => return None,
                Some(_) => {}
                None => agreed = Some(ty),
            }
        }
        agreed
    }

    /// The expected type of each argument, as far as the receiver pins it.
    /// Only callback parameters are reported: their closure parameters take
    /// the receiver's element type, which is what makes `xs.map({ x -> … })`
    /// check its body against the real element type.
    pub(in crate::typechecker) fn builtin_method_expected_callbacks(
        &self,
        receiver: &TypeExpr,
        method: &str,
        args: &[SNode],
        scope: &TypeScope,
    ) -> Vec<Option<TypeExpr>> {
        let mut expected = vec![None; args.len()];
        let Some((kind, mut bindings)) = Self::builtin_method_receiver(receiver) else {
            return expected;
        };
        let Some(row) = Self::builtin_method_row(kind, method, &mut bindings) else {
            return expected;
        };
        self.bind_method_arguments(row, args, &mut bindings, scope);
        for (slot, param) in expected.iter_mut().zip(row.signature.params) {
            let param_ty = param.ty.to_type_expr();
            if let TypeExpr::FnType {
                params,
                return_type,
            } = &param_ty
            {
                let unbound = Self::unbound_method_type_params(row, &bindings);
                *slot = Some(TypeExpr::FnType {
                    params: params
                        .iter()
                        .map(|p| Self::instantiate_method_type(p, &bindings, &unbound, true))
                        .collect(),
                    return_type: Box::new(Self::instantiate_method_type(
                        return_type,
                        &bindings,
                        &unbound,
                        true,
                    )),
                });
            }
        }
        expected
    }

    /// Bind the signature's argument-side generics: a callback's result binds
    /// what the callback returns (`U` in `map`), and any other argument binds
    /// generics the receiver left open (`U` in `reduce`'s initial value).
    fn bind_method_arguments(
        &self,
        row: &MethodDescriptor,
        args: &[SNode],
        bindings: &mut BTreeMap<String, TypeExpr>,
        scope: &TypeScope,
    ) {
        for (param, arg) in row.signature.params.iter().zip(args) {
            let unbound = Self::unbound_method_type_params(row, bindings);
            if unbound.is_empty() {
                return;
            }
            match param.ty.to_type_expr() {
                TypeExpr::FnType {
                    params,
                    return_type,
                } => {
                    let callback_params: Vec<TypeExpr> = params
                        .iter()
                        .map(|p| Self::instantiate_method_type(p, bindings, &unbound, true))
                        .collect();
                    if let Some(returned) = self.infer_callable_return(arg, &callback_params, scope)
                    {
                        let returned = self.resolve_alias(&returned, scope);
                        let _ = Self::extract_type_bindings(
                            &return_type,
                            &returned,
                            &unbound,
                            bindings,
                        );
                    }
                }
                param_ty => {
                    if let Some(actual) = self.infer_type(arg, scope) {
                        let actual = self.resolve_alias(&actual, scope);
                        let _ = Self::extract_type_bindings(&param_ty, &actual, &unbound, bindings);
                    }
                }
            }
        }
    }

    fn unbound_method_type_params(
        row: &MethodDescriptor,
        bindings: &BTreeMap<String, TypeExpr>,
    ) -> BTreeSet<String> {
        row.signature
            .type_params
            .iter()
            .filter(|name| bindings.get(**name).is_none_or(Self::is_wildcard_type))
            .map(|name| (*name).to_string())
            .collect()
    }

    fn method_result_type(
        row: &MethodDescriptor,
        bindings: &BTreeMap<String, TypeExpr>,
    ) -> InferredType {
        if row.signature.returns.is_any() {
            return None;
        }
        let unbound = Self::unbound_method_type_params(row, bindings);
        Some(Self::instantiate_method_type(
            &row.signature.returns.to_type_expr(),
            bindings,
            &unbound,
            false,
        ))
    }

    /// Substitute bound generics. A generic nothing bound becomes the
    /// wildcard inside a callback (so closure checking stays gradual) and
    /// `any` elsewhere, and a container of `any` erases to the bare container
    /// name the checker uses for an unparameterized value.
    fn instantiate_method_type(
        ty: &TypeExpr,
        bindings: &BTreeMap<String, TypeExpr>,
        unbound: &BTreeSet<String>,
        in_callback: bool,
    ) -> TypeExpr {
        let open = if in_callback {
            Self::wildcard_type()
        } else {
            TypeExpr::Named("any".into())
        };
        let mut full = bindings.clone();
        for name in unbound {
            full.insert(name.clone(), open.clone());
        }
        let substituted = crate::typechecker::substitute_type_expr(ty, &full);
        if in_callback {
            substituted
        } else {
            Self::erase_any_containers(substituted)
        }
    }

    fn erase_any_containers(ty: TypeExpr) -> TypeExpr {
        let is_any = |t: &TypeExpr| matches!(t, TypeExpr::Named(n) if n == "any");
        match ty {
            TypeExpr::List(inner) => {
                let inner = Self::erase_any_containers(*inner);
                if is_any(&inner) {
                    TypeExpr::Named("list".into())
                } else {
                    TypeExpr::List(Box::new(inner))
                }
            }
            TypeExpr::DictType(key, value) => {
                let value = Self::erase_any_containers(*value);
                if is_any(&value) {
                    TypeExpr::Named("dict".into())
                } else {
                    TypeExpr::DictType(key, Box::new(value))
                }
            }
            TypeExpr::Applied { name, args }
                if name == "set" && args.len() == 1 && is_any(&args[0]) =>
            {
                TypeExpr::Named("set".into())
            }
            TypeExpr::Union(members) => super::super::union::simplify_union(
                members
                    .into_iter()
                    .map(Self::erase_any_containers)
                    .collect(),
            ),
            other => other,
        }
    }
}
