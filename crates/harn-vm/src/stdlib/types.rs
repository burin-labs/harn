use std::rc::Rc;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_type_builtins(vm: &mut Vm) {
    vm.register_builtin("type_of", |args, _out| {
        let val = args.first().unwrap_or(&VmValue::Nil);
        Ok(VmValue::String(Rc::from(val.type_name())))
    });
    vm.register_builtin("to_string", |args, _out| {
        let val = args.first().unwrap_or(&VmValue::Nil);
        Ok(VmValue::String(Rc::from(val.display())))
    });
    vm.register_builtin("to_int", |args, _out| {
        let val = args.first().unwrap_or(&VmValue::Nil);
        match val {
            VmValue::Int(n) => Ok(VmValue::Int(*n)),
            VmValue::Float(n) => Ok(VmValue::Int(*n as i64)),
            VmValue::String(s) => Ok(s.parse::<i64>().map(VmValue::Int).unwrap_or(VmValue::Nil)),
            _ => Ok(VmValue::Nil),
        }
    });
    vm.register_builtin("to_float", |args, _out| {
        let val = args.first().unwrap_or(&VmValue::Nil);
        match val {
            VmValue::Float(n) => Ok(VmValue::Float(*n)),
            VmValue::Int(n) => Ok(VmValue::Float(*n as f64)),
            VmValue::String(s) => Ok(s.parse::<f64>().map(VmValue::Float).unwrap_or(VmValue::Nil)),
            _ => Ok(VmValue::Nil),
        }
    });

    // --- Result type helpers ---
    vm.register_builtin("Ok", |args, _out| {
        let val = args.first().cloned().unwrap_or(VmValue::Nil);
        Ok(VmValue::EnumVariant {
            enum_name: "Result".into(),
            variant: "Ok".into(),
            fields: vec![val],
        })
    });
    vm.register_builtin("Err", |args, _out| {
        let val = args.first().cloned().unwrap_or(VmValue::Nil);
        Ok(VmValue::EnumVariant {
            enum_name: "Result".into(),
            variant: "Err".into(),
            fields: vec![val],
        })
    });
    vm.register_builtin("is_ok", |args, _out| {
        let val = args.first().unwrap_or(&VmValue::Nil);
        Ok(VmValue::Bool(matches!(
            val,
            VmValue::EnumVariant { enum_name, variant, .. }
            if enum_name == "Result" && variant == "Ok"
        )))
    });
    vm.register_builtin("is_err", |args, _out| {
        let val = args.first().unwrap_or(&VmValue::Nil);
        Ok(VmValue::Bool(matches!(
            val,
            VmValue::EnumVariant { enum_name, variant, .. }
            if enum_name == "Result" && variant == "Err"
        )))
    });
    vm.register_builtin("unwrap", |args, _out| {
        let val = args.first().unwrap_or(&VmValue::Nil);
        match val {
            VmValue::EnumVariant {
                enum_name,
                variant,
                fields,
            } if enum_name == "Result" && variant == "Ok" => {
                Ok(fields.first().cloned().unwrap_or(VmValue::Nil))
            }
            VmValue::EnumVariant {
                enum_name,
                variant,
                fields,
            } if enum_name == "Result" && variant == "Err" => {
                let msg = fields.first().map(|f| f.display()).unwrap_or_default();
                Err(VmError::Runtime(format!("unwrap called on Err: {msg}")))
            }
            _ => Ok(val.clone()),
        }
    });
    vm.register_builtin("unwrap_or", |args, _out| {
        let val = args.first().unwrap_or(&VmValue::Nil);
        let default = args.get(1).cloned().unwrap_or(VmValue::Nil);
        match val {
            VmValue::EnumVariant {
                enum_name,
                variant,
                fields,
            } if enum_name == "Result" && variant == "Ok" => {
                Ok(fields.first().cloned().unwrap_or(VmValue::Nil))
            }
            VmValue::EnumVariant {
                enum_name, variant, ..
            } if enum_name == "Result" && variant == "Err" => Ok(default),
            _ => Ok(val.clone()),
        }
    });
    vm.register_builtin("unwrap_err", |args, _out| {
        let val = args.first().unwrap_or(&VmValue::Nil);
        match val {
            VmValue::EnumVariant {
                enum_name,
                variant,
                fields,
            } if enum_name == "Result" && variant == "Err" => {
                Ok(fields.first().cloned().unwrap_or(VmValue::Nil))
            }
            _ => Err(VmError::Runtime("unwrap_err called on non-Err".into())),
        }
    });

    // --- Collection/type conversion ---
    vm.register_builtin("to_list", |args, _out| {
        match args.first().unwrap_or(&VmValue::Nil) {
            VmValue::Set(s) => Ok(VmValue::List(std::rc::Rc::new(s.to_vec()))),
            VmValue::List(l) => Ok(VmValue::List(l.clone())),
            other => Ok(VmValue::List(std::rc::Rc::new(vec![other.clone()]))),
        }
    });

    vm.register_builtin("len", |args, _out| {
        match args.first().unwrap_or(&VmValue::Nil) {
            VmValue::String(s) => Ok(VmValue::Int(s.chars().count() as i64)),
            VmValue::List(items) => Ok(VmValue::Int(items.len() as i64)),
            VmValue::Dict(map) => Ok(VmValue::Int(map.len() as i64)),
            VmValue::Set(s) => Ok(VmValue::Int(s.len() as i64)),
            _ => Ok(VmValue::Int(0)),
        }
    });
}
