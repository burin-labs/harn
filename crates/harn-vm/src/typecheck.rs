//! Runtime type & arity validation, shared between user-defined function
//! calls and registry-known builtin calls.
//!
//! Every call-site validation in the VM funnels through three entry points:
//!
//! - [`assert_value_matches_type`] — project a [`VmValue`] through the shared
//!   `harn_kernel::type_contract` matcher. That matcher is the runtime source
//!   of truth for native and portable `int`/`string`/`list<T>`/... value
//!   compatibility and mirrors static `TypeChecker::types_compatible`
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

use harn_kernel::type_contract::{RuntimeTypeKind, TypeContractValue};
use harn_lexer::Span;
use harn_parser::builtin_signatures::{self, BuiltinSignature, TyExt};
use harn_parser::typechecker::format_type;
use harn_parser::TypeExpr;

use crate::chunk::{BindingTypeSlot, CompiledFunction, ParamSlot};
use crate::runtime_guards::RuntimeParamGuard;
use crate::value::{
    ArgTypeMismatchError, ArityExpect, ArityMismatchError, BindingTypeMismatchError, VmError,
    VmValue,
};
use crate::vm::CallArgs;

impl TypeContractValue for VmValue {
    fn runtime_type_kind(&self) -> RuntimeTypeKind {
        match self {
            Self::Int(_) => RuntimeTypeKind::Int,
            Self::Float(_) => RuntimeTypeKind::Float,
            Self::Decimal(_) => RuntimeTypeKind::Decimal,
            Self::String(_) => RuntimeTypeKind::String,
            Self::Bytes(_) => RuntimeTypeKind::Bytes,
            Self::Bool(_) => RuntimeTypeKind::Bool,
            Self::Nil => RuntimeTypeKind::Nil,
            Self::List(_) => RuntimeTypeKind::List,
            Self::Dict(_) => RuntimeTypeKind::Dict,
            Self::Closure(_) | Self::BuiltinRef(_) | Self::BuiltinRefId(_) => {
                RuntimeTypeKind::Closure
            }
            Self::Duration(_) => RuntimeTypeKind::Duration,
            Self::EnumVariant(_) => RuntimeTypeKind::Enum,
            Self::StructInstance(_) => RuntimeTypeKind::Struct,
            Self::TaskHandle(_) => RuntimeTypeKind::TaskHandle,
            Self::Channel(_) => RuntimeTypeKind::Channel,
            Self::Atomic(_) => RuntimeTypeKind::Atomic,
            Self::Rng(_) => RuntimeTypeKind::Rng,
            Self::SyncPermit(_) => RuntimeTypeKind::SyncPermit,
            Self::Resource(_) => RuntimeTypeKind::Resource,
            Self::ResourceGuard(_) => RuntimeTypeKind::ResourceGuard,
            Self::McpClient(_) => RuntimeTypeKind::McpClient,
            Self::VerdictReceipt(_) => RuntimeTypeKind::VerdictReceipt,
            Self::Set(_) => RuntimeTypeKind::Set,
            Self::Generator(_) => RuntimeTypeKind::Generator,
            Self::Stream(_) => RuntimeTypeKind::Stream,
            Self::Range(_) => RuntimeTypeKind::Range,
            Self::Iter(_) => RuntimeTypeKind::Iter,
            Self::Pair(_) => RuntimeTypeKind::Pair,
            Self::Harness(_) => RuntimeTypeKind::Harness,
        }
    }

    fn list_items(&self) -> Option<&[Self]> {
        match self {
            Self::List(items) => Some(items),
            _ => None,
        }
    }

    fn record_field(&self, name: &str) -> Option<&Self> {
        match self {
            Self::Dict(fields) => fields.get(name),
            Self::StructInstance(_) => self.struct_field(name),
            _ => None,
        }
    }

    fn record_values_match(&self, predicate: &mut dyn FnMut(&Self) -> bool) -> Option<bool> {
        match self {
            Self::Dict(fields) => Some(fields.values().all(predicate)),
            _ => None,
        }
    }

    fn string_literal(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value.as_str()),
            _ => None,
        }
    }

    fn int_literal(&self) -> Option<i64> {
        match self {
            Self::Int(value) => Some(*value),
            _ => None,
        }
    }

    fn nominal_type_name(&self) -> Option<&str> {
        match self {
            Self::StructInstance(value) => Some(value.layout.struct_name()),
            Self::EnumVariant(value) => Some(value.enum_name.as_str()),
            _ => None,
        }
    }
}

/// Validate that `value` satisfies `expected`. Returns `Ok(())` when the
/// value is acceptable, otherwise an [`VmError::ArgTypeMismatch`] tagged
/// with `callee` / `param` / `span` for the caller's diagnostic.
///
/// The shared kernel matcher mirrors the static checker's `types_compatible`
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

/// Validate an annotated `let` / `const` initializer against its declared type.
///
/// The binding-site counterpart of [`validate_user_call`]'s per-parameter
/// assertion, and deliberately the same decision procedure: both project the
/// value through `harn_kernel::type_contract::matches_type`. A declared type
/// therefore accepts and rejects the same values whether it is written on a
/// parameter or on a binding — which is the whole point of checking bindings
/// at all (harn#6252).
pub fn validate_binding_type(
    value: &VmValue,
    slot: &BindingTypeSlot,
    span: Option<Span>,
) -> Result<(), VmError> {
    if matches_type_with_generics(value, &slot.type_expr, &[], &slot.nominal_type_names) {
        return Ok(());
    }
    Err(VmError::BindingTypeMismatch(Box::new(
        BindingTypeMismatchError {
            binding: slot.name.clone(),
            expected: format_type(&slot.type_expr),
            got: value.type_name(),
            span,
        },
    )))
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
    harn_kernel::type_contract::matches_type(value, expected, type_params, nominal_type_names)
}

/// Validate a user-defined function call: arity (respecting defaults +
/// rest), then per-parameter declared-type assertion for parameters
/// that carry a [`TypeExpr`] in their [`crate::chunk::ParamSlot`].
pub fn validate_user_call(
    func: &CompiledFunction,
    args: &[VmValue],
    span: Option<Span>,
) -> Result<(), VmError> {
    validate_user_call_args(func, &CallArgs::Slice(args), span)
}

pub(crate) fn validate_user_call_args(
    func: &CompiledFunction,
    args: &CallArgs<'_>,
    span: Option<Span>,
) -> Result<(), VmError> {
    let required = func.minimum_arg_count();
    let got = args.len();

    if got < required {
        let expected = arity_expect_for(func);
        return Err(VmError::ArityMismatch(Box::new(ArityMismatchError {
            callee: func.name.clone(),
            expected,
            got,
            span,
        })));
    }

    if !func.has_runtime_type_checks {
        return Ok(());
    }

    for (i, value) in args.iter().enumerate() {
        let Some(slot) = user_param_for_arg(func, i) else {
            continue;
        };
        let Some(expected) = &slot.type_expr else {
            continue;
        };
        if let Some(guard) = &slot.runtime_guard {
            validate_with_runtime_guard(value, guard, func, slot, span)?;
            continue;
        }
        validate_uncached_type_expr(value, expected, func, slot, span)?;
    }

    Ok(())
}

fn validate_with_runtime_guard(
    value: &VmValue,
    guard: &RuntimeParamGuard,
    func: &CompiledFunction,
    slot: &ParamSlot,
    span: Option<Span>,
) -> Result<(), VmError> {
    match guard {
        RuntimeParamGuard::CanonicalSchema(schema) => {
            crate::schema::schema_assert_canonical_param(value, &slot.name, schema)
        }
        RuntimeParamGuard::InvalidSchema(error) => Err(VmError::TypeError(format!(
            "parameter '{}': {}",
            slot.name, error
        ))),
        RuntimeParamGuard::TypeExpr(expected) => {
            validate_type_expr_without_schema(value, expected, func, slot, span)
        }
    }
}

fn validate_uncached_type_expr(
    value: &VmValue,
    expected: &TypeExpr,
    func: &CompiledFunction,
    slot: &ParamSlot,
    span: Option<Span>,
) -> Result<(), VmError> {
    if matches!(expected, TypeExpr::Named(name) if func.declares_type_param(name)) {
        return Ok(());
    }
    if let Some(schema) = crate::compiler::Compiler::type_expr_to_schema_value(expected) {
        crate::schema::schema_assert_param(value, &slot.name, &schema)?;
        return Ok(());
    }
    validate_type_expr_without_schema(value, expected, func, slot, span)
}

fn validate_type_expr_without_schema(
    value: &VmValue,
    expected: &TypeExpr,
    func: &CompiledFunction,
    slot: &ParamSlot,
    span: Option<Span>,
) -> Result<(), VmError> {
    assert_value_matches_type_with_generics(
        value,
        expected,
        &func.name,
        &slot.name,
        span,
        &func.type_params,
        &func.nominal_type_names,
    )
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
        if matches!(&expected, TypeExpr::Named(n) if sig.is_type_param(n.as_str())) {
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
    ArityExpect::AtLeast(func.minimum_arg_count())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::Chunk;
    use std::sync::Arc;

    fn vm_int(n: i64) -> VmValue {
        VmValue::Int(n)
    }

    fn vm_string(s: &str) -> VmValue {
        VmValue::String(arcstr::ArcStr::from(s))
    }

    fn vm_dict(entries: impl IntoIterator<Item = (&'static str, VmValue)>) -> VmValue {
        VmValue::dict(entries)
    }

    fn ty_int() -> TypeExpr {
        TypeExpr::Named("int".into())
    }

    fn ty_string() -> TypeExpr {
        TypeExpr::Named("string".into())
    }

    fn param_slot(name: &str, type_expr: Option<TypeExpr>) -> ParamSlot {
        ParamSlot {
            name: name.to_string(),
            runtime_guard: type_expr.as_ref().map(RuntimeParamGuard::from_type_expr),
            type_expr,
            has_default: false,
        }
    }

    fn compiled_function(params: Vec<ParamSlot>) -> CompiledFunction {
        let has_runtime_type_checks = CompiledFunction::has_runtime_type_checks_for_params(&params);
        CompiledFunction {
            name: "f".to_string(),
            type_params: Vec::new(),
            nominal_type_names: Vec::new(),
            params,
            default_start: None,
            chunk: Arc::new(Chunk::new()),
            is_generator: false,
            is_stream: false,
            has_rest_param: false,
            has_runtime_type_checks,
        }
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
        let good = VmValue::List(std::sync::Arc::new(vec![vm_int(1), vm_int(2)]));
        let bad = VmValue::List(std::sync::Arc::new(vec![vm_int(1), vm_string("x")]));
        assert!(matches_type(&good, &list_int));
        assert!(!matches_type(&bad, &list_int));
    }

    #[test]
    fn tuple_validates_arity_and_each_position() {
        let tuple = TypeExpr::Tuple(vec![ty_int(), ty_string()]);
        let good = VmValue::List(std::sync::Arc::new(vec![vm_int(1), vm_string("x")]));
        let wrong_position = VmValue::List(std::sync::Arc::new(vec![vm_string("x"), vm_int(1)]));
        let wrong_arity = VmValue::List(std::sync::Arc::new(vec![vm_int(1)]));
        assert!(matches_type(&good, &tuple));
        assert!(!matches_type(&wrong_position, &tuple));
        assert!(!matches_type(&wrong_arity, &tuple));
    }

    #[test]
    fn shape_validates_required_fields() {
        let shape = TypeExpr::Shape(vec![harn_parser::ShapeField::synthetic(
            "x",
            ty_int(),
            false,
        )]);
        let mut good = std::collections::BTreeMap::new();
        good.insert("x".to_string(), vm_int(7));
        assert!(matches_type(&VmValue::dict(good), &shape));
        assert!(!matches_type(
            &VmValue::dict_map(Default::default()),
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

    #[test]
    fn validate_user_call_skips_param_walk_for_untyped_function() {
        let func = compiled_function(vec![param_slot("value", None)]);

        validate_user_call(&func, &[vm_string("anything")], None).unwrap();

        let err = validate_user_call(&func, &[], None).unwrap_err();
        assert!(matches!(err, VmError::ArityMismatch(_)));
    }

    #[test]
    fn validate_user_call_checks_typed_function() {
        let func = compiled_function(vec![param_slot("value", Some(ty_int()))]);

        validate_user_call(&func, &[vm_int(1)], None).unwrap();

        let err = validate_user_call(&func, &[vm_string("bad")], None).unwrap_err();
        assert!(matches!(err, VmError::Runtime(_) | VmError::TypeError(_)));
    }

    #[test]
    fn validate_user_call_uses_cached_runtime_guard_metadata() {
        let string_schema = VmValue::dict(std::collections::BTreeMap::from([(
            "type".to_string(),
            VmValue::String(arcstr::ArcStr::from("string")),
        )]));
        let guard = RuntimeParamGuard::CanonicalSchema(
            crate::schema::canonical_param_schema(&string_schema).unwrap(),
        );
        let func = compiled_function(vec![ParamSlot {
            name: "value".to_string(),
            type_expr: Some(ty_int()),
            runtime_guard: Some(guard),
            has_default: false,
        }]);

        validate_user_call(&func, &[vm_string("cached")], None).unwrap();
        validate_user_call(&func, &[vm_string("guard")], None).unwrap();

        let err = validate_user_call(&func, &[vm_int(1)], None).unwrap_err();
        assert!(matches!(err, VmError::Runtime(_) | VmError::TypeError(_)));
    }

    #[test]
    fn runtime_guard_does_not_narrow_partially_lowerable_union() {
        let func = compiled_function(vec![param_slot(
            "options",
            Some(TypeExpr::Union(vec![
                TypeExpr::Named("OpenOptions".into()),
                TypeExpr::Named("nil".into()),
            ])),
        )]);

        validate_user_call(&func, &[VmValue::Nil], None).unwrap();
        validate_user_call(&func, &[vm_dict([("foo", vm_string("ok"))])], None).unwrap();
    }
}
