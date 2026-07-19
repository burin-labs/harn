//! Structural generic-parameter extraction for call and literal inference.

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::{ShapeField, TypeExpr};

use super::super::union::simplify_union;
use super::super::TypeChecker;

impl TypeChecker {
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
                if generic_members.len() == 1 {
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
}
