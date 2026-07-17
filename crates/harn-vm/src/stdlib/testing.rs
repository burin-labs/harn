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

/// A message argument that stringifies to nil's own JSON encoding, to nil's
/// own `display()`, or to nothing at all carries the same "no message"
/// signal as omitting the argument outright. The dominant way test authors
/// hit this is `assert(cond, json_stringify(maybe_nil_value))`: when the
/// dumped value turns out to be nil, `json_stringify` is *correct* to return
/// `"null"` (that's the JSON encoding of nil), but forwarding it verbatim
/// makes the thrown message read as if "null" were meaningful diagnostic
/// content rather than the absence of any. Treat that value the same as an
/// omitted message so the fallback default (which names the failed
/// assertion) takes over instead.
fn is_uninformative_message(value: &VmValue) -> bool {
    matches!(value, VmValue::Nil)
        || matches!(value, VmValue::String(s) if s.trim().is_empty() || s.as_str() == "null")
}

/// Resolves the message argument at `index`, falling back to `default` when
/// the argument is absent or [`is_uninformative_message`].
fn assert_message(args: &[VmValue], index: usize, default: impl FnOnce() -> String) -> String {
    match args.get(index) {
        Some(value) if !is_uninformative_message(value) => value.display(),
        _ => default(),
    }
}

#[harn_builtin(
    sig = "assert(condition: any, message?: string) -> nil",
    category = "testing"
)]
fn assert_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let condition = args.first().unwrap_or(&VmValue::Nil);
    if !condition.is_truthy() {
        let msg = assert_message(args, 1, || "Assertion failed".to_string());
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(msg))));
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
            let msg = assert_message(args, 2, || {
                format!(
                    "Assertion failed: expected {}, got {}",
                    args[1].display(),
                    args[0].display()
                )
            });
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(msg))));
        }
        Ok(VmValue::Nil)
    } else {
        Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
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
            let msg = assert_message(args, 2, || {
                format!(
                    "Assertion failed: values should not be equal: {}",
                    args[0].display()
                )
            });
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(msg))));
        }
        Ok(VmValue::Nil)
    } else {
        Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
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
            Ok(VmValue::String(arcstr::ArcStr::from(cat)))
        }
        VmValue::String(s) => {
            let err = VmError::Runtime(s.to_string());
            Ok(VmValue::String(arcstr::ArcStr::from(
                error_to_category(&err).as_str(),
            )))
        }
        _ => Ok(VmValue::String(arcstr::ArcStr::from("generic"))),
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
            VmValue::String(arcstr::ArcStr::from(category)),
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
        let args = [error, VmValue::String(arcstr::ArcStr::from(category))];
        error_is_impl(&args, &mut out)
    }

    fn thrown_message(result: Result<VmValue, VmError>) -> String {
        match result.unwrap_err() {
            VmError::Thrown(VmValue::String(s)) => s.to_string(),
            other => panic!("expected Thrown(String), got {other:?}"),
        }
    }

    #[test]
    fn assert_with_no_message_uses_default() {
        let mut out = String::new();
        let args = [VmValue::Bool(false)];
        assert_eq!(
            thrown_message(assert_impl(&args, &mut out)),
            "Assertion failed"
        );
    }

    #[test]
    fn assert_with_nil_message_uses_default_instead_of_literal_nil() {
        let mut out = String::new();
        let args = [VmValue::Bool(false), VmValue::Nil];
        assert_eq!(
            thrown_message(assert_impl(&args, &mut out)),
            "Assertion failed"
        );
    }

    #[test]
    fn assert_with_empty_string_message_uses_default() {
        let mut out = String::new();
        let args = [
            VmValue::Bool(false),
            VmValue::String(arcstr::ArcStr::from("")),
        ];
        assert_eq!(
            thrown_message(assert_impl(&args, &mut out)),
            "Assertion failed"
        );
    }

    /// The realistic footgun this guards: `assert(cond, json_stringify(x))`
    /// where `x` turns out to be nil. `json_stringify(nil)` correctly
    /// returns the string `"null"` (issue is NOT with `json_stringify`);
    /// forwarding it verbatim as the assertion message must not surface the
    /// bare word `null` as though it were real diagnostic content.
    #[test]
    fn assert_with_message_equal_to_json_null_uses_default() {
        let mut out = String::new();
        let args = [
            VmValue::Bool(false),
            VmValue::String(arcstr::ArcStr::from("null")),
        ];
        assert_eq!(
            thrown_message(assert_impl(&args, &mut out)),
            "Assertion failed"
        );
    }

    #[test]
    fn assert_with_real_message_is_preserved() {
        let mut out = String::new();
        let args = [
            VmValue::Bool(false),
            VmValue::String(arcstr::ArcStr::from("receipt was missing a field")),
        ];
        assert_eq!(
            thrown_message(assert_impl(&args, &mut out)),
            "receipt was missing a field"
        );
    }

    #[test]
    fn assert_eq_with_nil_message_falls_back_to_synthesized_default() {
        let mut out = String::new();
        let args = [VmValue::Int(1), VmValue::Int(2), VmValue::Nil];
        assert_eq!(
            thrown_message(assert_eq_impl(&args, &mut out)),
            "Assertion failed: expected 2, got 1"
        );
    }

    #[test]
    fn assert_ne_with_json_null_message_falls_back_to_synthesized_default() {
        let mut out = String::new();
        let args = [
            VmValue::Int(5),
            VmValue::Int(5),
            VmValue::String(arcstr::ArcStr::from("null")),
        ];
        assert_eq!(
            thrown_message(assert_ne_impl(&args, &mut out)),
            "Assertion failed: values should not be equal: 5"
        );
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
