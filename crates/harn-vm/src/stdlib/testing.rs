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
    err_dict.insert(
        "message".to_string(),
        VmValue::String(std::sync::Arc::from(message.as_str())),
    );
    err_dict.insert(
        "category".to_string(),
        VmValue::String(std::sync::Arc::from(category.as_str())),
    );
    Err(VmError::Thrown(VmValue::Dict(std::sync::Arc::new(
        err_dict,
    ))))
}

#[harn_builtin(sig = "is_timeout(error: any) -> bool", category = "testing")]
fn is_timeout_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::Bool(check_error_category(
        args.first().unwrap_or(&VmValue::Nil),
        "timeout",
        ErrorCategory::Timeout,
    )))
}

#[harn_builtin(sig = "is_rate_limited(error: any) -> bool", category = "testing")]
fn is_rate_limited_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    Ok(VmValue::Bool(check_error_category(
        args.first().unwrap_or(&VmValue::Nil),
        "rate_limit",
        ErrorCategory::RateLimit,
    )))
}

fn check_error_category(val: &VmValue, category_str: &str, category: ErrorCategory) -> bool {
    match val {
        VmValue::Dict(d) => d
            .get("category")
            .map(|v| v.display() == category_str)
            .unwrap_or(false),
        VmValue::String(s) => {
            let err = VmError::Runtime(s.to_string());
            error_to_category(&err) == category
        }
        _ => false,
    }
}
