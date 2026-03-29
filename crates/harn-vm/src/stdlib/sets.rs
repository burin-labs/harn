use std::rc::Rc;

use crate::value::{values_equal, VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_set_builtins(vm: &mut Vm) {
    vm.register_builtin("set", |args, _out| {
        let mut items: Vec<VmValue> = Vec::new();
        for arg in args {
            match arg {
                VmValue::List(list) => {
                    for v in list.iter() {
                        if !items.iter().any(|x| values_equal(x, v)) {
                            items.push(v.clone());
                        }
                    }
                }
                VmValue::Set(s) => {
                    for v in s.iter() {
                        if !items.iter().any(|x| values_equal(x, v)) {
                            items.push(v.clone());
                        }
                    }
                }
                other => {
                    if !items.iter().any(|x| values_equal(x, other)) {
                        items.push(other.clone());
                    }
                }
            }
        }
        Ok(VmValue::Set(Rc::new(items)))
    });

    vm.register_builtin("set_add", |args, _out| {
        let s = match args.first() {
            Some(VmValue::Set(s)) => s.clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "set_add: first argument must be a set",
                ))));
            }
        };
        let val = args.get(1).cloned().unwrap_or(VmValue::Nil);
        let mut items: Vec<VmValue> = (*s).clone();
        if !items.iter().any(|x| values_equal(x, &val)) {
            items.push(val);
        }
        Ok(VmValue::Set(Rc::new(items)))
    });

    vm.register_builtin("set_remove", |args, _out| {
        let s = match args.first() {
            Some(VmValue::Set(s)) => s.clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "set_remove: first argument must be a set",
                ))));
            }
        };
        let val = args.get(1).cloned().unwrap_or(VmValue::Nil);
        let items: Vec<VmValue> = s
            .iter()
            .filter(|x| !values_equal(x, &val))
            .cloned()
            .collect();
        Ok(VmValue::Set(Rc::new(items)))
    });

    vm.register_builtin("set_contains", |args, _out| {
        let s = match args.first() {
            Some(VmValue::Set(s)) => s,
            _ => return Ok(VmValue::Bool(false)),
        };
        let val = args.get(1).unwrap_or(&VmValue::Nil);
        Ok(VmValue::Bool(s.iter().any(|x| values_equal(x, val))))
    });

    vm.register_builtin("set_union", |args, _out| {
        let a = match args.first() {
            Some(VmValue::Set(s)) => s,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "set_union: arguments must be sets",
                ))));
            }
        };
        let b = match args.get(1) {
            Some(VmValue::Set(s)) => s,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "set_union: arguments must be sets",
                ))));
            }
        };
        let mut items: Vec<VmValue> = (**a).clone();
        for v in b.iter() {
            if !items.iter().any(|x| values_equal(x, v)) {
                items.push(v.clone());
            }
        }
        Ok(VmValue::Set(Rc::new(items)))
    });

    vm.register_builtin("set_intersect", |args, _out| {
        let a = match args.first() {
            Some(VmValue::Set(s)) => s,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "set_intersect: arguments must be sets",
                ))));
            }
        };
        let b = match args.get(1) {
            Some(VmValue::Set(s)) => s,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "set_intersect: arguments must be sets",
                ))));
            }
        };
        let items: Vec<VmValue> = a
            .iter()
            .filter(|x| b.iter().any(|y| values_equal(x, y)))
            .cloned()
            .collect();
        Ok(VmValue::Set(Rc::new(items)))
    });

    vm.register_builtin("set_difference", |args, _out| {
        let a = match args.first() {
            Some(VmValue::Set(s)) => s,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "set_difference: arguments must be sets",
                ))));
            }
        };
        let b = match args.get(1) {
            Some(VmValue::Set(s)) => s,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "set_difference: arguments must be sets",
                ))));
            }
        };
        let items: Vec<VmValue> = a
            .iter()
            .filter(|x| !b.iter().any(|y| values_equal(x, y)))
            .cloned()
            .collect();
        Ok(VmValue::Set(Rc::new(items)))
    });

    vm.register_builtin("set_symmetric_difference", |args, _out| {
        let a = match args.first() {
            Some(VmValue::Set(s)) => s,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "set_symmetric_difference: arguments must be sets",
                ))));
            }
        };
        let b = match args.get(1) {
            Some(VmValue::Set(s)) => s,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "set_symmetric_difference: arguments must be sets",
                ))));
            }
        };
        let mut items: Vec<VmValue> = a
            .iter()
            .filter(|x| !b.iter().any(|y| values_equal(x, y)))
            .cloned()
            .collect();
        for v in b.iter() {
            if !a.iter().any(|x| values_equal(x, v)) {
                items.push(v.clone());
            }
        }
        Ok(VmValue::Set(Rc::new(items)))
    });

    vm.register_builtin("set_is_subset", |args, _out| {
        let a = match args.first() {
            Some(VmValue::Set(s)) => s,
            _ => return Ok(VmValue::Bool(false)),
        };
        let b = match args.get(1) {
            Some(VmValue::Set(s)) => s,
            _ => return Ok(VmValue::Bool(false)),
        };
        Ok(VmValue::Bool(
            a.iter().all(|x| b.iter().any(|y| values_equal(x, y))),
        ))
    });

    vm.register_builtin("set_is_superset", |args, _out| {
        let a = match args.first() {
            Some(VmValue::Set(s)) => s,
            _ => return Ok(VmValue::Bool(false)),
        };
        let b = match args.get(1) {
            Some(VmValue::Set(s)) => s,
            _ => return Ok(VmValue::Bool(false)),
        };
        Ok(VmValue::Bool(
            b.iter().all(|x| a.iter().any(|y| values_equal(x, y))),
        ))
    });

    vm.register_builtin("set_is_disjoint", |args, _out| {
        let a = match args.first() {
            Some(VmValue::Set(s)) => s,
            _ => return Ok(VmValue::Bool(true)),
        };
        let b = match args.get(1) {
            Some(VmValue::Set(s)) => s,
            _ => return Ok(VmValue::Bool(true)),
        };
        Ok(VmValue::Bool(
            !a.iter().any(|x| b.iter().any(|y| values_equal(x, y))),
        ))
    });
}
