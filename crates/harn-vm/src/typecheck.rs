//! Runtime type & arity validation, shared between user-defined function
//! calls and registry-known builtin calls.
//!
//! Every call-site validation in the VM funnels through three entry points:
//!
//! - [`assert_value_matches_type`] — given a [`VmValue`] and a
//!   [`TypeExpr`], decide whether the value satisfies the type. The
//!   single source of runtime truth for `int`/`string`/`list<T>`/...
//!   compatibility, mirroring the static [`TypeChecker::types_compatible`]
//!   semantics on values rather than type expressions.
//! - [`validate_user_call`] — arity check + per-arg declared-type
//!   assertion for compiled user-defined functions
//!   ([`crate::chunk::CompiledFunction`]).
//! - [`validate_builtin_call`] — arity check + per-arg type assertion
//!   for builtins, driven by the parser's
//!   [`harn_parser::builtin_signatures`] registry. The runtime never
//!   re-implements per-builtin validation; the registry is the contract.
//!
//! All three return [`crate::value::VmError`] variants
//! ([`VmError::ArityMismatch`], [`VmError::ArgTypeMismatch`]) on failure
//! so error UX is uniform. Callers may pass an optional
//! [`harn_lexer::Span`] when they have a source location for the call
//! site (e.g. derived from the chunk's PC→span table); when omitted the
//! error renders without a positional suffix.

use harn_lexer::Span;
use harn_parser::builtin_signatures::{self, BuiltinSignature};
use harn_parser::typechecker::format_type;
use harn_parser::TypeExpr;

use crate::chunk::{CompiledFunction, ParamSlot};
use crate::value::{ArgTypeMismatchError, ArityExpect, ArityMismatchError, VmError, VmValue};

/// Validate that `value` satisfies `expected`. Returns `Ok(())` when the
/// value is acceptable, otherwise an [`VmError::ArgTypeMismatch`] tagged
/// with `callee` / `param` / `span` for the caller's diagnostic.
///
/// The dispatch table mirrors the static checker's `types_compatible`
/// rules:
/// - `Named("any")` and the special generic-parameter sentinel skip
///   validation (any value passes).
/// - `Named("number")` accepts `int` or `float`.
/// - `Optional<T>` / `T | nil` accepts the inner type or `Nil`.
/// - `list<T>`, `dict<K, V>`, `iter<T>`, `Generator<T>`, `Stream<T>`
///   check the container; element-level validation is per element when
///   the value is a literal `VmValue::List` / `VmValue::Dict` whose
///   contents are cheap to walk. For lazy iterators / streams we skip
///   element validation (they may be infinite or expensive).
/// - `Shape{...}` validates field presence and per-field types against
///   `VmValue::Dict` and `VmValue::StructInstance`.
/// - `Union(...)` accepts any matching alternative.
/// - `Intersection(...)` accepts only when *every* alternative matches.
/// - Literal types (`LitInt`, `LitString`) require value equality with
///   the literal.
/// - `Never` always rejects.
pub fn assert_value_matches_type(
    value: &VmValue,
    expected: &TypeExpr,
    callee: &str,
    param: &str,
    span: Option<Span>,
) -> Result<(), VmError> {
    assert_value_matches_type_with_generics(value, expected, callee, param, span, &[], &[])
}

fn assert_value_matches_type_with_generics(
    value: &VmValue,
    expected: &TypeExpr,
    callee: &str,
    param: &str,
    span: Option<Span>,
    type_params: &[String],
    nominal_type_names: &[String],
) -> Result<(), VmError> {
    if matches_type_with_generics(value, expected, type_params, nominal_type_names) {
        Ok(())
    } else {
        Err(VmError::ArgTypeMismatch(Box::new(ArgTypeMismatchError {
            callee: callee.to_string(),
            param: param.to_string(),
            expected: format_type(expected),
            got: value.type_name(),
            span,
        })))
    }
}

fn user_param_for_arg(func: &CompiledFunction, index: usize) -> Option<&ParamSlot> {
    if func.has_rest_param && index >= func.params.len().saturating_sub(1) {
        func.params.last()
    } else {
        func.params.get(index)
    }
}

fn builtin_param_for_arg(
    sig: &BuiltinSignature,
    index: usize,
) -> Option<&harn_parser::builtin_signatures::Param> {
    if sig.has_rest && index >= sig.params.len().saturating_sub(1) {
        sig.params.last()
    } else {
        sig.params.get(index)
    }
}

/// Recursive predicate driving [`assert_value_matches_type`]. Kept
/// internal so the public API only exposes `Result`-returning forms.
#[cfg(test)]
fn matches_type(value: &VmValue, expected: &TypeExpr) -> bool {
    matches_type_with_generics(value, expected, &[], &[])
}

fn matches_type_with_generics(
    value: &VmValue,
    expected: &TypeExpr,
    type_params: &[String],
    nominal_type_names: &[String],
) -> bool {
    match expected {
        TypeExpr::Named(name) => match name.as_str() {
            _ if type_params.iter().any(|param| param == name) => true,
            "any" | "unknown" => true,
            "int" => matches!(value, VmValue::Int(_)),
            "float" => matches!(value, VmValue::Float(_) | VmValue::Int(_)),
            "number" => matches!(value, VmValue::Int(_) | VmValue::Float(_)),
            "string" => matches!(value, VmValue::String(_)),
            "bool" => matches!(value, VmValue::Bool(_)),
            "nil" => matches!(value, VmValue::Nil),
            "list" => matches!(value, VmValue::List(_)),
            "dict" => matches!(value, VmValue::Dict(_)),
            "bytes" => matches!(value, VmValue::Bytes(_)),
            "duration" => matches!(value, VmValue::Duration(_)),
            "set" => matches!(value, VmValue::Set(_)),
            "range" => matches!(value, VmValue::Range(_)),
            "iter" => matches!(value, VmValue::Iter(_)),
            "generator" | "Generator" => matches!(value, VmValue::Generator(_)),
            "stream" | "Stream" => matches!(value, VmValue::Stream(_)),
            "channel" => matches!(value, VmValue::Channel(_)),
            "task_handle" => matches!(value, VmValue::TaskHandle(_)),
            "atomic" => matches!(value, VmValue::Atomic(_)),
            "rng" => matches!(value, VmValue::Rng(_)),
            "sync_permit" => matches!(value, VmValue::SyncPermit(_)),
            "mcp_client" => matches!(value, VmValue::McpClient(_)),
            "pair" => matches!(value, VmValue::Pair(_)),
            "enum" => matches!(value, VmValue::EnumVariant { .. }),
            "struct" => matches!(value, VmValue::StructInstance { .. }),
            "closure" => matches!(
                value,
                VmValue::Closure(_) | VmValue::BuiltinRef(_) | VmValue::BuiltinRefId { .. }
            ),
            _ => {
                if !nominal_type_names.iter().any(|ty| ty == name) {
                    true
                } else {
                    value
                        .struct_name()
                        .is_some_and(|struct_name| struct_name == name)
                        || matches!(value, VmValue::EnumVariant { enum_name, .. } if enum_name.as_ref() == name)
                }
            }
        },
        TypeExpr::Union(members) => members
            .iter()
            .any(|m| matches_type_with_generics(value, m, type_params, nominal_type_names)),
        TypeExpr::Intersection(members) => members
            .iter()
            .all(|m| matches_type_with_generics(value, m, type_params, nominal_type_names)),
        TypeExpr::List(inner) => match value {
            VmValue::List(items) => items
                .iter()
                .all(|v| matches_type_with_generics(v, inner, type_params, nominal_type_names)),
            _ => false,
        },
        TypeExpr::DictType(_, vt) => match value {
            VmValue::Dict(map) => map
                .values()
                .all(|v| matches_type_with_generics(v, vt, type_params, nominal_type_names)),
            _ => false,
        },
        TypeExpr::Iter(_) | TypeExpr::Generator(_) | TypeExpr::Stream(_) => match value {
            // Lazy / async sequences: only check the container shape;
            // element-level validation would force evaluation.
            VmValue::List(_) | VmValue::Generator(_) | VmValue::Stream(_) => true,
            _ => false,
        },
        TypeExpr::Shape(fields) => match value {
            VmValue::Dict(map) => fields.iter().all(|f| match map.get(&f.name) {
                // Optional fields treat an explicit `nil` value the same as
                // "field missing" so callers can write `{flag: nil}` to mean
                // "default" without having to omit the key entirely.
                Some(VmValue::Nil) if f.optional => true,
                Some(v) => {
                    matches_type_with_generics(v, &f.type_expr, type_params, nominal_type_names)
                }
                None => f.optional,
            }),
            VmValue::StructInstance { .. } => {
                fields.iter().all(|f| match value.struct_field(&f.name) {
                    Some(VmValue::Nil) if f.optional => true,
                    Some(v) => {
                        matches_type_with_generics(v, &f.type_expr, type_params, nominal_type_names)
                    }
                    None => f.optional,
                })
            }
            _ => false,
        },
        TypeExpr::Applied { name, args } => match (name.as_str(), args.as_slice()) {
            ("list", [inner]) => matches_type_with_generics(
                value,
                &TypeExpr::List(Box::new(inner.clone())),
                type_params,
                nominal_type_names,
            ),
            ("dict", [k, v]) => matches_type_with_generics(
                value,
                &TypeExpr::DictType(Box::new(k.clone()), Box::new(v.clone())),
                type_params,
                nominal_type_names,
            ),
            ("Option", [inner]) => {
                matches!(value, VmValue::Nil)
                    || matches_type_with_generics(value, inner, type_params, nominal_type_names)
            }
            // Result<T, E>, custom user-applied generics, Schema<T>, etc.
            // fall through to permissive — runtime can't determine the
            // active variant without more semantic knowledge.
            _ => true,
        },
        TypeExpr::FnType { .. } => matches!(
            value,
            VmValue::Closure(_) | VmValue::BuiltinRef(_) | VmValue::BuiltinRefId { .. }
        ),
        TypeExpr::Never => false,
        TypeExpr::LitString(s) => matches!(value, VmValue::String(rs) if rs.as_ref() == s),
        TypeExpr::LitInt(i) => matches!(value, VmValue::Int(rv) if rv == i),
    }
}

/// Validate a user-defined function call: arity (respecting defaults +
/// rest), then per-parameter declared-type assertion for parameters
/// that carry a [`TypeExpr`] in their [`crate::chunk::ParamSlot`].
pub fn validate_user_call(
    func: &CompiledFunction,
    args: &[VmValue],
    span: Option<Span>,
) -> Result<(), VmError> {
    let total = func.params.len();
    let required = func.required_param_count();
    let got = args.len();

    let arity_ok = if func.has_rest_param {
        // Rest absorbs everything >= (total - 1).
        got >= total.saturating_sub(1)
    } else {
        got >= required && got <= total
    };

    if !arity_ok {
        let expected = arity_expect_for(func);
        return Err(VmError::ArityMismatch(Box::new(ArityMismatchError {
            callee: func.name.clone(),
            expected,
            got,
            span,
        })));
    }

    for (i, value) in args.iter().enumerate() {
        let Some(slot) = user_param_for_arg(func, i) else {
            continue;
        };
        let Some(expected) = &slot.type_expr else {
            continue;
        };
        if matches!(expected, TypeExpr::Named(name) if func.declares_type_param(name)) {
            continue;
        }
        if let Some(schema) = crate::compiler::Compiler::type_expr_to_schema_value(expected) {
            crate::schema::schema_assert_param(value, &slot.name, &schema)?;
            continue;
        }
        assert_value_matches_type_with_generics(
            value,
            expected,
            &func.name,
            &slot.name,
            span,
            &func.type_params,
            &func.nominal_type_names,
        )?;
    }

    Ok(())
}

/// Validate a builtin call against the parser's signature registry.
/// Returns `Ok(())` when the builtin is unknown to the registry — the
/// alignment guarantee enforced at registration time means unknown
/// names are necessarily internal/special-purpose builtins
/// (e.g. compiler-synthesized `__*`) that don't need runtime
/// validation.
pub fn validate_builtin_call(
    name: &str,
    args: &[VmValue],
    span: Option<Span>,
) -> Result<(), VmError> {
    let Some(sig) = builtin_signatures::lookup(name) else {
        return Ok(());
    };
    validate_against_signature(name, sig, args, span)
}

/// Shared implementation for [`validate_builtin_call`] (and any future
/// callers that already have a signature in hand). Public so test
/// harnesses can drive it directly with synthetic signatures.
pub fn validate_against_signature(
    name: &str,
    sig: &BuiltinSignature,
    args: &[VmValue],
    span: Option<Span>,
) -> Result<(), VmError> {
    let total = sig.params.len();
    let required = sig.required_params();
    let got = args.len();

    let arity_ok = if sig.has_rest {
        got >= total.saturating_sub(1)
    } else {
        got >= required && got <= total
    };

    if !arity_ok {
        let expected = if sig.has_rest {
            ArityExpect::AtLeast(total.saturating_sub(1))
        } else if required == total {
            ArityExpect::Exact(total)
        } else {
            ArityExpect::Range {
                min: required,
                max: total,
            }
        };
        return Err(VmError::ArityMismatch(Box::new(ArityMismatchError {
            callee: name.to_string(),
            expected,
            got,
            span,
        })));
    }

    for (i, value) in args.iter().enumerate() {
        let Some(param) = builtin_param_for_arg(sig, i) else {
            continue;
        };
        if param.optional && matches!(value, VmValue::Nil) {
            continue;
        }
        // Generic type parameters inside builtin signatures are not
        // resolvable at the value level — the static checker handles
        // them. Skip type-param positions at runtime to avoid bogus
        // mismatches.
        let expected = param.ty.to_type_expr();
        if matches!(&expected, TypeExpr::Named(n) if sig.is_type_param(n)) {
            continue;
        }
        // `any` is always satisfied; format_type would render "any"
        // and the runtime predicate accepts everything anyway.
        if param.ty.is_any() {
            continue;
        }
        if matches!(param.ty, harn_parser::builtin_signatures::Ty::SchemaOf(_)) {
            continue;
        }
        assert_value_matches_type(value, &expected, name, param.name, span)?;
    }

    Ok(())
}

/// Compute the [`ArityExpect`] to embed in an [`VmError::ArityMismatch`]
/// for a user-defined function. Respects defaults and rest-param flags
/// so the message reads naturally.
fn arity_expect_for(func: &CompiledFunction) -> ArityExpect {
    let total = func.params.len();
    let required = func.required_param_count();
    if func.has_rest_param {
        ArityExpect::AtLeast(total.saturating_sub(1))
    } else if required == total {
        ArityExpect::Exact(total)
    } else {
        ArityExpect::Range {
            min: required,
            max: total,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    fn vm_int(n: i64) -> VmValue {
        VmValue::Int(n)
    }

    fn vm_string(s: &str) -> VmValue {
        VmValue::String(Rc::from(s))
    }

    fn ty_int() -> TypeExpr {
        TypeExpr::Named("int".into())
    }

    fn ty_string() -> TypeExpr {
        TypeExpr::Named("string".into())
    }

    #[test]
    fn matches_primitive_types() {
        assert!(matches_type(&vm_int(42), &ty_int()));
        assert!(!matches_type(&vm_int(42), &ty_string()));
        assert!(matches_type(&vm_string("x"), &ty_string()));
        assert!(matches_type(
            &VmValue::Bool(true),
            &TypeExpr::Named("bool".into())
        ));
        assert!(matches_type(&VmValue::Nil, &TypeExpr::Named("nil".into())));
    }

    #[test]
    fn float_accepts_int_promotion() {
        // Mirrors the static rule: `int` is assignable to `float`.
        assert!(matches_type(&vm_int(3), &TypeExpr::Named("float".into())));
        assert!(matches_type(
            &VmValue::Float(3.0),
            &TypeExpr::Named("float".into())
        ));
    }

    #[test]
    fn union_accepts_any_member() {
        let union = TypeExpr::Union(vec![ty_int(), ty_string()]);
        assert!(matches_type(&vm_int(1), &union));
        assert!(matches_type(&vm_string("y"), &union));
        assert!(!matches_type(&VmValue::Bool(true), &union));
    }

    #[test]
    fn optional_accepts_nil() {
        let opt = TypeExpr::Union(vec![ty_string(), TypeExpr::Named("nil".into())]);
        assert!(matches_type(&VmValue::Nil, &opt));
        assert!(matches_type(&vm_string("x"), &opt));
        assert!(!matches_type(&vm_int(1), &opt));
    }

    #[test]
    fn list_validates_elements() {
        let list_int = TypeExpr::List(Box::new(ty_int()));
        let good = VmValue::List(Rc::new(vec![vm_int(1), vm_int(2)]));
        let bad = VmValue::List(Rc::new(vec![vm_int(1), vm_string("x")]));
        assert!(matches_type(&good, &list_int));
        assert!(!matches_type(&bad, &list_int));
    }

    #[test]
    fn shape_validates_required_fields() {
        let shape = TypeExpr::Shape(vec![harn_parser::ShapeField {
            name: "x".into(),
            type_expr: ty_int(),
            optional: false,
        }]);
        let mut good = std::collections::BTreeMap::new();
        good.insert("x".to_string(), vm_int(7));
        assert!(matches_type(&VmValue::Dict(Rc::new(good)), &shape));
        assert!(!matches_type(
            &VmValue::Dict(Rc::new(std::collections::BTreeMap::new())),
            &shape
        ));
    }

    #[test]
    fn named_type_matches_user_struct_name() {
        let custom = TypeExpr::Named("MyStruct".into());
        assert!(!matches_type_with_generics(
            &vm_int(1),
            &custom,
            &[],
            &["MyStruct".to_string()]
        ));
        assert!(matches_type_with_generics(
            &VmValue::struct_instance("MyStruct", Default::default()),
            &custom,
            &[],
            &["MyStruct".to_string()]
        ));
    }

    #[test]
    fn lit_int_requires_value_equality() {
        assert!(matches_type(&vm_int(42), &TypeExpr::LitInt(42)));
        assert!(!matches_type(&vm_int(7), &TypeExpr::LitInt(42)));
    }

    #[test]
    fn assert_value_returns_arg_type_mismatch_on_fail() {
        let err =
            assert_value_matches_type(&vm_string("abc"), &ty_int(), "myFn", "n", None).unwrap_err();
        match err {
            VmError::ArgTypeMismatch(err) => {
                assert_eq!(err.callee, "myFn");
                assert_eq!(err.param, "n");
                assert_eq!(err.expected, "int");
                assert_eq!(err.got, "string");
                assert!(err.span.is_none());
            }
            other => panic!("expected ArgTypeMismatch, got {other:?}"),
        }
    }
}
