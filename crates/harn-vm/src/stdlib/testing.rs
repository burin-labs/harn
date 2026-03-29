use std::rc::Rc;

use crate::value::{values_equal, VmError, VmValue};
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
}
