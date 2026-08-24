//! Structural generic-parameter extraction for call and literal inference.

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::{Node, SNode, ShapeField, TypeExpr};

use super::super::schema_inference::schema_type_expr_from_node;
use super::super::scope::TypeScope;
use super::super::union::simplify_union;
use super::super::TypeChecker;

impl TypeChecker {
    /// Bind type parameters by walking a parameter type against an argument AST
    /// node. This preserves structural information that ordinary inferred
    /// argument types can erase, including inline schemas and contextual
    /// tuples.
    pub(in crate::typechecker) fn bind_from_arg_node(
        &self,
        param: &TypeExpr,
        arg: &SNode,
        type_params: &BTreeSet<String>,
        bindings: &mut BTreeMap<String, TypeExpr>,
        scope: &TypeScope,
    ) -> Result<(), String> {
        let resolved_param = self.resolve_alias(param, scope);
        if resolved_param != *param {
            return self.bind_from_arg_node(&resolved_param, arg, type_params, bindings, scope);
        }
        match param {
            TypeExpr::Applied { name, args } if name == "Schema" && args.len() == 1 => {
                if let TypeExpr::Named(type_param) = &args[0] {
                    if type_params.contains(type_param) {
                        if let Some(resolved) = schema_type_expr_from_node(arg, scope) {
                            Self::bind_type_param(type_param, &resolved, bindings)?;
                        }
                    }
                }
            }
            TypeExpr::Shape(fields) => {
                if let Node::DictLiteral(entries) = &arg.node {
                    for field in fields {
                        if let Some(entry) = entries.iter().find(|entry| {
                            matches!(
                                &entry.key.node,
                                Node::StringLiteral(key) | Node::Identifier(key)
                                    if key == &field.name
                            )
                        }) {
                            self.bind_from_arg_node(
                                &field.type_expr,
                                &entry.value,
                                type_params,
                                bindings,
                                scope,
                            )?;
                        }
                    }
                    return Ok(());
                }
                self.bind_from_inferred_arg(param, arg, type_params, bindings, scope)?;
            }
            TypeExpr::Tuple(params) => {
                if let Node::ListLiteral(items) = &arg.node {
                    if params.len() == items.len()
                        && items
                            .iter()
                            .all(|item| !matches!(item.node, Node::Spread(_)))
                    {
                        for (param, item) in params.iter().zip(items) {
                            self.bind_from_arg_node(param, item, type_params, bindings, scope)?;
                        }
                        return Ok(());
                    }
                }
                self.bind_from_inferred_arg(param, arg, type_params, bindings, scope)?;
            }
            _ => self.bind_from_inferred_arg(param, arg, type_params, bindings, scope)?,
        }
        Ok(())
    }

    fn bind_from_inferred_arg(
        &self,
        param: &TypeExpr,
        arg: &SNode,
        type_params: &BTreeSet<String>,
        bindings: &mut BTreeMap<String, TypeExpr>,
        scope: &TypeScope,
    ) -> Result<(), String> {
        if let Some(arg_type) = self.infer_type(arg, scope) {
            let arg_type = self.resolve_alias(&arg_type, scope);
            Self::extract_type_bindings(param, &arg_type, type_params, bindings)?;
        }
        Ok(())
    }

    /// Recursively extract type parameter bindings from matching param/arg types.
    /// E.g., param_type=list<T> + arg_type=list<Dog> → binds T=Dog.
    pub(in crate::typechecker) fn extract_type_bindings(
        param_type: &TypeExpr,
        arg_type: &TypeExpr,
        type_params: &BTreeSet<String>,
        bindings: &mut BTreeMap<String, TypeExpr>,
    ) -> Result<(), String> {
        match (param_type, arg_type) {
            (TypeExpr::Named(param_name), concrete) if type_params.contains(param_name) => {
                Self::bind_type_param(param_name, concrete, bindings)
            }
            (TypeExpr::List(p_inner), TypeExpr::List(a_inner)) => {
                Self::extract_type_bindings(p_inner, a_inner, type_params, bindings)
            }
            (TypeExpr::Tuple(params), TypeExpr::Tuple(args)) if params.len() == args.len() => {
                for (param, arg) in params.iter().zip(args) {
                    Self::extract_type_bindings(param, arg, type_params, bindings)?;
                }
                Ok(())
            }
            (TypeExpr::List(param), TypeExpr::Tuple(args)) => {
                for arg in args {
                    Self::extract_type_bindings(param, arg, type_params, bindings)?;
                }
                Ok(())
            }
            // A collection may infer a union element type before the generic
            // call is checked (for example `list<Step<int> | Step<string>>`).
            // Every member is an independent inference candidate; the normal
            // binding policy below joins distinct candidates into a union.
            (param, TypeExpr::Union(actual_members)) if !matches!(param, TypeExpr::Union(_)) => {
                for actual in actual_members {
                    Self::extract_type_bindings(param, actual, type_params, bindings)?;
                }
                Ok(())
            }
            // Match closed unions by first removing identical arms. This
            // makes nullable and partially concrete fields unambiguous:
            // `T | nil` against `int | nil` binds `T = int`. When more than
            // one generic-bearing arm remains there is no principled pairing,
            // so leave the call gradual instead of guessing.
            (TypeExpr::Union(param_members), TypeExpr::Union(actual_members)) => {
                let mut unmatched_actual = actual_members.clone();
                let mut unmatched_param = Vec::new();
                for param in param_members {
                    if let Some(index) = unmatched_actual.iter().position(|actual| actual == param)
                    {
                        unmatched_actual.remove(index);
                    } else {
                        unmatched_param.push(param);
                    }
                }
                let generic_members: Vec<_> = unmatched_param
                    .into_iter()
                    .filter(|param| Self::contains_type_param(param, type_params))
                    .collect();
                if generic_members.len() == 1 && !unmatched_actual.is_empty() {
                    let concrete = simplify_union(unmatched_actual);
                    Self::extract_type_bindings(
                        generic_members[0],
                        &concrete,
                        type_params,
                        bindings,
                    )?;
                }
                Ok(())
            }
            (TypeExpr::Union(param_members), actual) => {
                if param_members.iter().any(|param| param == actual) {
                    return Ok(());
                }
                let generic_members: Vec<_> = param_members
                    .iter()
                    .filter(|param| Self::contains_type_param(param, type_params))
                    .collect();
                let compatible_members: Vec<_> = generic_members
                    .iter()
                    .copied()
                    .filter(|param| Self::binding_shapes_compatible(param, actual, type_params))
                    .collect();
                if compatible_members.len() == 1 {
                    Self::extract_type_bindings(
                        compatible_members[0],
                        actual,
                        type_params,
                        bindings,
                    )?;
                } else if generic_members.len() == 1 {
                    Self::extract_type_bindings(generic_members[0], actual, type_params, bindings)?;
                }
                Ok(())
            }
            (TypeExpr::DictType(pk, pv), TypeExpr::DictType(ak, av)) => {
                Self::extract_type_bindings(pk, ak, type_params, bindings)?;
                Self::extract_type_bindings(pv, av, type_params, bindings)
            }
            // A shape literal `{a: 1, b: "x"}` flowing into a `dict<string, V>`
            // parameter is the most common stdlib call pattern — `pick_keys`,
            // `filter_nil`, `merge`, etc. all advertise a generic dict-shape
            // contract. Bind V to the union of field types so the projected
            // result keeps useful element typing instead of collapsing to
            // `dict`.
            (TypeExpr::DictType(pk, pv), TypeExpr::Shape(arg_fields))
            | (
                TypeExpr::DictType(pk, pv),
                TypeExpr::OpenShape {
                    fields: arg_fields, ..
                },
            ) => {
                if matches!(pk.as_ref(), TypeExpr::Named(name) if name == "string") {
                    let value_union = Self::union_of_shape_field_types(arg_fields)
                        .unwrap_or_else(|| TypeExpr::Named("nil".into()));
                    Self::extract_type_bindings(pv, &value_union, type_params, bindings)?;
                }
                Ok(())
            }
            (
                TypeExpr::Applied {
                    name: p_name,
                    args: p_args,
                },
                TypeExpr::Applied {
                    name: a_name,
                    args: a_args,
                },
            ) if p_name == a_name && p_args.len() == a_args.len() => {
                for (param, arg) in p_args.iter().zip(a_args.iter()) {
                    Self::extract_type_bindings(param, arg, type_params, bindings)?;
                }
                Ok(())
            }
            (TypeExpr::Shape(param_fields), TypeExpr::Shape(arg_fields)) => {
                for param_field in param_fields {
                    if let Some(arg_field) = arg_fields
                        .iter()
                        .find(|field| field.name == param_field.name)
                    {
                        Self::extract_type_bindings(
                            &param_field.type_expr,
                            &arg_field.type_expr,
                            type_params,
                            bindings,
                        )?;
                    }
                }
                Ok(())
            }
            // Open record parameter `{f: T, ...R}` against an actual record:
            // bind the explicit fields field-by-field, then bind the single row
            // variable `R` to the actual's **leftover** fields (one-sided row
            // matching — the design's core operation; no HM unification). With
            // no explicit fields (`{...R}`, as in `merge`'s params) R simply
            // binds to the whole actual record. Multiple row variables can't be
            // split unambiguously, so they are left for the gradual fallback.
            (
                TypeExpr::OpenShape {
                    fields: pf,
                    rests: prests,
                },
                arg_type,
            ) => {
                let af: &[ShapeField] = match arg_type {
                    TypeExpr::Shape(af) => af,
                    TypeExpr::OpenShape { fields: af, .. } => af,
                    _ => return Ok(()),
                };
                for pfield in pf {
                    if let Some(afield) = af.iter().find(|f| f.name == pfield.name) {
                        Self::extract_type_bindings(
                            &pfield.type_expr,
                            &afield.type_expr,
                            type_params,
                            bindings,
                        )?;
                    }
                }
                let row_vars: Vec<&String> = prests
                    .iter()
                    .filter_map(|r| match r {
                        TypeExpr::Named(n) if type_params.contains(n) => Some(n),
                        _ => None,
                    })
                    .collect();
                if row_vars.len() == 1 {
                    let explicit: BTreeSet<&str> = pf.iter().map(|f| f.name.as_str()).collect();
                    let leftover: Vec<ShapeField> = af
                        .iter()
                        .filter(|f| !explicit.contains(f.name.as_str()))
                        .cloned()
                        .collect();
                    Self::bind_type_param(row_vars[0], &TypeExpr::Shape(leftover), bindings)?;
                }
                Ok(())
            }
            (
                TypeExpr::FnType {
                    params: p_params,
                    return_type: p_ret,
                },
                TypeExpr::FnType {
                    params: a_params,
                    return_type: a_ret,
                },
            ) => {
                for (param, arg) in p_params.iter().zip(a_params.iter()) {
                    Self::extract_type_bindings(param, arg, type_params, bindings)?;
                }
                Self::extract_type_bindings(p_ret, a_ret, type_params, bindings)
            }
            _ => Ok(()),
        }
    }

    /// Return whether two types have compatible outer structure for generic
    /// binding. This is intentionally weaker than subtyping: it only
    /// disambiguates generic-bearing union arms before ordinary compatibility
    /// checking. For example, `fn() -> T | fn(nil) -> T` against a nullary
    /// closure has exactly one viable arm.
    fn binding_shapes_compatible(
        param: &TypeExpr,
        actual: &TypeExpr,
        type_params: &BTreeSet<String>,
    ) -> bool {
        match (param, actual) {
            (TypeExpr::Named(name), _) if type_params.contains(name) => true,
            (TypeExpr::FnType { params: left, .. }, TypeExpr::FnType { params: right, .. }) => {
                left.len() == right.len()
            }
            (TypeExpr::List(_), TypeExpr::List(_))
            | (TypeExpr::DictType(_, _), TypeExpr::DictType(_, _))
            | (TypeExpr::Shape(_), TypeExpr::Shape(_)) => true,
            (TypeExpr::Tuple(left), TypeExpr::Tuple(right)) => left.len() == right.len(),
            (
                TypeExpr::Applied {
                    name: left,
                    args: left_args,
                },
                TypeExpr::Applied {
                    name: right,
                    args: right_args,
                },
            ) => left == right && left_args.len() == right_args.len(),
            (TypeExpr::Union(members), actual) => members
                .iter()
                .any(|member| Self::binding_shapes_compatible(member, actual, type_params)),
            (left, right) => left == right,
        }
    }

    pub(in crate::typechecker) fn apply_type_bindings(
        ty: &TypeExpr,
        bindings: &BTreeMap<String, TypeExpr>,
    ) -> TypeExpr {
        match ty {
            TypeExpr::Named(name) => bindings
                .get(name)
                .cloned()
                .unwrap_or_else(|| TypeExpr::Named(name.clone())),
            TypeExpr::Union(items) => TypeExpr::Union(
                items
                    .iter()
                    .map(|item| Self::apply_type_bindings(item, bindings))
                    .collect(),
            ),
            TypeExpr::Intersection(items) => TypeExpr::Intersection(
                items
                    .iter()
                    .map(|item| Self::apply_type_bindings(item, bindings))
                    .collect(),
            ),
            TypeExpr::Shape(fields) => TypeExpr::Shape(
                fields
                    .iter()
                    .map(|field| ShapeField {
                        type_expr: Self::apply_type_bindings(&field.type_expr, bindings),
                        ..field.clone()
                    })
                    .collect(),
            ),
            TypeExpr::OpenShape { fields, rests } => {
                let fields = fields
                    .iter()
                    .map(|field| ShapeField {
                        type_expr: Self::apply_type_bindings(&field.type_expr, bindings),
                        ..field.clone()
                    })
                    .collect();
                let rests = rests
                    .iter()
                    .map(|rest| Self::apply_type_bindings(rest, bindings))
                    .collect();
                super::super::binary_ops::fold_open_shape(fields, rests)
            }
            TypeExpr::List(inner) => {
                TypeExpr::List(Box::new(Self::apply_type_bindings(inner, bindings)))
            }
            TypeExpr::Tuple(items) => TypeExpr::Tuple(
                items
                    .iter()
                    .map(|item| Self::apply_type_bindings(item, bindings))
                    .collect(),
            ),
            TypeExpr::Iter(inner) => {
                TypeExpr::Iter(Box::new(Self::apply_type_bindings(inner, bindings)))
            }
            TypeExpr::Generator(inner) => {
                TypeExpr::Generator(Box::new(Self::apply_type_bindings(inner, bindings)))
            }
            TypeExpr::Stream(inner) => {
                TypeExpr::Stream(Box::new(Self::apply_type_bindings(inner, bindings)))
            }
            TypeExpr::DictType(key, value) => TypeExpr::DictType(
                Box::new(Self::apply_type_bindings(key, bindings)),
                Box::new(Self::apply_type_bindings(value, bindings)),
            ),
            TypeExpr::Applied { name, args } => TypeExpr::Applied {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| Self::apply_type_bindings(arg, bindings))
                    .collect(),
            },
            TypeExpr::FnType {
                params,
                return_type,
            } => TypeExpr::FnType {
                params: params
                    .iter()
                    .map(|param| Self::apply_type_bindings(param, bindings))
                    .collect(),
                return_type: Box::new(Self::apply_type_bindings(return_type, bindings)),
            },
            TypeExpr::Never => TypeExpr::Never,
            TypeExpr::LitString(value) => TypeExpr::LitString(value.clone()),
            TypeExpr::LitInt(value) => TypeExpr::LitInt(*value),
            TypeExpr::Owned(inner) => {
                TypeExpr::Owned(Box::new(Self::apply_type_bindings(inner, bindings)))
            }
        }
    }
}
