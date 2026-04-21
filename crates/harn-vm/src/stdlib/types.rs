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

    vm.register_builtin("Ok", |args, _out| {
        let val = args.first().cloned().unwrap_or(VmValue::Nil);
        Ok(VmValue::enum_variant("Result", "Ok", vec![val]))
    });
    vm.register_builtin("Err", |args, _out| {
        let val = args.first().cloned().unwrap_or(VmValue::Nil);
        Ok(VmValue::enum_variant("Result", "Err", vec![val]))
    });
    vm.register_builtin("is_ok", |args, _out| {
        let val = args.first().unwrap_or(&VmValue::Nil);
        Ok(VmValue::Bool(matches!(
            val,
            VmValue::EnumVariant { enum_name, variant, .. }
            if enum_name.as_ref() == "Result" && variant.as_ref() == "Ok"
        )))
    });
    vm.register_builtin("is_err", |args, _out| {
        let val = args.first().unwrap_or(&VmValue::Nil);
        Ok(VmValue::Bool(matches!(
            val,
            VmValue::EnumVariant { enum_name, variant, .. }
            if enum_name.as_ref() == "Result" && variant.as_ref() == "Err"
        )))
    });
    vm.register_builtin("unwrap", |args, _out| {
        let val = args.first().unwrap_or(&VmValue::Nil);
        match val {
            VmValue::EnumVariant {
                enum_name,
                variant,
                fields,
            } if enum_name.as_ref() == "Result" && variant.as_ref() == "Ok" => {
                Ok(fields.first().cloned().unwrap_or(VmValue::Nil))
            }
            VmValue::EnumVariant {
                enum_name,
                variant,
                fields,
            } if enum_name.as_ref() == "Result" && variant.as_ref() == "Err" => {
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
            } if enum_name.as_ref() == "Result" && variant.as_ref() == "Ok" => {
                Ok(fields.first().cloned().unwrap_or(VmValue::Nil))
            }
            VmValue::EnumVariant {
                enum_name, variant, ..
            } if enum_name.as_ref() == "Result" && variant.as_ref() == "Err" => Ok(default),
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
            } if enum_name.as_ref() == "Result" && variant.as_ref() == "Err" => {
                Ok(fields.first().cloned().unwrap_or(VmValue::Nil))
            }
            _ => Err(VmError::Runtime("unwrap_err called on non-Err".into())),
        }
    });

    vm.register_builtin("unreachable", |args, _out| {
        let msg = match args.first() {
            Some(val) => format!("unreachable code was reached: {}", val.display()),
            None => "unreachable code was reached".to_string(),
        };
        Err(VmError::Runtime(msg))
    });

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
            VmValue::Bytes(bytes) => Ok(VmValue::Int(bytes.len() as i64)),
            VmValue::List(items) => Ok(VmValue::Int(items.len() as i64)),
            VmValue::Dict(map) => Ok(VmValue::Int(map.len() as i64)),
            VmValue::Set(s) => Ok(VmValue::Int(s.len() as i64)),
            VmValue::Range(r) => Ok(VmValue::Int(r.len())),
            _ => Ok(VmValue::Int(0)),
        }
    });

    // `==` is structural. `is_same` is identity (Rc::ptr_eq for heap values);
    // for primitive scalars it reduces to structural equality.
    vm.register_builtin("is_same", |args, _out| {
        let a = args.first().unwrap_or(&VmValue::Nil);
        let b = args.get(1).unwrap_or(&VmValue::Nil);
        Ok(VmValue::Bool(crate::value::values_identical(a, b)))
    });

    // Stable identity key — differs iff two values live at different heap
    // allocations. For hashing by identity rather than structure; primitives
    // return their display() text.
    vm.register_builtin("addr_of", |args, _out| {
        let v = args.first().unwrap_or(&VmValue::Nil);
        Ok(VmValue::String(Rc::from(crate::value::value_identity_key(
            v,
        ))))
    });
}
