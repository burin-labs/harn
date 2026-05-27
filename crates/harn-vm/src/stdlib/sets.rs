use std::collections::HashSet;
use std::rc::Rc;

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
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
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

#[harn_builtin(sig = "set(...args: any) -> any", category = "sets")]
fn set_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
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
}

#[harn_builtin(sig = "set_add(s: any, value: any) -> any", category = "sets")]
fn set_add_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
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
}

#[harn_builtin(sig = "set_remove(s: any, value: any) -> any", category = "sets")]
fn set_remove_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
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
}

#[harn_builtin(sig = "set_contains(s: any, value: any) -> bool", category = "sets")]
fn set_contains_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let s = match args.first() {
        Some(VmValue::Set(s)) => s,
        _ => return Ok(VmValue::Bool(false)),
    };
    let val = args.get(1).unwrap_or(&VmValue::Nil);
    let val_key = value_structural_hash_key(val);
    let keys = hash_set_from_items(s);
    Ok(VmValue::Bool(keys.contains(&val_key)))
}

#[harn_builtin(sig = "set_union(a: any, b: any) -> any", category = "sets")]
fn set_union_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
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
}

#[harn_builtin(sig = "set_intersect(a: any, b: any) -> any", category = "sets")]
fn set_intersect_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
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
}

#[harn_builtin(sig = "set_difference(a: any, b: any) -> any", category = "sets")]
fn set_difference_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
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
}

#[harn_builtin(
    sig = "set_symmetric_difference(a: any, b: any) -> any",
    category = "sets"
)]
fn set_symmetric_difference_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
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
}

#[harn_builtin(sig = "set_is_subset(a: any, b: any) -> bool", category = "sets")]
fn set_is_subset_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
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
}

#[harn_builtin(sig = "set_is_superset(a: any, b: any) -> bool", category = "sets")]
fn set_is_superset_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
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
}

#[harn_builtin(sig = "set_is_disjoint(a: any, b: any) -> bool", category = "sets")]
fn set_is_disjoint_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
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
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &SET_IMPL_DEF,
    &SET_ADD_IMPL_DEF,
    &SET_REMOVE_IMPL_DEF,
    &SET_CONTAINS_IMPL_DEF,
    &SET_UNION_IMPL_DEF,
    &SET_INTERSECT_IMPL_DEF,
    &SET_DIFFERENCE_IMPL_DEF,
    &SET_SYMMETRIC_DIFFERENCE_IMPL_DEF,
    &SET_IS_SUBSET_IMPL_DEF,
    &SET_IS_SUPERSET_IMPL_DEF,
    &SET_IS_DISJOINT_IMPL_DEF,
];
