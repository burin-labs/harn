use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{error_to_category, values_equal, ErrorCategory, VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_testing_builtins(vm: &mut Vm) {
    vm.register_builtin("assert", |args, _out| {
        let condition = args.first().unwrap_or(&VmValue::Nil);
        if !condition.is_truthy() {
            let msg = args
                .get(1)
                .map(|a| a.display())
                .unwrap_or_else(|| "Assertion failed".to_string());
            return Err(VmError::Thrown(VmValue::String(Rc::from(msg))));
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("assert_eq", |args, _out| {
        if args.len() >= 2 {
            if !values_equal(&args[0], &args[1]) {
                let msg = args.get(2).map(|a| a.display()).unwrap_or_else(|| {
                    format!(
                        "Assertion failed: expected {}, got {}",
                        args[1].display(),
                        args[0].display()
                    )
                });
                return Err(VmError::Thrown(VmValue::String(Rc::from(msg))));
            }
            Ok(VmValue::Nil)
        } else {
            Err(VmError::Thrown(VmValue::String(Rc::from(
                "assert_eq requires at least 2 arguments",
            ))))
        }
    });

    vm.register_builtin("assert_ne", |args, _out| {
        if args.len() >= 2 {
            if values_equal(&args[0], &args[1]) {
                let msg = args.get(2).map(|a| a.display()).unwrap_or_else(|| {
                    format!(
                        "Assertion failed: values should not be equal: {}",
                        args[0].display()
                    )
                });
                return Err(VmError::Thrown(VmValue::String(Rc::from(msg))));
            }
            Ok(VmValue::Nil)
        } else {
            Err(VmError::Thrown(VmValue::String(Rc::from(
                "assert_ne requires at least 2 arguments",
            ))))
        }
    });

    // --- Structured error builtins ---

    // error_category(err_value) -> string
    // Extract the error category from a caught error value.
    // Returns: "timeout", "auth", "rate_limit", "tool_error", "cancelled",
    //          "not_found", "circuit_open", or "generic"
    vm.register_builtin("error_category", |args, _out| {
        let val = args.first().unwrap_or(&VmValue::Nil);
        match val {
            VmValue::Dict(d) => {
                // Check for category field
                let cat = d
                    .get("category")
                    .map(|v| v.display())
                    .unwrap_or_else(|| "generic".to_string());
                Ok(VmValue::String(Rc::from(cat)))
            }
            VmValue::String(s) => {
                // Infer category from error message text
                let err = VmError::Runtime(s.to_string());
                Ok(VmValue::String(Rc::from(error_to_category(&err).as_str())))
            }
            _ => Ok(VmValue::String(Rc::from("generic"))),
        }
    });

    // throw_error(message, category) -> never
    // Throw a categorized error that can be matched with error_category().
    vm.register_builtin("throw_error", |args, _out| {
        let message = args.first().map(|a| a.display()).unwrap_or_default();
        let category = args
            .get(1)
            .map(|a| ErrorCategory::parse(&a.display()))
            .unwrap_or(ErrorCategory::Generic);

        // Throw as a dict with category and message fields for pattern matching
        let mut err_dict = BTreeMap::new();
        err_dict.insert(
            "message".to_string(),
            VmValue::String(Rc::from(message.as_str())),
        );
        err_dict.insert(
            "category".to_string(),
            VmValue::String(Rc::from(category.as_str())),
        );
        Err(VmError::Thrown(VmValue::Dict(Rc::new(err_dict))))
    });

    // is_timeout(err) -> bool
    vm.register_builtin("is_timeout", |args, _out| {
        let val = args.first().unwrap_or(&VmValue::Nil);
        let is = match val {
            VmValue::Dict(d) => d
                .get("category")
                .map(|v| v.display() == "timeout")
                .unwrap_or(false),
            VmValue::String(s) => {
                let err = VmError::Runtime(s.to_string());
                error_to_category(&err) == ErrorCategory::Timeout
            }
            _ => false,
        };
        Ok(VmValue::Bool(is))
    });

    // is_rate_limited(err) -> bool
    vm.register_builtin("is_rate_limited", |args, _out| {
        let val = args.first().unwrap_or(&VmValue::Nil);
        let is = match val {
            VmValue::Dict(d) => d
                .get("category")
                .map(|v| v.display() == "rate_limit")
                .unwrap_or(false),
            VmValue::String(s) => {
                let err = VmError::Runtime(s.to_string());
                error_to_category(&err) == ErrorCategory::RateLimit
            }
            _ => false,
        };
        Ok(VmValue::Bool(is))
    });
}
