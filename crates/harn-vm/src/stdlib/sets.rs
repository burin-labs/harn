use std::collections::HashSet;
use std::rc::Rc;

use crate::value::{value_structural_hash_key, VmError, VmValue};
use crate::vm::Vm;

/// Build a HashSet of structural hash keys for O(1) membership checks.
fn hash_set_from_items(items: &[VmValue]) -> HashSet<String> {
    items.iter().map(value_structural_hash_key).collect()
}

/// Deduplicated insert: adds `v` to `items` only if its hash key is new.
fn dedup_insert(items: &mut Vec<VmValue>, seen: &mut HashSet<String>, v: &VmValue) {
    let key = value_structural_hash_key(v);
    if seen.insert(key) {
        items.push(v.clone());
    }
}

pub(crate) fn register_set_builtins(vm: &mut Vm) {
    vm.register_builtin("set", |args, _out| {
        let mut items: Vec<VmValue> = Vec::new();
        let mut seen = HashSet::new();
        for arg in args {
            match arg {
                VmValue::List(list) => {
                    for v in list.iter() {
                        dedup_insert(&mut items, &mut seen, v);
                    }
                }
                VmValue::Set(s) => {
                    for v in s.iter() {
                        dedup_insert(&mut items, &mut seen, v);
                    }
                }
                other => {
                    dedup_insert(&mut items, &mut seen, other);
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
        let val_key = value_structural_hash_key(&val);
        let mut seen = hash_set_from_items(&s);
        let mut items: Vec<VmValue> = (*s).clone();
        if seen.insert(val_key) {
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
        let val_key = value_structural_hash_key(&val);
        let items: Vec<VmValue> = s
            .iter()
            .filter(|x| value_structural_hash_key(x) != val_key)
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
        let val_key = value_structural_hash_key(val);
        let keys = hash_set_from_items(s);
        Ok(VmValue::Bool(keys.contains(&val_key)))
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
        let mut seen = hash_set_from_items(a);
        for v in b.iter() {
            dedup_insert(&mut items, &mut seen, v);
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
        let b_keys = hash_set_from_items(b);
        let items: Vec<VmValue> = a
            .iter()
            .filter(|x| b_keys.contains(&value_structural_hash_key(x)))
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
        let b_keys = hash_set_from_items(b);
        let items: Vec<VmValue> = a
            .iter()
            .filter(|x| !b_keys.contains(&value_structural_hash_key(x)))
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
        let a_keys = hash_set_from_items(a);
        let b_keys = hash_set_from_items(b);
        let mut items: Vec<VmValue> = a
            .iter()
            .filter(|x| !b_keys.contains(&value_structural_hash_key(x)))
            .cloned()
            .collect();
        for v in b.iter() {
            if !a_keys.contains(&value_structural_hash_key(v)) {
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
        let b_keys = hash_set_from_items(b);
        Ok(VmValue::Bool(
            a.iter()
                .all(|x| b_keys.contains(&value_structural_hash_key(x))),
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
        let a_keys = hash_set_from_items(a);
        Ok(VmValue::Bool(
            b.iter()
                .all(|x| a_keys.contains(&value_structural_hash_key(x))),
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
        let b_keys = hash_set_from_items(b);
        Ok(VmValue::Bool(
            !a.iter()
                .any(|x| b_keys.contains(&value_structural_hash_key(x))),
        ))
    });
}
