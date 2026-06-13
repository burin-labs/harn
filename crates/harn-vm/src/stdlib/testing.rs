use crate::value::VmDictExt;
use std::collections::BTreeMap;

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{error_to_category, values_equal, ErrorCategory, VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_testing_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &TESTING_CALL_BODY_IMPL_DEF,
    &ASSERT_IMPL_DEF,
    &ASSERT_EQ_IMPL_DEF,
    &ASSERT_NE_IMPL_DEF,
    &ERROR_CATEGORY_IMPL_DEF,
    &THROW_ERROR_IMPL_DEF,
    &IS_TIMEOUT_IMPL_DEF,
    &IS_RATE_LIMITED_IMPL_DEF,
    &ERROR_IS_IMPL_DEF,
    &ERROR_IS_TRANSIENT_IMPL_DEF,
];

#[harn_builtin(
    sig = "__testing_call_body(body: any) -> any",
    kind = "async",
    category = "testing"
)]
async fn testing_call_body_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let body = args
        .first()
        .cloned()
        .ok_or_else(|| VmError::Runtime("__testing_call_body: body is required".to_string()))?;
    if !Vm::is_callable_value(&body) {
        return Err(VmError::TypeError(format!(
            "__testing_call_body: body must be callable, got {}",
            body.type_name()
        )));
    }

    let call_args = match &body {
        VmValue::Closure(closure) => {
            let required = closure.func.required_param_count();
            if required == 0 {
                Vec::new()
            } else if required == 1 {
                vec![VmValue::Nil]
            } else {
                return Err(VmError::Runtime(format!(
                    "__testing_call_body: body expects {required} required argument(s); scoped mock helpers pass at most one context value"
                )));
            }
        }
        _ => Vec::new(),
    };

    let mut vm = ctx.child_vm();
    let result = vm.call_callable_owned(&body, call_args).await;
    ctx.forward_output(&vm.take_output());
    result
}

#[harn_builtin(
    sig = "assert(condition: any, message?: string) -> nil",
    category = "testing"
)]
fn assert_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let condition = args.first().unwrap_or(&VmValue::Nil);
    if !condition.is_truthy() {
        let msg = args
            .get(1)
            .map(|a| a.display())
            .unwrap_or_else(|| "Assertion failed".to_string());
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(msg))));
    }
    Ok(VmValue::Nil)
}

#[harn_builtin(
    sig = "assert_eq(left: any, right: any, message?: string) -> nil",
    category = "testing"
)]
fn assert_eq_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.len() >= 2 {
        if !values_equal(&args[0], &args[1]) {
            let msg = args.get(2).map(|a| a.display()).unwrap_or_else(|| {
                format!(
                    "Assertion failed: expected {}, got {}",
                    args[1].display(),
                    args[0].display()
                )
            });
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(msg))));
        }
        Ok(VmValue::Nil)
    } else {
        Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "assert_eq requires at least 2 arguments",
        ))))
    }
}

#[harn_builtin(
    sig = "assert_ne(left: any, right: any, message?: string) -> nil",
    category = "testing"
)]
fn assert_ne_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.len() >= 2 {
        if values_equal(&args[0], &args[1]) {
            let msg = args.get(2).map(|a| a.display()).unwrap_or_else(|| {
                format!(
                    "Assertion failed: values should not be equal: {}",
                    args[0].display()
                )
            });
            return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(msg))));
        }
        Ok(VmValue::Nil)
    } else {
        Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "assert_ne requires at least 2 arguments",
        ))))
    }
}

#[harn_builtin(sig = "error_category(error: any) -> string", category = "testing")]
fn error_category_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let val = args.first().unwrap_or(&VmValue::Nil);
    match val {
        VmValue::Dict(d) => {
            let cat = d
                .get("category")
                .map(|v| v.display())
                .unwrap_or_else(|| "generic".to_string());
            Ok(VmValue::String(std::sync::Arc::from(cat)))
        }
        VmValue::String(s) => {
            let err = VmError::Runtime(s.to_string());
            Ok(VmValue::String(std::sync::Arc::from(
                error_to_category(&err).as_str(),
            )))
        }
        _ => Ok(VmValue::String(std::sync::Arc::from("generic"))),
    }
}

#[harn_builtin(
    sig = "throw_error(message: string, category?: string) -> never",
    category = "testing"
)]
fn throw_error_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let message = args.first().map(|a| a.display()).unwrap_or_default();
    let category = args
        .get(1)
        .map(|a| ErrorCategory::parse(&a.display()))
        .unwrap_or(ErrorCategory::Generic);

    let mut err_dict = BTreeMap::new();
    err_dict.put_str("message", message.as_str());
    err_dict.put_str("category", category.as_str());
    Err(VmError::Thrown(VmValue::dict(err_dict)))
}

#[harn_builtin(sig = "is_timeout(error: any) -> bool", category = "testing")]
fn is_timeout_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::Bool(error_has_category(
        args.first().unwrap_or(&VmValue::Nil),
        ErrorCategory::Timeout,
    )))
}

#[harn_builtin(sig = "is_rate_limited(error: any) -> bool", category = "testing")]
fn is_rate_limited_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::Bool(error_has_category(
        args.first().unwrap_or(&VmValue::Nil),
        ErrorCategory::RateLimit,
    )))
}

/// Parameterized over the whole `ErrorCategory` taxonomy — `is_timeout` /
/// `is_rate_limited` are just the two pre-wired spellings of this. A harness
/// author can assert any category (`cancelled`, `budget_exceeded`,
/// `server_error`, ...) without the VM hand-wiring a predicate per variant.
#[harn_builtin(
    sig = "error_is(error: any, category: string) -> bool",
    category = "testing"
)]
fn error_is_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let category_str = args.get(1).map(|a| a.display()).unwrap_or_default();
    let category = ErrorCategory::parse(&category_str);
    // `parse` is total (unknown → Generic); reject typos loudly rather than
    // silently asserting against `generic`.
    if category == ErrorCategory::Generic && category_str != "generic" {
        return Err(VmError::Runtime(format!(
            "error_is: unknown error category {category_str:?}"
        )));
    }
    Ok(VmValue::Bool(error_has_category(
        args.first().unwrap_or(&VmValue::Nil),
        category,
    )))
}

/// Whether the error's category is one the agent loop treats as a transient,
/// worth-retrying provider failure — the exact `ErrorCategory::is_transient`
/// oracle, surfaced so tests can assert the retry decision directly.
#[harn_builtin(sig = "error_is_transient(error: any) -> bool", category = "testing")]
fn error_is_transient_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::Bool(
        error_category_of(args.first().unwrap_or(&VmValue::Nil))
            .map(|category| category.is_transient())
            .unwrap_or(false),
    ))
}

/// The category carried by an error value: a structured `{category}` dict's
/// field, or the classification of a raw error string. The dict field is run
/// through `ErrorCategory::parse` so comparisons are by canonical variant (any
/// non-taxonomy string normalizes to `Generic`), matching how `error_is`
/// resolves its argument.
fn error_category_of(val: &VmValue) -> Option<ErrorCategory> {
    match val {
        VmValue::Dict(d) => d
            .get("category")
            .map(|v| ErrorCategory::parse(&v.display())),
        VmValue::String(s) => Some(error_to_category(&VmError::Runtime(s.to_string()))),
        _ => None,
    }
}

fn error_has_category(val: &VmValue, category: ErrorCategory) -> bool {
    error_category_of(val) == Some(category)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict_err(category: &str) -> VmValue {
        VmValue::dict(std::collections::BTreeMap::from([(
            "category".to_string(),
            VmValue::String(std::sync::Arc::from(category)),
        )]))
    }

    fn as_bool(result: Result<VmValue, VmError>) -> bool {
        match result.unwrap() {
            VmValue::Bool(matched) => matched,
            other => panic!("expected bool, got {other:?}"),
        }
    }

    fn error_is(error: VmValue, category: &str) -> Result<VmValue, VmError> {
        let mut out = String::new();
        let args = [error, VmValue::String(std::sync::Arc::from(category))];
        error_is_impl(&args, &mut out)
    }

    #[test]
    fn error_is_matches_any_category_and_subsumes_the_legacy_predicates() {
        assert!(as_bool(error_is(dict_err("cancelled"), "cancelled")));
        assert!(as_bool(error_is(
            dict_err("budget_exceeded"),
            "budget_exceeded"
        )));
        assert!(!as_bool(error_is(dict_err("timeout"), "rate_limit")));
        let mut out = String::new();
        assert!(as_bool(is_timeout_impl(&[dict_err("timeout")], &mut out)));
    }

    #[test]
    fn error_is_rejects_unknown_categories() {
        assert!(error_is(dict_err("timeout"), "not_a_category").is_err());
    }

    #[test]
    fn error_is_transient_uses_the_retry_oracle() {
        let mut out = String::new();
        assert!(as_bool(error_is_transient_impl(
            &[dict_err("rate_limit")],
            &mut out
        )));
        assert!(!as_bool(error_is_transient_impl(
            &[dict_err("auth")],
            &mut out
        )));
    }
}
