use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmSet, VmValue};
use crate::vm::Vm;

pub(crate) fn register_set_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

/// Extract the backing [`VmSet`] from a set argument, throwing `message` when
/// the argument is not a set.
fn expect_set<'a>(arg: Option<&'a VmValue>, message: &str) -> Result<&'a VmSet, VmError> {
    match arg {
        Some(VmValue::Set(set)) => Ok(set),
        _ => Err(VmError::Thrown(VmValue::string(message))),
    }
}

#[harn_builtin(sig = "set(...args: any) -> any", category = "sets")]
fn set_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let mut set = VmSet::new();
    for arg in args {
        match arg {
            VmValue::List(list) => {
                for value in list.iter() {
                    set.insert(value.clone());
                }
            }
            VmValue::Set(other) => {
                for value in other.iter() {
                    set.insert(value.clone());
                }
            }
            other => {
                set.insert(other.clone());
            }
        }
    }
    Ok(VmValue::set_value(set))
}

#[harn_builtin(sig = "set_add(s: any, value: any) -> any", category = "sets")]
fn set_add_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let mut set = expect_set(args.first(), "set_add: first argument must be a set")?.clone();
    set.insert(args.get(1).cloned().unwrap_or(VmValue::Nil));
    Ok(VmValue::set_value(set))
}

#[harn_builtin(sig = "set_remove(s: any, value: any) -> any", category = "sets")]
fn set_remove_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let mut set = expect_set(args.first(), "set_remove: first argument must be a set")?.clone();
    set.remove(args.get(1).unwrap_or(&VmValue::Nil));
    Ok(VmValue::set_value(set))
}

#[harn_builtin(sig = "set_contains(s: any, value: any) -> bool", category = "sets")]
fn set_contains_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let set = match args.first() {
        Some(VmValue::Set(set)) => set,
        _ => return Ok(VmValue::Bool(false)),
    };
    Ok(VmValue::Bool(
        set.contains(args.get(1).unwrap_or(&VmValue::Nil)),
    ))
}

#[harn_builtin(sig = "set_union(a: any, b: any) -> any", category = "sets")]
fn set_union_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let a = expect_set(args.first(), "set_union: arguments must be sets")?;
    let b = expect_set(args.get(1), "set_union: arguments must be sets")?;
    Ok(VmValue::set_value(a.union(b)))
}

#[harn_builtin(sig = "set_intersect(a: any, b: any) -> any", category = "sets")]
fn set_intersect_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let a = expect_set(args.first(), "set_intersect: arguments must be sets")?;
    let b = expect_set(args.get(1), "set_intersect: arguments must be sets")?;
    Ok(VmValue::set_value(a.intersect(b)))
}

#[harn_builtin(sig = "set_difference(a: any, b: any) -> any", category = "sets")]
fn set_difference_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let a = expect_set(args.first(), "set_difference: arguments must be sets")?;
    let b = expect_set(args.get(1), "set_difference: arguments must be sets")?;
    Ok(VmValue::set_value(a.difference(b)))
}

#[harn_builtin(
    sig = "set_symmetric_difference(a: any, b: any) -> any",
    category = "sets"
)]
fn set_symmetric_difference_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let a = expect_set(
        args.first(),
        "set_symmetric_difference: arguments must be sets",
    )?;
    let b = expect_set(
        args.get(1),
        "set_symmetric_difference: arguments must be sets",
    )?;
    Ok(VmValue::set_value(a.symmetric_difference(b)))
}

#[harn_builtin(sig = "set_is_subset(a: any, b: any) -> bool", category = "sets")]
fn set_is_subset_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let (Some(VmValue::Set(a)), Some(VmValue::Set(b))) = (args.first(), args.get(1)) else {
        return Ok(VmValue::Bool(false));
    };
    Ok(VmValue::Bool(a.is_subset(b)))
}

#[harn_builtin(sig = "set_is_superset(a: any, b: any) -> bool", category = "sets")]
fn set_is_superset_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let (Some(VmValue::Set(a)), Some(VmValue::Set(b))) = (args.first(), args.get(1)) else {
        return Ok(VmValue::Bool(false));
    };
    Ok(VmValue::Bool(a.is_superset(b)))
}

#[harn_builtin(sig = "set_is_disjoint(a: any, b: any) -> bool", category = "sets")]
fn set_is_disjoint_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let (Some(VmValue::Set(a)), Some(VmValue::Set(b))) = (args.first(), args.get(1)) else {
        return Ok(VmValue::Bool(true));
    };
    Ok(VmValue::Bool(a.is_disjoint(b)))
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
