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

    vm.register_builtin("error_category", |args, _out| {
        let val = args.first().unwrap_or(&VmValue::Nil);
        match val {
            VmValue::Dict(d) => {
                let cat = d
                    .get("category")
                    .map(|v| v.display())
                    .unwrap_or_else(|| "generic".to_string());
                Ok(VmValue::String(Rc::from(cat)))
            }
            VmValue::String(s) => {
                let err = VmError::Runtime(s.to_string());
                Ok(VmValue::String(Rc::from(error_to_category(&err).as_str())))
            }
            _ => Ok(VmValue::String(Rc::from("generic"))),
        }
    });

    vm.register_builtin("throw_error", |args, _out| {
        let message = args.first().map(|a| a.display()).unwrap_or_default();
        let category = args
            .get(1)
            .map(|a| ErrorCategory::parse(&a.display()))
            .unwrap_or(ErrorCategory::Generic);

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

    vm.register_builtin("is_timeout", |args, _out| {
        Ok(VmValue::Bool(check_error_category(
            args.first().unwrap_or(&VmValue::Nil),
            "timeout",
            ErrorCategory::Timeout,
        )))
    });

    vm.register_builtin("is_rate_limited", |args, _out| {
        Ok(VmValue::Bool(check_error_category(
            args.first().unwrap_or(&VmValue::Nil),
            "rate_limit",
            ErrorCategory::RateLimit,
        )))
    });
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
